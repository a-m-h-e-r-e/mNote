//! [`Vim`]: the mode state machine. It reads keys and applies them to the
//! note's [`Buffer`]: moving the cursor, editing through operators, keeping
//! registers and marks, and grouping each command into one undo step.

use std::collections::HashMap;
use std::ops::Range;

use mnote_core::Buffer;
use mnote_core::text::Origin;
use ropey::Rope;

use crate::Mode;
use crate::ex::cmdline::CommandKind;
use crate::keys::Key;
use crate::motion::{Find, Motion, MotionKind, Target, repeat_find};
use crate::registers::{Clipboard, Register, Registers, is_register_name};
use crate::text::{
    clamp_normal, column, first_non_blank, last_line, line_end, line_len, line_of, line_start,
};
use crate::textobj;

/// Whether the key was a Vim key. Unhandled keys (such as the `space` leader
/// or `:` text in command mode) are for the app to deal with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Handled,
    Unhandled(Key),
}

/// An operator waiting for a motion or text object: `d`, `c`, `y`, `>`, `<`,
/// `gu`, `gU`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Delete,
    Change,
    Yank,
    Indent,
    Outdent,
    Lower,
    Upper,
    /// `g~`: swaps upper and lower case.
    Toggle,
    /// `=`: indents lines as their language's syntax says.
    Reindent,
    /// `gc`: comments lines in or out, with the language's comment token.
    Comment,
    /// `gb`: wraps text in the language's block comment, or unwraps it.
    BlockComment,
    /// One added with [`crate::ext::register_operator`], by id.
    Func(u16),
}

impl Operator {
    fn from_key(c: char) -> Option<Self> {
        Some(match c {
            'd' => Operator::Delete,
            'c' => Operator::Change,
            'y' => Operator::Yank,
            '>' => Operator::Indent,
            '<' => Operator::Outdent,
            '=' => Operator::Reindent,
            _ => return None,
        })
    }

    /// The key that, typed again, applies the operator to whole lines
    /// (`dd`, `yy`, `>>`, `guu`, `gUU`).
    fn line_key(self) -> char {
        match self {
            Operator::Delete => 'd',
            Operator::Change => 'c',
            Operator::Yank => 'y',
            Operator::Indent => '>',
            Operator::Outdent => '<',
            Operator::Lower => 'u',
            Operator::Upper => 'U',
            Operator::Toggle => '~',
            Operator::Reindent => '=',
            // `gcc` and `gbc`, as Comment.nvim.
            Operator::Comment | Operator::BlockComment => 'c',
            Operator::Func(id) => crate::ext::operator_line_key(id),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Pending {
    #[default]
    None,
    G,
    Bracket {
        forward: bool,
    },
    Find {
        forward: bool,
        till: bool,
    },
    Object {
        around: bool,
    },
    Register,
    SetMark,
    /// `q`, waiting for `:`, `/` or `?` (the command-line window).
    Q,
    JumpMark {
        exact: bool,
    },
    /// `r`, waiting for the character.
    Replace,
    /// `@`, waiting for the register.
    At,
    /// `z`, waiting for `t` `z` `b` (and `⏎` `.` `-`).
    Z,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct OpPending {
    op: Operator,
    count: Option<usize>,
}

/// An insert-mode session, for `3ix<esc>` style repeats.
#[derive(Debug, Clone, Default)]
pub(crate) struct InsertSession {
    count: usize,
    /// What each repeat inserts before the typed text (`\n` after `o`).
    prefix: String,
    pub(crate) typed: String,
    /// The typed text can't be repeated reliably (backspaced past it).
    pub(crate) broken: bool,
    /// Where typing started, for `⌃u`.
    pub(crate) start: usize,
}

/// A REPLACE-mode session (`R`): what each typed character overwrote, for
/// Backspace, and the typed text, for a count.
#[derive(Debug, Clone, Default)]
struct ReplaceSession {
    count: usize,
    typed: String,
    /// Per typed character: the one it overwrote, or `None` if it was added
    /// (at the end of the line, or a line break).
    overwritten: Vec<Option<char>>,
}

/// What a mapping runs: keys, or a command line (`<Cmd>daily<CR>`, as in
/// Neovim), which runs without being typed into the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapRhs {
    Keys(Vec<Key>),
    Command(String),
}

impl MapRhs {
    /// From the right-hand side of `:map`: `<Cmd>…<CR>` is a command.
    pub fn parse(rhs: &str) -> Option<Self> {
        let rhs = rhs.trim();
        let lower = rhs.to_ascii_lowercase();
        if lower.starts_with("<cmd>") && lower.ends_with("<cr>") {
            let line = rhs[5..rhs.len() - 4].trim();
            return (!line.is_empty()).then(|| MapRhs::Command(line.to_owned()));
        }
        let keys = crate::parse_keys(rhs);
        (!keys.is_empty()).then_some(MapRhs::Keys(keys))
    }

    /// As `:map` lists it.
    pub fn notation(&self) -> String {
        match self {
            MapRhs::Keys(keys) => crate::key_notation(keys),
            MapRhs::Command(line) => format!("<Cmd>{line}<CR>"),
        }
    }
}

/// What `.` repeats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LastChange {
    /// The keys of a change, replayed (a count replaces theirs).
    Keys(Vec<Key>),
    /// A command line that changed the text (a `<Cmd>` mapping's, or one an
    /// extension asked for with [`Vim::repeat_with`]).
    Ex(String),
}

/// Mappings for one mode: keys typed, what they run.
pub(crate) type MapTable = Vec<(Vec<Key>, MapRhs)>;

/// Where long lines wrap on screen, supplied by the app for `gj` / `gk`.
pub trait ScreenRows {
    /// The characters of line `line` (from 0 in the line) where each screen
    /// row after the first starts; empty when the line fits on one row.
    fn row_starts(&self, line: usize) -> Vec<usize>;
}

/// Mappings: `:map` and friends, and keys waiting to become one.
pub(crate) struct Mappings {
    /// Insert-mode mappings (`:imap`): keys typed, keys they become. `jk`
    /// to `<Esc>` is there by default.
    pub imaps: MapTable,
    /// NORMAL and VISUAL mappings (`:nmap`, `:vmap`), non-recursive.
    pub nmaps: MapTable,
    pub vmaps: MapTable,
    /// Keys typed in NORMAL or VISUAL that may yet become a mapping.
    pub map_pending: Vec<Key>,
    /// Keys typed in insert mode that start a mapping; they're already in
    /// the text and come out again if the mapping completes.
    pub imap_pending: Vec<Key>,
    /// Feeding a mapping's keys: they aren't mapped again.
    pub in_mapping: bool,
}

impl Default for Mappings {
    fn default() -> Self {
        Mappings {
            imaps: vec![(
                vec![Key::Char('j'), Key::Char('k')],
                MapRhs::Keys(vec![Key::Esc]),
            )],
            nmaps: Vec::new(),
            vmaps: Vec::new(),
            map_pending: Vec::new(),
            imap_pending: Vec::new(),
            in_mapping: false,
        }
    }
}

/// The last search and substitute, for `n`, highlights and `:&`.
pub(crate) struct SearchState {
    /// The last search pattern (`/`, `?`, `:s`, `:g`), for `n` and `:s//`.
    pub last_search: Option<String>,
    /// Whether matches of the last search are highlighted (`:noh` clears).
    pub search_highlight: bool,
    /// The last `:s` (pattern, replacement, flags), for `:s` and `:&`.
    pub last_substitute: Option<(String, String, String)>,
    /// The last search compiled, for `n`, `N` and highlighting.
    pub search_pattern: Option<crate::ex::pattern::Pattern>,
    /// Direction of the last `/` (true) or `?` search.
    pub search_forward: bool,
}

impl Default for SearchState {
    fn default() -> Self {
        SearchState {
            last_search: None,
            search_highlight: false,
            last_substitute: None,
            search_pattern: None,
            search_forward: true,
        }
    }
}

/// The command line: what's being typed, its preview, the last message, histories, completion and the command-line window.
#[derive(Default)]
pub(crate) struct CmdlineState {
    /// The line being typed in command mode, and its live preview.
    pub cmdline: Option<crate::ex::cmdline::CommandLine>,
    pub preview: Option<crate::ex::cmdline::Preview>,
    /// The last command's message, until the next key.
    pub message: Option<crate::ex::cmdline::Message>,
    /// An app command from the last command line, until the app takes it.
    pub app_command: Option<crate::ex::AppCommand>,
    /// Lines run from the command line, oldest first.
    pub ex_history: Vec<String>,
    pub search_history: Vec<String>,
    pub completer: Option<Box<dyn crate::ex::cmdline::Completer>>,
    pub cmdwin: Option<Box<crate::ex::cmdwin::CmdWindow>>,
}

/// Vim's state for one editor or text input.
pub struct Vim {
    pub(crate) mode: Mode,
    pub(crate) cursor: usize,
    pub(crate) anchor: usize,
    pub(crate) want_col: Option<usize>,
    count: Option<usize>,
    pending: Pending,
    pub(crate) op: Option<OpPending>,
    pub(crate) register: Option<char>,
    last_find: Option<Find>,
    pub(crate) registers: Registers,
    pub(crate) marks: HashMap<char, usize>,
    pub(crate) options: crate::ex::Options,
    /// Line anchors `:g` and `:normal` walk; edits keep them on their lines,
    /// and a deleted line's anchor becomes `None`.
    pub(crate) anchors: crate::anchors::Anchors,
    /// The lines on screen and the page size (the app sets them).
    pub(crate) view: crate::view::View,
    pub(crate) jumps: crate::view::Jumps,
    /// Where long lines wrap on screen, for `gj` / `gk` (the app supplies it).
    screen_rows: Option<Box<dyn ScreenRows>>,
    /// Plain `j` / `k` move by screen row (config.toml's `wrap_motion`).
    wrap_motion: bool,
    pub(crate) insert: Option<InsertSession>,
    replace: Option<ReplaceSession>,
    /// `⌃r` typed in INSERT: the next key names a register.
    pub(crate) insert_register: bool,
    /// INSERT set aside by `⌃o` while one NORMAL command runs.
    pub(crate) paused: Option<crate::insert_keys::Paused>,
    pub(crate) macros: crate::macros::Macros,
    pub(crate) block: crate::block::BlockState,
    /// Who the edits made now come from (the user, unless a plugin is
    /// running), recorded in the buffer's change log.
    origin: Origin,
    /// Keys of the command being typed, and of the last change, for `.`.
    keys: Vec<Key>,
    last_change: Option<LastChange>,
    /// The command being typed went through VISUAL (its change isn't
    /// repeated, as `.` after a visual change would need the selection).
    visual_in_command: bool,
    /// Buffer revision when the current command started.
    command_revision: u64,
    pub(crate) maps: Mappings,
    pub(crate) search: SearchState,
    pub(crate) cmd: CmdlineState,
}

impl std::fmt::Debug for Vim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vim")
            .field("mode", &self.mode)
            .field("cursor", &self.cursor)
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

impl Default for Vim {
    fn default() -> Self {
        Vim {
            mode: Mode::Normal,
            cursor: 0,
            anchor: 0,
            want_col: None,
            count: None,
            pending: Pending::None,
            op: None,
            register: None,
            last_find: None,
            registers: Registers::default(),
            marks: HashMap::new(),
            options: crate::ex::Options::default(),
            anchors: crate::anchors::Anchors::default(),
            view: crate::view::View::default(),
            jumps: crate::view::Jumps::default(),
            screen_rows: None,
            wrap_motion: false,
            insert: None,
            replace: None,
            insert_register: false,
            paused: None,
            macros: crate::macros::Macros::default(),
            block: crate::block::BlockState::default(),
            origin: Origin::User,
            keys: Vec::new(),
            last_change: None,
            visual_in_command: false,
            command_revision: 0,
            maps: Mappings::default(),
            search: SearchState::default(),
            cmd: CmdlineState::default(),
        }
    }
}

impl Vim {
    pub fn new() -> Self {
        Self::default()
    }

    /// A Vim state for a text input (D-001): starts in insert mode.
    pub fn for_input() -> Self {
        Vim {
            mode: Mode::Insert,
            insert: Some(InsertSession {
                count: 1,
                ..InsertSession::default()
            }),
            ..Self::default()
        }
    }

    /// Plugs in the system clipboard for the `+` and `*` registers.
    /// Shares the unnamed register with the system clipboard (`y`, `d`
    /// and `p` use it), or keeps them apart (only `"+` reaches it).
    pub fn share_clipboard(&mut self, shared: bool) {
        self.registers.share_clipboard(shared);
    }

    /// `⌘V`: types `text` where the cursor is: into the note in INSERT
    /// (one undo step with the rest of the insert), or into the command
    /// line (line breaks become spaces). Returns whether it went anywhere.
    pub fn paste_text(&mut self, text: &str, buf: &mut Buffer) -> bool {
        match self.mode {
            Mode::Insert => {
                self.typed(buf, text);
                if let Some(session) = &mut self.insert {
                    // Pasted text isn't repeated by a count or `.`.
                    session.broken = true;
                }
                true
            }
            Mode::Command => self.paste_into_command_line(text, buf),
            _ => false,
        }
    }

    pub fn set_clipboard(&mut self, clipboard: Box<dyn Clipboard>) {
        self.registers.set_clipboard(clipboard);
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Where a visual selection started.
    pub fn anchor(&self) -> usize {
        self.anchor
    }

    /// The selected range in visual mode (`end` exclusive; the character
    /// under the cursor is selected, as in Vim).
    pub fn selection(&self) -> Option<Range<usize>> {
        match self.mode {
            Mode::Visual => {
                let (a, b) = (self.anchor.min(self.cursor), self.anchor.max(self.cursor));
                Some(a..b + 1)
            }
            _ => None,
        }
    }

    /// Sets who the edits made from now on come from; returns who it was,
    /// so a caller can put it back (a plugin's call, then the user again).
    pub fn set_origin(&mut self, origin: Origin) -> Origin {
        std::mem::replace(&mut self.origin, origin)
    }

    /// Moves the cursor, for example after a mouse click.
    /// Replaces `range` with `text` as one undo step, keeping marks in
    /// place (for edits the app makes, such as toggling a task).
    pub fn replace(&mut self, buf: &mut Buffer, range: Range<usize>, text: &str) {
        buf.begin_change(self.cursor);
        self.edit(buf, range, text);
        buf.end_change(self.cursor);
    }

    /// Replaces `range` with `text`, keeping marks in place, as part of the
    /// change being made (for extension operators and commands; the change
    /// around it is the undo step).
    pub fn edit_text(&mut self, buf: &mut Buffer, range: Range<usize>, text: &str) {
        self.edit(buf, range, text);
    }

    pub fn set_cursor(&mut self, pos: usize) {
        self.cursor = pos;
        self.want_col = None;
    }

    /// Whether a key sequence is half typed (shown in the status line).
    pub fn is_pending(&self) -> bool {
        self.pending != Pending::None
            || !self.maps.map_pending.is_empty()
            || self.count.is_some()
            || self.op.is_some()
            || self.register.is_some()
    }

    /// The register picked with `"x` for the next command, if any.
    pub fn pending_register(&self) -> Option<char> {
        self.register
    }

    /// The contents of a register, as `p` would paste it.
    pub fn register(&mut self, name: char) -> Option<Register> {
        self.registers.get(Some(name))
    }

    /// A mark's position (`a` to `z`, `<`, `>`, and `'` for the last jump).
    pub fn mark(&self, name: char) -> Option<usize> {
        self.marks.get(&name).copied()
    }

    /// Handles one key, editing `buf` as needed.
    pub fn handle_key(&mut self, key: Key, buf: &mut Buffer) -> Effect {
        self.macros.depth += 1;
        self.record_key(key);
        let effect = self.dispatch_key(key, buf);
        self.macros.depth -= 1;
        effect
    }

    fn dispatch_key(&mut self, key: Key, buf: &mut Buffer) -> Effect {
        if !matches!(self.mode, Mode::Insert | Mode::Replace) && self.is_idle() {
            self.keys.clear();
            self.visual_in_command = self.mode.is_visual();
            self.command_revision = buf.revision();
        }
        self.keys.push(key);
        self.cmd.message = None;
        if self.cmd.cmdwin.is_some() {
            self.cmd_window_key(key, buf);
            return Effect::Handled;
        }
        let was_insert = self.mode == Mode::Insert;
        let effect = match self.mode {
            Mode::Insert => self.insert_mapped(key, buf),
            Mode::Replace => self.replace_key(key, buf),
            Mode::Command => {
                self.command_key(key, buf);
                Effect::Handled
            }
            Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::VisualBlock => {
                self.normal_mapped(key, buf)
            }
        };
        if self.paused.is_some() && !was_insert {
            self.resume_insert(buf);
        }
        effect
    }

    /// Starts the keys `.` repeats afresh, as if `keys` had just been typed.
    pub(crate) fn restart_keys(&mut self, keys: Vec<Key>, revision: u64) {
        self.keys = keys;
        self.command_revision = revision;
    }

    pub(crate) fn is_idle(&self) -> bool {
        !self.is_pending()
    }

    // ---- insert mode ------------------------------------------------------

    /// A NORMAL or VISUAL key, through the `:nmap` / `:vmap` mappings.
    /// Keys that may start a mapping wait (see [`Vim::map_pending`]); a
    /// full match runs the mapping's keys, and keys that turn out not to
    /// be one run as typed.
    fn normal_mapped(&mut self, key: Key, buf: &mut Buffer) -> Effect {
        let visual = self.mode.is_visual();
        let mappable = !self.maps.in_mapping
            && self.op.is_none()
            && self.pending == Pending::None
            && self.register.is_none();
        let table = if visual {
            &self.maps.vmaps
        } else {
            &self.maps.nmaps
        };
        if self.maps.map_pending.is_empty() && (!mappable || table.is_empty()) {
            return self.normal_key(key, buf);
        }
        let mut typed = std::mem::take(&mut self.maps.map_pending);
        typed.push(key);
        if let Some(rhs) = table
            .iter()
            .find(|(lhs, _)| *lhs == typed)
            .map(|(_, rhs)| rhs.clone())
        {
            return self.run_mapping(&rhs, buf);
        }
        if table
            .iter()
            .any(|(lhs, _)| lhs.len() > typed.len() && lhs.starts_with(&typed))
        {
            self.maps.map_pending = typed;
            return Effect::Handled;
        }
        self.run_unmapped(typed, buf)
    }

    /// Keys that didn't become a mapping: the first runs as typed, the
    /// rest may still start one.
    fn run_unmapped(&mut self, mut typed: Vec<Key>, buf: &mut Buffer) -> Effect {
        if typed.is_empty() {
            return Effect::Handled;
        }
        let first = typed.remove(0);
        let mut effect = self.normal_key(first, buf);
        for key in typed {
            effect = self.normal_mapped(key, buf);
        }
        effect
    }

    /// A mapping's keys, run without mapping them again, or its command.
    fn run_mapping(&mut self, rhs: &MapRhs, buf: &mut Buffer) -> Effect {
        let keys = match rhs {
            MapRhs::Keys(keys) => keys,
            MapRhs::Command(line) => {
                self.run_mapped_command(line, buf);
                return Effect::Handled;
            }
        };
        self.maps.in_mapping = true;
        let mut effect = Effect::Handled;
        for key in keys {
            effect = self.handle_key(*key, buf);
        }
        self.maps.in_mapping = false;
        effect
    }

    /// A mapping's command line: runs as `:` would (its message shows, an
    /// app command goes to the app) without going into the history. Like
    /// any command line it ends in NORMAL.
    pub(crate) fn run_mapped_command(&mut self, line: &str, buf: &mut Buffer) {
        self.cancel();
        let revision = buf.revision();
        let result = self.execute(line, buf).map(|outcome| {
            self.cmd.app_command = outcome.app;
            outcome.message
        });
        self.cmd.message = match result {
            Ok(Some(text)) => Some(crate::ex::cmdline::Message {
                text,
                error: false,
                line: None,
            }),
            Ok(None) => None,
            Err(err) => Some(crate::ex::cmdline::Message {
                text: err.to_string(),
                error: true,
                line: None,
            }),
        };
        // A command that changed the text is what `.` repeats.
        if buf.revision() != revision {
            self.last_change = Some(LastChange::Ex(line.to_owned()));
        }
        self.keys.clear();
        self.command_revision = buf.revision();
    }

    /// Whether NORMAL or VISUAL keys wait to see if they become a mapping;
    /// the app gives up on them after a second ([`Vim::flush_mapping`]).
    pub fn map_pending(&self) -> bool {
        !self.maps.map_pending.is_empty()
    }

    /// Runs the waiting keys as typed (the wait for a mapping timed out).
    pub fn flush_mapping(&mut self, buf: &mut Buffer) -> Effect {
        let typed = std::mem::take(&mut self.maps.map_pending);
        if typed.is_empty() {
            return Effect::Handled;
        }
        // The first can't start a longer mapping any more; the rest may.
        self.run_unmapped(typed, buf)
    }

    /// Forgets every mapping (before config.toml's are applied).
    pub fn clear_mappings(&mut self) {
        self.maps.imaps.clear();
        self.maps.nmaps.clear();
        self.maps.vmaps.clear();
        self.maps.map_pending.clear();
    }

    /// An insert-mode key, through the `:imap` mappings. A key that may
    /// start a mapping is typed as usual; when the mapping completes, those
    /// keys come out again and the mapping's keys run instead (so `jk`
    /// leaves insert mode with no `j` left behind).
    fn insert_mapped(&mut self, key: Key, buf: &mut Buffer) -> Effect {
        if self.maps.in_mapping || self.maps.imaps.is_empty() {
            return self.insert_key(key, buf);
        }
        let mut typed = std::mem::take(&mut self.maps.imap_pending);
        typed.push(key);
        if let Some(rhs) = self
            .maps
            .imaps
            .iter()
            .find(|(lhs, _)| *lhs == typed)
            .map(|(_, rhs)| rhs.clone())
        {
            for _ in 1..typed.len() {
                self.insert_key(Key::Backspace, buf);
            }
            return self.run_mapping(&rhs, buf);
        }
        let prefix = |keys: &[Key]| {
            self.maps
                .imaps
                .iter()
                .any(|(lhs, _)| lhs.len() > keys.len() && lhs.starts_with(keys))
        };
        if matches!(key, Key::Char(_)) && prefix(&typed) {
            self.maps.imap_pending = typed;
            return self.insert_key(key, buf);
        }
        if typed.len() > 1 {
            // The earlier keys didn't become a mapping; this one may start one.
            return self.insert_mapped(key, buf);
        }
        self.insert_key(key, buf)
    }

    fn insert_key(&mut self, key: Key, buf: &mut Buffer) -> Effect {
        if std::mem::take(&mut self.insert_register) {
            if let Key::Char(c) = key {
                self.insert_register(c, buf);
            }
            return Effect::Handled;
        }
        match key {
            Key::Esc => {
                let session = self.insert.take();
                if let Some(session) = &session
                    && session.count > 1
                    && !session.broken
                {
                    let unit = format!("{}{}", session.prefix, session.typed);
                    let repeat = unit.repeat(session.count - 1);
                    self.insert_text(buf, &repeat);
                }
                // `I` / `A` / `c` on a block: the same typing on its other lines.
                let typed = session.filter(|s| !s.broken).map(|s| s.typed);
                self.finish_block_insert(typed.as_deref(), buf);
                // Where INSERT ended, for `gi`.
                self.marks.insert('^', self.cursor);
                let start = line_start(buf.text(), line_of(buf.text(), self.cursor));
                if self.cursor > start {
                    self.cursor -= 1;
                }
                buf.end_change(self.cursor);
                self.enter(Mode::Normal, buf.text());
                self.done(buf, true);
            }
            Key::Char(c) => {
                self.typed(buf, &c.to_string());
                self.reindent_after_typing(c, buf);
            }
            Key::Tab => self.typed(buf, "\t"),
            Key::Enter => {
                let line = line_of(buf.text(), self.cursor);
                let fallback = indent_of(buf.text(), line);
                self.insert_text(buf, "\n");
                let indent = self.syntax_indent(buf, line + 1).unwrap_or(fallback);
                self.insert_text(buf, &indent);
                if let Some(s) = &mut self.insert {
                    s.typed.push('\n');
                }
            }
            Key::Backspace | Key::Ctrl('h') => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.edit(buf, self.cursor..self.cursor + 1, "");
                    if let Some(s) = &mut self.insert {
                        if s.typed.pop().is_none() {
                            s.broken = true;
                        }
                        s.start = s.start.min(self.cursor);
                    }
                }
            }
            Key::Ctrl('w') => self.delete_word_before(buf),
            Key::Ctrl('u') => self.delete_line_before(buf),
            Key::Ctrl('r') => self.insert_register = true,
            Key::Ctrl(c @ ('t' | 'd')) => self.shift_insert_line(c == 't', buf),
            Key::Ctrl('o') => self.pause_insert(buf),
            Key::Left
            | Key::Right
            | Key::Up
            | Key::Down
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown => self.insert_move(key, buf),
            Key::Ctrl(_) | Key::Alt(_) | Key::BackTab => {
                return Effect::Unhandled(key);
            }
        }
        Effect::Handled
    }

    /// An arrow, Home/End or Page key while inserting: the cursor moves and
    /// the insert goes on as a new undo step (as in Vim, it can't be
    /// repeated with `.` or a count after that).
    fn insert_move(&mut self, key: Key, buf: &mut Buffer) {
        let text = buf.text();
        let line = line_of(text, self.cursor);
        let start = line_start(text, line);
        let column = self.cursor - start;
        let to_line = |line: usize| {
            let start = line_start(text, line);
            start + column.min(line_len(text, line))
        };
        let last = crate::text::line_count(text).saturating_sub(1);
        let to = match key {
            Key::Left => self.cursor.max(start + 1) - 1,
            Key::Right => (self.cursor + 1).min(start + line_len(text, line)),
            Key::Up => to_line(line.saturating_sub(1)),
            Key::Down => to_line((line + 1).min(last)),
            Key::PageUp => to_line(line.saturating_sub(self.page().max(1))),
            Key::PageDown => to_line((line + self.page().max(1)).min(last)),
            Key::Home => start,
            Key::End => start + line_len(text, line),
            _ => self.cursor,
        };
        if to == self.cursor {
            return;
        }
        buf.end_change(self.cursor);
        self.cursor = to;
        buf.begin_change(self.cursor);
        if let Some(session) = &mut self.insert {
            session.broken = true;
        }
    }

    /// A key in REPLACE mode (`R`): characters overwrite the text (at the
    /// end of a line they're added), Backspace puts back what was
    /// overwritten, Esc repeats the typing for a count and ends it, as one
    /// undo step.
    fn replace_key(&mut self, key: Key, buf: &mut Buffer) -> Effect {
        match key {
            Key::Esc => {
                if let Some(session) = self.replace.clone() {
                    for _ in 1..session.count {
                        for c in session.typed.chars() {
                            self.overwrite(c, buf);
                        }
                    }
                }
                self.replace = None;
                self.marks.insert('^', self.cursor);
                let start = line_start(buf.text(), line_of(buf.text(), self.cursor));
                if self.cursor > start {
                    self.cursor -= 1;
                }
                buf.end_change(self.cursor);
                self.enter(Mode::Normal, buf.text());
                self.done(buf, true);
            }
            Key::Char(c) => self.overwrite(c, buf),
            Key::Tab => self.overwrite('\t', buf),
            Key::Enter => self.overwrite('\n', buf),
            Key::Backspace | Key::Ctrl('h') => {
                let undone = self.replace.as_mut().and_then(|s| {
                    s.typed.pop();
                    s.overwritten.pop()
                });
                match undone {
                    // Nothing typed to take back: just move left.
                    None if self.cursor > 0 => self.cursor -= 1,
                    None => {}
                    Some(old) => {
                        self.cursor -= 1;
                        let at = self.cursor;
                        match old {
                            Some(c) => self.edit(buf, at..at + 1, &c.to_string()),
                            None => self.edit(buf, at..at + 1, ""),
                        }
                    }
                }
            }
            Key::Left | Key::Right | Key::Up | Key::Down | Key::Home | Key::End => {
                self.insert_move(key, buf);
                if let Some(s) = &mut self.replace {
                    s.overwritten.clear();
                    s.typed.clear();
                    s.count = 1;
                }
            }
            _ => return Effect::Unhandled(key),
        }
        Effect::Handled
    }

    /// Types `c` over the character at the cursor (a line break, or the
    /// end of a line, takes nothing).
    fn overwrite(&mut self, c: char, buf: &mut Buffer) {
        let text = buf.text();
        let under = (self.cursor < text.len_chars())
            .then(|| text.char(self.cursor))
            .filter(|&u| u != '\n' && c != '\n');
        let at = self.cursor;
        let end = if under.is_some() { at + 1 } else { at };
        self.edit(buf, at..end, &c.to_string());
        self.cursor += 1;
        if let Some(s) = &mut self.replace {
            s.typed.push(c);
            s.overwritten.push(under);
        }
    }

    /// `r{char}`: replaces `count` characters from the cursor with `c` (not
    /// past the end of the line; a line break replaces them with one), or
    /// every selected character in VISUAL.
    fn replace_chars(&mut self, c: char, buf: &mut Buffer) -> Effect {
        if self.mode == Mode::VisualBlock {
            self.block_replace(c, buf);
            return Effect::Handled;
        }
        let text = buf.text();
        if self.mode.is_visual() {
            let (range, _) = self.visual_range(text);
            let replaced: String = text
                .slice(range.clone())
                .chars()
                .map(|ch| if ch == '\n' { ch } else { c })
                .collect();
            self.leave_visual(text);
            buf.begin_change(self.cursor);
            self.edit(buf, range.clone(), &replaced);
            self.cursor = clamp_normal(buf.text(), range.start);
            buf.end_change(self.cursor);
            self.done(buf, false);
            return Effect::Handled;
        }
        let count = self.count.take().unwrap_or(1);
        let line = line_of(text, self.cursor);
        let end = line_start(text, line) + line_len(text, line);
        if self.cursor + count > end {
            self.cancel();
            return Effect::Handled;
        }
        buf.begin_change(self.cursor);
        if c == '\n' {
            self.edit(buf, self.cursor..self.cursor + count, "\n");
            self.cursor += 1;
        } else {
            let with = c.to_string().repeat(count);
            self.edit(buf, self.cursor..self.cursor + count, &with);
            self.cursor += count - 1;
        }
        buf.end_change(self.cursor);
        self.done(buf, true);
        Effect::Handled
    }

    /// `~`: swaps the case of `count` characters and moves past them.
    fn toggle_chars(&mut self, buf: &mut Buffer) -> Effect {
        let count = self.count.take().unwrap_or(1);
        let text = buf.text();
        let line = line_of(text, self.cursor);
        let end = line_start(text, line) + line_len(text, line);
        let to = (self.cursor + count).min(end);
        if to <= self.cursor {
            self.done(buf, false);
            return Effect::Handled;
        }
        let changed = toggle_case(&text.slice(self.cursor..to).to_string());
        buf.begin_change(self.cursor);
        self.edit(buf, self.cursor..to, &changed);
        self.cursor = clamp_normal(buf.text(), to);
        buf.end_change(self.cursor);
        self.done(buf, true);
        Effect::Handled
    }

    /// `⌃a` / `⌃x`: adds `delta` times the count to the number at or after
    /// the cursor on its line; the cursor ends on its last digit.
    fn add_to_number(&mut self, delta: i64, buf: &mut Buffer) -> Effect {
        let count = self.count.take().unwrap_or(1) as i64;
        let text = buf.text();
        let line = line_of(text, self.cursor);
        let start = line_start(text, line);
        let chars: Vec<char> = text
            .slice(start..start + line_len(text, line))
            .chars()
            .collect();
        let col = self.cursor - start;
        // The digits under the cursor, or the first ones after it.
        let mut first = if chars.get(col).is_some_and(char::is_ascii_digit) {
            let mut i = col;
            while i > 0 && chars[i - 1].is_ascii_digit() {
                i -= 1;
            }
            i
        } else if let Some(i) = (col..chars.len()).find(|&i| chars[i].is_ascii_digit()) {
            i
        } else {
            self.cancel();
            return Effect::Handled;
        };
        let mut last = first;
        while last + 1 < chars.len() && chars[last + 1].is_ascii_digit() {
            last += 1;
        }
        if first > 0 && chars[first - 1] == '-' {
            first -= 1;
        }
        let number: String = chars[first..=last].iter().collect();
        let Ok(value) = number.parse::<i64>() else {
            self.cancel();
            return Effect::Handled;
        };
        let new = value
            .saturating_add(delta.saturating_mul(count))
            .to_string();
        buf.begin_change(self.cursor);
        self.edit(buf, start + first..start + last + 1, &new);
        self.cursor = start + first + new.chars().count() - 1;
        buf.end_change(self.cursor);
        self.done(buf, true);
        Effect::Handled
    }

    /// A mouse click at character `pos`: the cursor goes there. A selection
    /// ends; an insert goes on from there (as a new undo step, like an
    /// arrow key); NORMAL keeps the cursor on a character.
    pub fn click(&mut self, pos: usize, buf: &mut Buffer) {
        let text = buf.text();
        let pos = pos.min(text.len_chars());
        if self.cmd.cmdline.is_some() {
            return;
        }
        match self.mode {
            Mode::Insert => {
                if pos != self.cursor {
                    buf.end_change(self.cursor);
                    self.cursor = pos;
                    buf.begin_change(self.cursor);
                    if let Some(session) = &mut self.insert {
                        session.broken = true;
                    }
                }
            }
            mode => {
                if mode.is_visual() {
                    self.set_visual_marks(text);
                }
                self.cursor = pos;
                self.enter(Mode::Normal, text);
            }
        }
        self.want_col = None;
    }

    /// Selects from `anchor` to `cursor` (characters, both inclusive), by
    /// line if `linewise`: a mouse drag, double or triple click. An insert
    /// ends first.
    pub fn select(&mut self, anchor: usize, cursor: usize, linewise: bool, buf: &mut Buffer) {
        if self.cmd.cmdline.is_some() {
            return;
        }
        if self.mode == Mode::Insert {
            self.handle_key(Key::Esc, buf);
        }
        let text = buf.text();
        let last = text.len_chars().saturating_sub(1);
        self.anchor = anchor.min(last);
        self.cursor = cursor.min(last);
        let mode = if linewise {
            Mode::VisualLine
        } else {
            Mode::Visual
        };
        self.mode = mode;
        self.pending = Pending::None;
        self.op = None;
        self.count = None;
        self.want_col = None;
    }

    /// Plugs in where long lines wrap on screen, for `gj` / `gk`.
    pub fn set_screen_rows(&mut self, rows: Box<dyn ScreenRows>) {
        self.screen_rows = Some(rows);
    }

    /// Plain `j` / `k` (and the arrows) move by screen row, or by line.
    pub fn set_wrap_motion(&mut self, on: bool) {
        self.wrap_motion = on;
    }

    /// Joins `lines` lines (at least two) from line `first` into one, as
    /// one undo step. With `spaces` (`J`), the joined lines lose their
    /// leading whitespace and one space separates them, except after an
    /// empty line or a trailing space, or before `)`; without (`gJ`), they
    /// join as they are. The cursor ends at the last join.
    fn join_lines(&mut self, first: usize, lines: usize, spaces: bool, buf: &mut Buffer) {
        buf.begin_change(self.cursor);
        let mut at = self.cursor;
        for _ in 1..lines.max(2) {
            let text = buf.text();
            if first >= last_line(text) {
                break;
            }
            let end = line_start(text, first) + line_len(text, first);
            let next = end + 1;
            let next_text: String = text
                .slice(next..next + line_len(text, first + 1))
                .to_string();
            let (cut, sep) = if spaces {
                let blank = next_text
                    .chars()
                    .take_while(|c| matches!(c, ' ' | '\t'))
                    .count();
                let rest = &next_text[next_text
                    .char_indices()
                    .nth(blank)
                    .map_or(next_text.len(), |(b, _)| b)..];
                let last = (line_len(text, first) > 0).then(|| text.char(end - 1));
                let no_space = rest.is_empty()
                    || rest.starts_with(')')
                    || last.is_none_or(|c| matches!(c, ' ' | '\t'));
                (blank, if no_space { "" } else { " " })
            } else {
                (0, "")
            };
            self.edit(buf, end..next + cut, sep);
            at = end;
        }
        self.cursor = clamp_normal(buf.text(), at);
        buf.end_change(self.cursor);
        self.done(buf, true);
    }

    /// `⌥j` / `⌥k`, and `J` / `K` in VISUAL: swaps the cursor line (or the
    /// selected lines) with the `count` lines below or above, as one undo
    /// step, stopping at the first and last line. The cursor (and the
    /// selection) stay on the moved lines, so the key can be pressed again.
    fn move_lines(&mut self, down: bool, buf: &mut Buffer) -> Effect {
        let count = self.count.take().unwrap_or(1);
        let text = buf.text();
        let visual = self.mode.is_visual();
        let (a, b) = if visual {
            (self.anchor.min(self.cursor), self.anchor.max(self.cursor))
        } else {
            (self.cursor, self.cursor)
        };
        let (first, last) = (line_of(text, a), line_of(text, b));
        let n = if down {
            count.min(last_line(text) - last)
        } else {
            count.min(first)
        };
        if n == 0 {
            self.done(buf, false);
            return Effect::Handled;
        }
        let (from, to) = if down {
            (first, last + n)
        } else {
            (first - n, last)
        };
        let lines: Vec<String> = (from..=to)
            .map(|l| {
                let start = line_start(text, l);
                text.slice(start..start + line_len(text, l)).to_string()
            })
            .collect();
        // The block, and the lines it trades places with.
        let split = if down { last - from + 1 } else { first - from };
        let (head, tail) = lines.split_at(split);
        let moved = [tail, head].concat().join("\n");
        let start = line_start(text, from);
        let end = line_start(text, to) + line_len(text, to);
        // Where the cursor and anchor sit: line and column, to put back.
        let place = |pos: usize| {
            let line = line_of(text, pos);
            (line, pos - line_start(text, line))
        };
        let (cursor_at, anchor_at) = (place(self.cursor), place(self.anchor));
        buf.begin_change(self.cursor);
        self.edit(buf, start..end, &moved);
        let text = buf.text();
        let shift = |(line, col): (usize, usize)| {
            let line = if down { line + n } else { line - n };
            line_start(text, line) + col.min(line_len(text, line))
        };
        self.cursor = shift(cursor_at);
        if visual {
            self.anchor = shift(anchor_at);
        } else {
            self.cursor = clamp_normal(text, self.cursor);
        }
        buf.end_change(self.cursor);
        self.done(buf, true);
        Effect::Handled
    }

    /// `J` / `gJ` in VISUAL: joins the selected lines (at least two).
    fn join_selection(&mut self, spaces: bool, buf: &mut Buffer) -> Effect {
        let text = buf.text();
        let (a, b) = (self.anchor.min(self.cursor), self.anchor.max(self.cursor));
        let (first, last) = (line_of(text, a), line_of(text, b));
        self.leave_visual(text);
        self.join_lines(first, last - first + 1, spaces, buf);
        Effect::Handled
    }

    /// `gj` / `gk` (and `j` / `k` with `wrap_motion`): moves `count` screen
    /// rows, keeping the column within the row. Without wrap information
    /// it moves by line, as `j` / `k` do.
    fn screen_motion(&mut self, down: bool, buf: &mut Buffer) -> Effect {
        let count = self.count.take().unwrap_or(1);
        if self.screen_rows.is_none() {
            self.count = Some(count);
            let motion = if down { Motion::Down } else { Motion::Up };
            return self.run_motion(motion, buf);
        }
        let text = buf.text();
        for _ in 0..count {
            self.cursor = self.screen_step(down, text);
        }
        if self.mode == Mode::Normal {
            self.cursor = clamp_normal(text, self.cursor);
        }
        self.want_col = None;
        self.done(buf, false);
        Effect::Handled
    }

    /// The position one screen row down or up from the cursor.
    fn screen_step(&self, down: bool, text: &Rope) -> usize {
        let rows = |line: usize| {
            self.screen_rows
                .as_ref()
                .map(|r| r.row_starts(line))
                .unwrap_or_default()
        };
        let line = line_of(text, self.cursor);
        let start = line_start(text, line);
        let len = line_len(text, line);
        let col = self.cursor - start;
        let starts = rows(line);
        // This row, and the column within it.
        let row = starts.iter().filter(|&&s| s <= col).count();
        let row_start = if row == 0 { 0 } else { starts[row - 1] };
        let in_row = col - row_start;
        // A position `in_row` along the row from `from` to `to` (exclusive).
        let place = |base: usize, from: usize, to: usize| {
            base + (from + in_row).min(to.saturating_sub(1).max(from))
        };
        if down {
            if row < starts.len() {
                let to = starts.get(row + 1).copied().unwrap_or(len);
                return place(start, starts[row], to);
            }
            if line < last_line(text) {
                let next = line + 1;
                let to = rows(next).first().copied().unwrap_or(line_len(text, next));
                return place(line_start(text, next), 0, to);
            }
        } else {
            if row > 0 {
                let from = if row >= 2 { starts[row - 2] } else { 0 };
                return place(start, from, starts[row - 1]);
            }
            if line > 0 {
                let prev = line - 1;
                let from = rows(prev).last().copied().unwrap_or(0);
                return place(line_start(text, prev), from, line_len(text, prev));
            }
        }
        self.cursor
    }

    pub(crate) fn typed(&mut self, buf: &mut Buffer, s: &str) {
        self.insert_text(buf, s);
        if let Some(session) = &mut self.insert {
            session.typed.push_str(s);
        }
    }

    fn insert_text(&mut self, buf: &mut Buffer, s: &str) {
        self.edit(buf, self.cursor..self.cursor, s);
        self.cursor += s.chars().count();
    }

    /// INSERT at the cursor with no count, in the undo step already begun.
    pub(crate) fn begin_insert(&mut self, buf: &mut Buffer) {
        self.start_insert(buf, 1, "");
    }

    /// Starts insert mode at the cursor as part of the current undo step.
    fn start_insert(&mut self, buf: &mut Buffer, count: usize, prefix: &str) {
        self.insert = Some(InsertSession {
            count: count.max(1),
            prefix: prefix.to_owned(),
            start: self.cursor,
            ..InsertSession::default()
        });
        self.enter(Mode::Insert, buf.text());
    }

    // ---- normal and visual mode -------------------------------------------

    fn normal_key(&mut self, key: Key, buf: &mut Buffer) -> Effect {
        let pending = std::mem::take(&mut self.pending);
        let text = buf.text();
        let motion = match (pending, key) {
            (Pending::None, key) => return self.normal_start(key, buf),
            (Pending::Register, Key::Char(c)) if is_register_name(c) => {
                self.register = Some(c);
                return Effect::Handled;
            }
            (Pending::Q, Key::Char(c @ (':' | '/' | '?'))) => {
                let kind = match c {
                    ':' => CommandKind::Ex,
                    c => CommandKind::Search { forward: c == '/' },
                };
                self.open_cmd_window(kind, "", buf);
                return Effect::Handled;
            }
            (Pending::Q, Key::Char(c)) => return self.start_recording(c),
            (Pending::At, Key::Char(c)) if is_register_name(c) || matches!(c, '@' | ':') => {
                return self.run_macro(c, buf);
            }
            (Pending::Replace, Key::Char(c)) => return self.replace_chars(c, buf),
            (Pending::Replace, Key::Enter) => return self.replace_chars('\n', buf),
            (Pending::Replace, Key::Tab) => return self.replace_chars('\t', buf),
            (Pending::SetMark, Key::Char(c)) if c.is_ascii_lowercase() => {
                self.marks.insert(c, self.cursor);
                self.done(buf, false);
                return Effect::Handled;
            }
            (Pending::JumpMark { exact }, Key::Char(c)) => {
                let name = if c == '`' { '\'' } else { c };
                let Some(&pos) = self.marks.get(&name) else {
                    self.cancel();
                    return Effect::Handled;
                };
                let pos = pos.min(text.len_chars());
                let target = if exact {
                    Target {
                        pos,
                        kind: MotionKind::Exclusive,
                        keep_column: false,
                    }
                } else {
                    Target {
                        pos: first_non_blank(text, line_of(text, pos)),
                        kind: MotionKind::Linewise,
                        keep_column: false,
                    }
                };
                return self.arrive(Some(target), true, buf);
            }
            (Pending::Find { forward, till }, Key::Char(ch)) => {
                let find = Find { ch, forward, till };
                self.last_find = Some(find);
                Motion::Find(find)
            }
            // Extensions on `g…` keys come before the built-ins.
            (Pending::G, Key::Char(c))
                if crate::ext::motion_for(crate::ext::Keys::G(c)).is_some() =>
            {
                let id = crate::ext::motion_for(crate::ext::Keys::G(c)).unwrap_or_default();
                Motion::Custom(id)
            }
            (Pending::G, Key::Char(c))
                if crate::ext::operator_for(crate::ext::Keys::G(c)).is_some() =>
            {
                let id = crate::ext::operator_for(crate::ext::Keys::G(c)).unwrap_or_default();
                return self.operator_key(Operator::Func(id), buf);
            }
            // `g@`: the operator named by `operatorfunc`.
            (Pending::G, Key::Char('@')) => {
                let name = self.options.operatorfunc.clone();
                match crate::ext::operator_named(&name) {
                    Some(id) => return self.operator_key(Operator::Func(id), buf),
                    None => {
                        self.cancel();
                        self.cmd.message = Some(crate::ex::cmdline::Message {
                            text: "E774: 'operatorfunc' is empty".into(),
                            error: true,
                            line: None,
                        });
                        return Effect::Handled;
                    }
                }
            }
            (Pending::G, Key::Char('g')) => Motion::FirstLine,
            (Pending::G, Key::Char(c @ ('e' | 'E'))) => Motion::WordEndBackward { big: c == 'E' },
            (Pending::G, Key::Char('_')) => Motion::LastNonBlank,
            // `g*` / `g#`: the word under the cursor, also inside longer words.
            (Pending::G, Key::Char(c @ ('*' | '#'))) => {
                self.pending = Pending::None;
                return self.search_word(c == '*', false, buf);
            }
            // `gJ` joins lines as they are, with no spaces added or removed.
            (Pending::G, Key::Char('J')) if self.op.is_none() => {
                self.pending = Pending::None;
                if self.mode.is_visual() {
                    return self.join_selection(false, buf);
                }
                let lines = self.count.take().unwrap_or(2).max(2);
                let first = line_of(buf.text(), self.cursor);
                self.join_lines(first, lines, false, buf);
                return Effect::Handled;
            }
            // `gj` / `gk`: one screen row of a wrapped line.
            (Pending::G, Key::Char(c @ ('j' | 'k'))) if self.op.is_none() => {
                self.pending = Pending::None;
                return self.screen_motion(c == 'j', buf);
            }
            // `gt` / `gT` / `g<Tab>`: tab pages, which are the app's. `{N}gt`
            // goes to tab N, `{N}gT` back N tabs, `g<Tab>` to the last used.
            (Pending::G, Key::Char(c @ ('t' | 'T'))) if self.op.is_none() => {
                let count = self.count.take();
                self.cancel();
                if self.mode.is_visual() {
                    self.leave_visual(buf.text());
                }
                let mut command =
                    crate::ex::AppCommand::new(if c == 't' { "tabnext" } else { "tabprevious" });
                if let Some(n) = count {
                    command.args = n.to_string();
                }
                self.cmd.app_command = Some(command);
                return Effect::Handled;
            }
            (Pending::G, Key::Tab) if self.op.is_none() => {
                self.cancel();
                if self.mode.is_visual() {
                    self.leave_visual(buf.text());
                }
                let mut command = crate::ex::AppCommand::new("tabnext");
                command.args = "#".to_owned();
                self.cmd.app_command = Some(command);
                return Effect::Handled;
            }
            // `gd` follows the link under the cursor: the app does that.
            (Pending::G, Key::Char('d')) => {
                self.cancel();
                self.cmd.app_command = Some(crate::ex::AppCommand::new("follow"));
                return Effect::Handled;
            }
            (Pending::G, Key::Char(c @ ('c' | 'b'))) => {
                let op = if c == 'c' {
                    Operator::Comment
                } else {
                    Operator::BlockComment
                };
                return self.operator_key(op, buf);
            }
            (Pending::G, Key::Char(c @ ('u' | 'U' | '~'))) => {
                let op = match c {
                    'u' => Operator::Lower,
                    'U' => Operator::Upper,
                    _ => Operator::Toggle,
                };
                return self.operator_key(op, buf);
            }
            // `gp` / `gP`: put, leaving the cursor just after the text.
            (Pending::G, Key::Char(c @ ('p' | 'P')))
                if self.op.is_none() && !self.mode.is_visual() =>
            {
                self.paste_moving(c == 'p', true, buf);
                return Effect::Handled;
            }
            // `gi`: INSERT where it last ended.
            (Pending::G, Key::Char('i')) if self.op.is_none() && !self.mode.is_visual() => {
                let at = self.marks.get(&'^').copied().unwrap_or(self.cursor);
                let line = line_of(text, at.min(text.len_chars()));
                self.cursor = at.min(line_end(text, line));
                let count = self.count.take().unwrap_or(1);
                buf.begin_change(self.cursor);
                self.start_insert(buf, count, "");
                return Effect::Handled;
            }
            // `g&`: the last `:s` again, with its flags, on every line.
            (Pending::G, Key::Char('&')) if self.op.is_none() && !self.mode.is_visual() => {
                return self.repeat_substitute(true, buf);
            }
            (Pending::Z, Key::Char(c @ ('t' | 'z' | 'b' | '.' | '-'))) if self.op.is_none() => {
                self.pending = Pending::None;
                return self.scroll_cursor(c, buf);
            }
            (Pending::Z, Key::Enter) if self.op.is_none() => {
                self.pending = Pending::None;
                return self.scroll_cursor('\n', buf);
            }
            // `]p` and `[p` (`]P`, `[P`): put lines, indented to fit.
            (Pending::Bracket { forward }, Key::Char(c @ ('p' | 'P'))) if self.op.is_none() => {
                self.pending = Pending::None;
                self.paste_indented(forward && c == 'p', buf);
                return Effect::Handled;
            }
            (Pending::Bracket { forward }, Key::Char(c))
                if c == if forward { ']' } else { '[' } =>
            {
                if forward {
                    Motion::NextHeading
                } else {
                    Motion::PrevHeading
                }
            }
            (Pending::Object { around }, Key::Char(obj)) => {
                return self.text_object(obj, around, buf);
            }
            // Any other key cancels the half-typed sequence, as in Vim.
            _ => {
                self.cancel();
                return Effect::Handled;
            }
        };
        self.run_motion(motion, buf)
    }

    fn normal_start(&mut self, key: Key, buf: &mut Buffer) -> Effect {
        // `⌃a` / `⌃x`: add to or subtract from the number under the cursor.
        if let Key::Ctrl(c @ ('a' | 'x')) = key
            && self.op.is_none()
            && self.mode == Mode::Normal
        {
            return self.add_to_number(if c == 'a' { 1 } else { -1 }, buf);
        }
        // `⌃o` / `⌃i` (Tab): back and forward through the jump list; `⌃e` /
        // `⌃y`: scroll a line.
        if self.op.is_none() {
            match key {
                Key::Ctrl('o') if self.mode == Mode::Normal => return self.jump_list(true, buf),
                Key::Ctrl('i') | Key::Tab if self.mode == Mode::Normal => {
                    return self.jump_list(false, buf);
                }
                Key::Ctrl(c @ ('e' | 'y')) => return self.scroll_lines(c == 'e', buf),
                // `⌃v`: VISUAL BLOCK, or back to NORMAL from it.
                Key::Ctrl('v') => {
                    let text = buf.text();
                    if self.mode == Mode::VisualBlock {
                        self.leave_visual(text);
                    } else {
                        self.enter(Mode::VisualBlock, text);
                    }
                    return Effect::Handled;
                }
                _ => {}
            }
        }
        // `⌥j` / `⌥k`: the line (or the selected lines) down or up.
        if let Key::Alt(c @ ('j' | 'k')) = key
            && self.op.is_none()
        {
            return self.move_lines(c == 'j', buf);
        }
        let c = match key {
            Key::Char(c) => c,
            // Arrows move like `h` `j` `k` `l`, Home and End like `0` `$`.
            Key::Left => 'h',
            Key::Down => 'j',
            Key::Up => 'k',
            Key::Right => 'l',
            Key::Home => '0',
            Key::End => '$',
            // Enter is `+`, as in Vim.
            Key::Enter => '+',
            // A page (or half) down or up: `j` / `k` that many times.
            Key::PageDown | Key::PageUp | Key::Ctrl('f' | 'b' | 'd' | 'u')
                if self.count.is_none() =>
            {
                let half = matches!(key, Key::Ctrl('d' | 'u'));
                let lines = if half { self.page() / 2 } else { self.page() };
                self.count = Some(lines.max(1));
                if matches!(key, Key::PageDown | Key::Ctrl('f' | 'd')) {
                    'j'
                } else {
                    'k'
                }
            }
            Key::Esc => {
                let was_visual = self.mode.is_visual();
                self.cancel();
                if was_visual {
                    self.leave_visual(buf.text());
                }
                return Effect::Handled;
            }
            Key::Ctrl('r') if self.op.is_none() => {
                for _ in 0..self.count.take().unwrap_or(1) {
                    if let Some(pos) = buf.redo() {
                        self.cursor = clamp_normal(buf.text(), pos);
                    }
                }
                self.done(buf, false);
                return Effect::Handled;
            }
            _ => {
                self.cancel();
                return Effect::Unhandled(key);
            }
        };

        // Counts: `0` is a motion unless a count is already being typed.
        if let Some(d) = c.to_digit(10)
            && (d != 0 || self.count.is_some())
        {
            let count = self
                .count
                .unwrap_or(0)
                .saturating_mul(10)
                .saturating_add(d as usize);
            self.count = Some(count.min(999_999));
            return Effect::Handled;
        }

        // With `wrap_motion`, plain `j` / `k` move by screen row too.
        if self.wrap_motion && self.op.is_none() && matches!(c, 'j' | 'k') {
            return self.screen_motion(c == 'j', buf);
        }
        // Extension motions come before the built-ins.
        if let Some(id) = crate::ext::motion_for(crate::ext::Keys::Plain(c)) {
            return self.run_motion(Motion::Custom(id), buf);
        }
        if let Some(motion) = motion_for(c) {
            return self.run_motion(motion, buf);
        }
        match c {
            'H' | 'M' | 'L' => return self.window_line(c, buf),
            '*' | '#' => return self.search_word(c == '*', true, buf),
            _ => {}
        }

        let waits = match c {
            'g' => Some(Pending::G),
            ']' => Some(Pending::Bracket { forward: true }),
            '[' => Some(Pending::Bracket { forward: false }),
            'f' => Some(Pending::Find {
                forward: true,
                till: false,
            }),
            't' => Some(Pending::Find {
                forward: true,
                till: true,
            }),
            'F' => Some(Pending::Find {
                forward: false,
                till: false,
            }),
            'T' => Some(Pending::Find {
                forward: false,
                till: true,
            }),
            '\'' => Some(Pending::JumpMark { exact: false }),
            '`' => Some(Pending::JumpMark { exact: true }),
            '"' if self.op.is_none() => Some(Pending::Register),
            'm' if self.op.is_none() && !self.mode.is_visual() => Some(Pending::SetMark),
            'r' if self.op.is_none() => Some(Pending::Replace),
            'z' if self.op.is_none() => Some(Pending::Z),
            // `q{reg}` records a macro; `q:` opens the command-line window.
            'q' if self.op.is_none() && self.recording().is_some() => {
                return self.stop_recording();
            }
            'q' if self.op.is_none() && !self.mode.is_visual() => Some(Pending::Q),
            '@' if self.op.is_none() && !self.mode.is_visual() => Some(Pending::At),
            'i' | 'a' if self.op.is_some() || self.mode.is_visual() => {
                Some(Pending::Object { around: c == 'a' })
            }
            _ => None,
        };
        if let Some(pending) = waits {
            self.pending = pending;
            return Effect::Handled;
        }

        if c == ';' || c == ',' {
            let count = self.count.take();
            let Some(find) = self.last_find else {
                self.cancel();
                return Effect::Handled;
            };
            let find = if c == ',' { find.reversed() } else { find };
            let target = repeat_find(buf.text(), self.cursor, find, count);
            return self.arrive(target, false, buf);
        }

        if c == 'n' || c == 'N' {
            let count = self.take_count().unwrap_or(1);
            self.search_next(c == 'N', count, buf);
            if self.op.is_some() {
                // The search failed; the operator goes with it.
                self.cancel();
            }
            return Effect::Handled;
        }

        // An operator typed twice applies to whole lines (`dd`, `guu`).
        if let Some(pending) = self.op
            && (pending.op.line_key() == c || Operator::from_key(c) == Some(pending.op))
        {
            return self.operate_on_lines(buf);
        }
        if let Some(id) = crate::ext::operator_for(crate::ext::Keys::Plain(c)) {
            return self.operator_key(Operator::Func(id), buf);
        }
        if let Some(op) = Operator::from_key(c) {
            return self.operator_key(op, buf);
        }
        if self.op.is_some() {
            self.cancel();
            return Effect::Handled;
        }

        if self.mode.is_visual() {
            return self.visual_command(c, buf);
        }
        self.normal_command(c, buf)
    }

    /// Commands that act at once in normal mode (no operator pending).
    fn normal_command(&mut self, c: char, buf: &mut Buffer) -> Effect {
        let text = buf.text();
        let line = line_of(text, self.cursor);
        match c {
            'i' | 'a' | 'I' | 'A' => {
                let count = self.count.take().unwrap_or(1);
                self.cursor = match c {
                    'a' => (self.cursor + 1).min(line_end(text, line)),
                    'I' => first_non_blank(text, line).min(line_end(text, line)),
                    'A' => line_end(text, line),
                    _ => self.cursor,
                };
                buf.begin_change(self.cursor);
                self.start_insert(buf, count, "");
            }
            'o' | 'O' => {
                let count = self.count.take().unwrap_or(1);
                let fallback = indent_of(text, line);
                let (start, end) = (line_start(text, line), line_end(text, line));
                buf.begin_change(self.cursor);
                // The new line first, then its indent from the syntax
                // around it (the tree follows the edit at once).
                if c == 'o' {
                    self.cursor = end;
                    self.insert_text(buf, "\n");
                    let indent = self.syntax_indent(buf, line + 1).unwrap_or(fallback);
                    self.insert_text(buf, &indent);
                } else {
                    self.edit(buf, start..start, "\n");
                    self.cursor = start;
                    let indent = self.syntax_indent(buf, line).unwrap_or(fallback);
                    self.insert_text(buf, &indent);
                }
                self.start_insert(buf, count, "\n");
            }
            'x' => return self.shortcut(Operator::Delete, Motion::Right, buf),
            'X' => return self.shortcut(Operator::Delete, Motion::Left, buf),
            'D' => return self.shortcut(Operator::Delete, Motion::LineEnd, buf),
            'C' => return self.shortcut(Operator::Change, Motion::LineEnd, buf),
            's' => return self.shortcut(Operator::Change, Motion::Right, buf),
            'S' | 'Y' => {
                let op = if c == 'S' {
                    Operator::Change
                } else {
                    Operator::Yank
                };
                self.op = Some(OpPending {
                    op,
                    count: self.count.take(),
                });
                return self.operate_on_lines(buf);
            }
            'p' | 'P' => self.paste(c == 'p', buf),
            '~' => return self.toggle_chars(buf),
            'R' => {
                let count = self.count.take().unwrap_or(1);
                buf.begin_change(self.cursor);
                self.replace = Some(ReplaceSession {
                    count,
                    ..ReplaceSession::default()
                });
                self.enter(Mode::Replace, buf.text());
            }
            '&' => return self.repeat_substitute(false, buf),
            'J' => {
                let lines = self.count.take().unwrap_or(2).max(2);
                let first = line_of(text, self.cursor);
                self.join_lines(first, lines, true, buf);
            }
            'u' => {
                for _ in 0..self.count.take().unwrap_or(1) {
                    if let Some(pos) = buf.undo() {
                        self.cursor = clamp_normal(buf.text(), pos);
                    }
                }
                self.done(buf, false);
            }
            '.' => self.repeat_last_change(buf),
            'v' => {
                self.enter(Mode::Visual, text);
            }
            'V' => {
                self.enter(Mode::VisualLine, text);
            }
            ':' => self.open_command_line(CommandKind::Ex, "", buf),
            '/' => self.open_command_line(CommandKind::Search { forward: true }, "", buf),
            '?' => self.open_command_line(CommandKind::Search { forward: false }, "", buf),
            _ => {
                self.cancel();
                return Effect::Unhandled(Key::Char(c));
            }
        }
        Effect::Handled
    }

    /// Commands in visual mode.
    fn visual_command(&mut self, c: char, buf: &mut Buffer) -> Effect {
        let text = buf.text();
        let op = match c {
            'd' | 'x' => Some(Operator::Delete),
            'c' | 's' => Some(Operator::Change),
            'y' => Some(Operator::Yank),
            '>' => Some(Operator::Indent),
            '<' => Some(Operator::Outdent),
            'u' => Some(Operator::Lower),
            'U' => Some(Operator::Upper),
            '~' => Some(Operator::Toggle),
            _ => None,
        };
        if let Some(op) = op
            && self.mode == Mode::VisualBlock
        {
            self.block_operator(op, buf);
            return Effect::Handled;
        }
        if self.mode == Mode::VisualBlock {
            match c {
                'I' | 'A' => {
                    self.block_insert(c == 'A', buf);
                    return Effect::Handled;
                }
                'O' => {
                    self.block_other_corner(text);
                    return Effect::Handled;
                }
                'p' | 'P' => return self.block_put_over(buf),
                'v' | 'V' => {
                    self.block.to_end = false;
                    let mode = if c == 'v' {
                        Mode::Visual
                    } else {
                        Mode::VisualLine
                    };
                    self.enter(mode, text);
                    return Effect::Handled;
                }
                _ => {}
            }
        }
        if let Some(op) = op {
            let (range, linewise) = self.visual_range(text);
            self.leave_visual(text);
            self.apply_operator(op, range, linewise, buf);
            return Effect::Handled;
        }
        // `J` / `K` move the selected lines (`gJ` still joins them).
        if c == 'J' || c == 'K' {
            return self.move_lines(c == 'J', buf);
        }
        match (self.mode, c) {
            (_, 'p' | 'P') => {
                let reg = self.registers.get(self.register.take());
                let (range, linewise) = self.visual_range(text);
                self.leave_visual(text);
                if let Some(reg) = reg {
                    let removed = text.slice(range.clone()).to_string();
                    buf.begin_change(self.cursor);
                    self.edit(buf, range.clone(), "");
                    self.cursor = range.start;
                    // Replacing the last lines: put after the line before.
                    let past_end = linewise && range.start > 0 && range.start >= buf.len_chars();
                    if past_end {
                        self.cursor = range.start - 1;
                    }
                    self.put(&reg, past_end, buf, 1);
                    self.registers.delete(
                        None,
                        Register {
                            text: removed,
                            linewise,
                            block: false,
                        },
                    );
                    buf.end_change(self.cursor);
                }
            }
            (Mode::VisualLine, 'v') => {
                self.enter(Mode::Visual, text);
            }
            (Mode::Visual, 'V') => {
                self.enter(Mode::VisualLine, text);
            }
            (_, 'O') => {
                std::mem::swap(&mut self.anchor, &mut self.cursor);
                self.want_col = None;
            }
            (Mode::Visual, 'v') | (Mode::VisualLine, 'V') => self.leave_visual(text),
            (_, 'o') => {
                std::mem::swap(&mut self.anchor, &mut self.cursor);
                self.want_col = None;
            }
            (_, ':') => {
                self.set_visual_marks(text);
                self.open_command_line(CommandKind::Ex, "'<,'>", buf);
            }
            _ => {
                self.cancel();
                return Effect::Unhandled(Key::Char(c));
            }
        }
        Effect::Handled
    }

    fn operator_key(&mut self, op: Operator, buf: &mut Buffer) -> Effect {
        if self.mode == Mode::VisualBlock {
            self.block_operator(op, buf);
            return Effect::Handled;
        }
        if self.mode.is_visual() {
            let text = buf.text();
            let (range, linewise) = self.visual_range(text);
            self.leave_visual(text);
            self.apply_operator(op, range, linewise, buf);
            return Effect::Handled;
        }
        match self.op {
            Some(pending) if pending.op == op => self.operate_on_lines(buf),
            Some(_) => {
                self.cancel();
                Effect::Handled
            }
            None => {
                self.op = Some(OpPending {
                    op,
                    count: self.count.take(),
                });
                Effect::Handled
            }
        }
    }

    /// `x`, `D`, `C` and friends: an operator with a fixed motion.
    fn shortcut(&mut self, op: Operator, motion: Motion, buf: &mut Buffer) -> Effect {
        self.op = Some(OpPending { op, count: None });
        self.run_motion(motion, buf)
    }

    fn run_motion(&mut self, motion: Motion, buf: &mut Buffer) -> Effect {
        let count = self.take_count();
        // Only a motion that reads the syntax costs a snapshot.
        let syntax = if motion.wants_syntax() {
            buf.syntax_snapshot()
        } else {
            None
        };
        let syntax = syntax.as_ref();
        let text = buf.text();
        if self.op.is_some() {
            // `cw` on a word changes to its end, like `ce` (Vim's special case).
            if self.op.is_some_and(|p| p.op == Operator::Change)
                && let Motion::WordForward { big } = motion
                && char_at_is_word(text, self.cursor)
            {
                let end = change_word_end(text, self.cursor, count.unwrap_or(1), big);
                let target = Target {
                    pos: end,
                    kind: MotionKind::Inclusive,
                    keep_column: false,
                };
                return self.arrive(Some(target), false, buf);
            }
            let past_end = matches!(motion, Motion::Right);
            let target = motion.apply(text, syntax, self.cursor, count, None, past_end);
            self.macros.failed |= stuck(text, motion, self.cursor, target);
            return self.arrive(target, is_jump(motion), buf);
        }
        if self.mode == Mode::VisualBlock {
            // `$` widens every line to its end until a sideways motion.
            match motion {
                Motion::LineEnd => self.block.to_end = true,
                Motion::Up | Motion::Down => {}
                _ => self.block.to_end = false,
            }
        }
        let past_end = self.mode != Mode::Normal;
        let target = motion.apply(text, syntax, self.cursor, count, self.want_col, past_end);
        self.macros.failed |= stuck(text, motion, self.cursor, target);
        self.arrive(target, is_jump(motion), buf)
    }

    /// Finishes a motion: applies the pending operator, or moves the cursor.
    pub(crate) fn arrive(
        &mut self,
        target: Option<Target>,
        jump: bool,
        buf: &mut Buffer,
    ) -> Effect {
        let Some(target) = target else {
            self.macros.failed = true;
            self.cancel();
            return Effect::Handled;
        };
        let text = buf.text();
        if let Some(pending) = self.op.take() {
            let (range, linewise) = motion_range(text, self.cursor, target);
            self.apply_operator(pending.op, range, linewise, buf);
            return Effect::Handled;
        }
        if jump {
            self.record_jump(text);
        }
        if target.keep_column {
            if self.want_col.is_none() {
                self.want_col = Some(column(text, self.cursor));
            }
        } else {
            self.want_col = None;
        }
        self.cursor = if self.mode == Mode::Normal {
            clamp_normal(text, target.pos)
        } else {
            target.pos.min(text.len_chars())
        };
        self.done(buf, false);
        Effect::Handled
    }

    fn text_object(&mut self, obj: char, around: bool, buf: &mut Buffer) -> Effect {
        let count = self.take_count().unwrap_or(1);
        let ext = crate::ext::text_object(obj);
        let syntax = match &ext {
            Some(run) if run.wants_syntax() => buf.syntax_snapshot(),
            _ => None,
        };
        let text = buf.text();
        let sel = match ext {
            Some(run) => run.run(text, syntax.as_ref(), self.cursor, around, count),
            None => textobj::select(text, self.cursor, obj, around, count),
        };
        let Some(sel) = sel else {
            self.macros.failed = true;
            self.cancel();
            return Effect::Handled;
        };
        if self.mode.is_visual() {
            // Visual `iw` and friends select the object.
            if sel.linewise && self.mode == Mode::Visual {
                self.enter(Mode::VisualLine, text);
            }
            self.anchor = sel.range.start;
            self.cursor = sel.range.end.saturating_sub(1).max(sel.range.start);
            return Effect::Handled;
        }
        match self.op.take() {
            Some(pending) => self.apply_operator(pending.op, sel.range, sel.linewise, buf),
            None => self.cancel(),
        }
        Effect::Handled
    }

    /// `dd`, `3yy`, `>>`, `guu`, `S`, `Y`: the operator on `count` lines.
    fn operate_on_lines(&mut self, buf: &mut Buffer) -> Effect {
        let Some(pending) = self.op.take() else {
            return Effect::Handled;
        };
        let n = combine(pending.count, self.count.take()).unwrap_or(1);
        let text = buf.text();
        let first = line_of(text, self.cursor);
        let last = (first + n - 1).min(last_line(text));
        let range = line_range(text, first, last);
        self.apply_operator(pending.op, range, true, buf);
        Effect::Handled
    }

    pub(crate) fn apply_operator(
        &mut self,
        op: Operator,
        range: Range<usize>,
        linewise: bool,
        buf: &mut Buffer,
    ) {
        let text = buf.text();
        let len = text.len_chars();
        let range = range.start.min(len)..range.end.min(len);
        let mut taken = text.slice(range.clone()).to_string();
        if linewise && !taken.ends_with('\n') {
            taken.push('\n');
        }
        let reg = Register {
            text: taken,
            linewise,
            block: false,
        };
        let name = self.register.take();
        // Everything read from the text happens before the first edit.
        let start_line = line_of(text, range.start);
        let end_line = line_of(text, range.end.saturating_sub(1).max(range.start));
        let indent = indent_of(text, start_line);
        let ends_in_newline = ends_with_newline(text, range.end);
        let original = text.slice(range.clone()).to_string();
        let cursor_line = line_of(text, self.cursor);
        match op {
            Operator::Yank => {
                self.registers.yank(name, reg);
                if !linewise {
                    self.cursor = clamp_normal(text, range.start);
                } else if start_line < cursor_line {
                    self.cursor = first_non_blank(text, start_line);
                }
                self.done(buf, false);
            }
            Operator::Delete => {
                self.registers.delete(name, reg);
                let mut del = range.clone();
                // Deleting the last lines also takes the newline before them.
                if linewise && del.end >= len && del.start > 0 && !ends_in_newline {
                    del.start -= 1;
                }
                buf.begin_change(self.cursor);
                self.edit(buf, del, "");
                let text = buf.text();
                let at = range.start.min(text.len_chars());
                self.cursor = if linewise {
                    first_non_blank(text, line_of(text, at))
                } else {
                    clamp_normal(text, at)
                };
                buf.end_change(self.cursor);
                self.done(buf, true);
            }
            Operator::Change => {
                self.registers.delete(name, reg);
                buf.begin_change(self.cursor);
                if linewise {
                    // Keep the first line's indent and one (empty) line.
                    let end = if ends_in_newline {
                        range.end - 1
                    } else {
                        range.end
                    };
                    self.edit(buf, range.start..end, &indent);
                    self.cursor = range.start + indent.chars().count();
                } else {
                    self.edit(buf, range.clone(), "");
                    self.cursor = range.start;
                }
                self.start_insert(buf, 1, "");
            }
            Operator::Comment | Operator::BlockComment => {
                self.comment(op, range.clone(), linewise, start_line, end_line, buf);
            }
            Operator::Reindent => {
                buf.begin_change(self.cursor);
                for line in start_line..=end_line {
                    self.reindent_line(line, buf);
                }
                self.cursor = first_non_blank(buf.text(), start_line);
                buf.end_change(self.cursor);
                self.done(buf, true);
            }
            Operator::Indent | Operator::Outdent => {
                let (first, last) = (start_line, end_line);
                buf.begin_change(self.cursor);
                for line in (first..=last).rev() {
                    let text = buf.text();
                    let start = line_start(text, line);
                    if op == Operator::Indent {
                        if line_len(text, line) > 0 {
                            let pad = " ".repeat(self.options.shiftwidth);
                            self.edit(buf, start..start, &pad);
                        }
                    } else {
                        let strip = outdent_width(text, line, self.options.shiftwidth);
                        self.edit(buf, start..start + strip, "");
                    }
                }
                self.cursor = first_non_blank(buf.text(), first);
                buf.end_change(self.cursor);
                self.done(buf, true);
            }
            Operator::Func(id) => {
                let Some(run) = crate::ext::operator(id) else {
                    self.done(buf, false);
                    return;
                };
                buf.begin_change(self.cursor);
                self.cursor = range.start;
                run(self, buf, range.clone(), linewise);
                self.cursor = clamp_normal(buf.text(), self.cursor);
                buf.end_change(self.cursor);
                self.done(buf, true);
            }
            Operator::Lower | Operator::Upper | Operator::Toggle => {
                let changed = match op {
                    Operator::Lower => original.to_lowercase(),
                    Operator::Upper => original.to_uppercase(),
                    _ => toggle_case(&original),
                };
                buf.begin_change(self.cursor);
                if changed != original {
                    self.edit(buf, range.clone(), &changed);
                }
                self.cursor = clamp_normal(buf.text(), range.start);
                buf.end_change(self.cursor);
                self.done(buf, true);
            }
        }
    }

    /// `p` and `P`.
    fn paste(&mut self, after: bool, buf: &mut Buffer) {
        self.paste_moving(after, false, buf);
    }

    /// `p` / `P`, or with `past` `gp` / `gP`: the cursor goes just after the
    /// put text (the line after put lines).
    fn paste_moving(&mut self, after: bool, past: bool, buf: &mut Buffer) {
        let count = self.count.take().unwrap_or(1);
        let Some(reg) = self.registers.get(self.register.take()) else {
            self.done(buf, false);
            return;
        };
        buf.begin_change(self.cursor);
        let end = self.put(&reg, after, buf, count);
        if past {
            let text = buf.text();
            self.cursor = end.min(text.len_chars());
            if reg.linewise {
                self.cursor = line_start(text, line_of(text, self.cursor));
            }
        }
        buf.end_change(self.cursor);
        self.done(buf, true);
    }

    /// `]p` / `[p`: puts the register's lines after (or before) this one,
    /// shifted together so the first gets the indent the syntax gives it
    /// there (without one, this line's, as Vim's `]p`); the lines keep
    /// their indent relative to each other. Text that isn't whole lines
    /// goes in as `p` puts it.
    fn paste_indented(&mut self, after: bool, buf: &mut Buffer) {
        let count = self.count.take().unwrap_or(1);
        let Some(reg) = self.registers.get(self.register.take()) else {
            self.done(buf, false);
            return;
        };
        buf.begin_change(self.cursor);
        let line = line_of(buf.text(), self.cursor);
        let here = indent_of(buf.text(), line);
        self.put(&reg, after, buf, count);
        if reg.linewise && !reg.block {
            let first = if after { line + 1 } else { line };
            let lines = reg.text.lines().count() * count;
            let want = self.syntax_indent(buf, first).unwrap_or(here);
            let ts = self.options.tabstop;
            let width = |s: &str| indent_width(s, ts);
            let shift = width(&want) as i64 - width(&indent_of(buf.text(), first)) as i64;
            if shift != 0 {
                for l in first..(first + lines).min(buf.text().len_lines()) {
                    let text = buf.text();
                    let lead = indent_of(text, l);
                    if line_len(text, l) == lead.chars().count() {
                        continue;
                    }
                    let cols = (width(&lead) as i64 + shift).max(0) as usize;
                    let start = line_start(text, l);
                    let new = self.indent_text(cols);
                    self.edit(buf, start..start + lead.chars().count(), &new);
                }
            }
            self.cursor = first_non_blank(buf.text(), first);
        }
        buf.end_change(self.cursor);
        self.done(buf, true);
    }

    /// `&` (`:s` again on this line) and `g&` (with its flags, on every line).
    fn repeat_substitute(&mut self, everywhere: bool, buf: &mut Buffer) -> Effect {
        self.count = None;
        let command = if everywhere { "%&&" } else { "&" };
        if let Err(err) = self.execute(command, buf) {
            self.cmd.message = Some(crate::ex::cmdline::Message {
                text: err.to_string(),
                error: true,
                line: None,
            });
        }
        self.done(buf, true);
        Effect::Handled
    }

    /// Puts `reg` `count` times; returns where the put text ends.
    pub(crate) fn put(
        &mut self,
        reg: &Register,
        after: bool,
        buf: &mut Buffer,
        count: usize,
    ) -> usize {
        if reg.block {
            self.put_block(reg, after, buf, count);
            return self.cursor;
        }
        let text = buf.text();
        let len = text.len_chars();
        let line = line_of(text, self.cursor);
        let insert = reg.text.repeat(count);
        if reg.linewise {
            let (at, insert, target_line) = if after {
                if line < last_line(text) {
                    (line_start(text, line + 1), insert, line + 1)
                } else if len > 0 && text.char(len - 1) == '\n' {
                    (len, insert, line + 1)
                } else {
                    let body = insert.strip_suffix('\n').unwrap_or(&insert);
                    (len, format!("\n{body}"), line + 1)
                }
            } else {
                (line_start(text, line), insert, line)
            };
            self.edit(buf, at..at, &insert);
            self.cursor = first_non_blank(buf.text(), target_line);
            at + insert.chars().count()
        } else {
            let at = if after && line_len(text, line) > 0 {
                (self.cursor + 1).min(line_end(text, line))
            } else {
                self.cursor
            };
            self.edit(buf, at..at, &insert);
            let end = at + insert.chars().count();
            self.cursor = clamp_normal(buf.text(), end.saturating_sub(1).max(at));
            end
        }
    }

    /// `.`: replays the keys of the last change, with a new count if given.
    fn repeat_last_change(&mut self, buf: &mut Buffer) {
        let count = self.count.take();
        let mut keys = match self.last_change.clone() {
            Some(LastChange::Keys(keys)) => keys,
            Some(LastChange::Ex(line)) => {
                self.keys.clear();
                self.run_mapped_command(&line, buf);
                return;
            }
            None => return,
        };
        if let Some(n) = count {
            let digits = keys
                .iter()
                .take_while(|k| matches!(k, Key::Char(c) if c.is_ascii_digit()))
                .count();
            keys.drain(..digits);
            let prefix = n.to_string().chars().map(Key::Char).collect::<Vec<_>>();
            keys.splice(0..0, prefix);
        }
        self.keys.clear();
        for key in keys {
            self.handle_key(key, buf);
        }
    }

    // ---- helpers ----------------------------------------------------------

    /// Applies an edit and keeps marks and line anchors pointing at the same
    /// text.
    pub(crate) fn edit(&mut self, buf: &mut Buffer, range: Range<usize>, text: &str) {
        let removed = range.len();
        let added = text.chars().count();
        // A rope clone is O(1); anchors need the text as it was.
        let before = (!self.anchors.is_empty()).then(|| buf.text().clone());
        if buf
            .edit(range.clone(), text, self.cursor, self.origin.clone())
            .is_err()
        {
            return;
        }
        for pos in self.marks.values_mut() {
            if *pos >= range.end {
                *pos = *pos - removed + added;
            } else if *pos > range.start {
                *pos = range.start;
            }
        }
        self.jumps.edited(range.clone(), added);
        if let Some(before) = before {
            self.anchors.apply_edit(&before, range, added);
        }
    }

    /// Ends the current command. A command that changed the text becomes the
    /// one `.` repeats (changes made from visual mode aren't repeatable).
    pub(crate) fn done(&mut self, buf: &Buffer, may_repeat: bool) {
        let visual = self.mode.is_visual() || self.visual_in_command;
        if may_repeat && buf.revision() != self.command_revision && !visual {
            self.last_change = Some(LastChange::Keys(self.keys.clone()));
        }
        self.keys.clear();
        self.visual_in_command = false;
        self.command_revision = buf.revision();
    }

    /// Makes `.` run `line` (for an extension command that changed the text,
    /// so it repeats like a built-in change).
    pub fn repeat_with(&mut self, line: &str) {
        self.last_change = Some(LastChange::Ex(line.to_owned()));
    }

    pub(crate) fn cancel(&mut self) {
        self.pending = Pending::None;
        self.op = None;
        self.count = None;
        self.register = None;
        self.keys.clear();
        self.visual_in_command = false;
    }

    pub(crate) fn take_count(&mut self) -> Option<usize> {
        let op_count = self.op.and_then(|p| p.count);
        combine(op_count, self.count.take())
    }

    fn visual_range(&self, text: &Rope) -> (Range<usize>, bool) {
        let (a, b) = (self.anchor.min(self.cursor), self.anchor.max(self.cursor));
        if self.mode == Mode::VisualLine {
            (line_range(text, line_of(text, a), line_of(text, b)), true)
        } else {
            (a..(b + 1).min(text.len_chars()), false)
        }
    }

    fn set_visual_marks(&mut self, text: &Rope) {
        let (a, b) = (self.anchor.min(self.cursor), self.anchor.max(self.cursor));
        self.marks.insert('<', a.min(text.len_chars()));
        self.marks.insert('>', b.min(text.len_chars()));
    }

    fn leave_visual(&mut self, text: &Rope) {
        self.set_visual_marks(text);
        let start = self.anchor.min(self.cursor);
        self.cursor = start;
        self.enter(Mode::Normal, text);
    }

    pub(crate) fn enter(&mut self, mode: Mode, text: &Rope) {
        self.maps.imap_pending.clear();
        if mode.is_visual() && !self.mode.is_visual() {
            self.anchor = self.cursor;
            self.visual_in_command = true;
        }
        if mode == Mode::Normal {
            self.cursor = clamp_normal(text, self.cursor);
        }
        self.mode = mode;
        self.pending = Pending::None;
        self.op = None;
        self.count = None;
        self.want_col = None;
    }
}

fn motion_for(c: char) -> Option<Motion> {
    Some(match c {
        'h' => Motion::Left,
        'l' => Motion::Right,
        'j' => Motion::Down,
        'k' => Motion::Up,
        'w' => Motion::WordForward { big: false },
        'W' => Motion::WordForward { big: true },
        'b' => Motion::WordBackward { big: false },
        'B' => Motion::WordBackward { big: true },
        'e' => Motion::WordEnd { big: false },
        'E' => Motion::WordEnd { big: true },
        '0' => Motion::LineStart,
        '^' => Motion::FirstNonBlank,
        '$' => Motion::LineEnd,
        'G' => Motion::LastLine,
        '%' => Motion::MatchingBracket,
        '}' => Motion::ParagraphForward,
        '{' => Motion::ParagraphBackward,
        ')' => Motion::SentenceForward,
        '(' => Motion::SentenceBackward,
        '+' => Motion::NextLineStart,
        '-' => Motion::PrevLineStart,
        '_' => Motion::LineStartBelow,
        _ => return None,
    })
}

/// `h` `j` `k` `l` that can't move (at an edge of the line or the note):
/// a failure, which stops a macro.
fn stuck(text: &Rope, motion: Motion, from: usize, target: Option<Target>) -> bool {
    match motion {
        Motion::Up | Motion::Down | Motion::NextLineStart | Motion::PrevLineStart => {
            target.is_none_or(|t| line_of(text, t.pos) == line_of(text, from))
        }
        Motion::Left | Motion::Right => target.is_none_or(|t| t.pos == from),
        _ => false,
    }
}

/// Motions that count as jumps: `''` returns to where they started.
fn is_jump(motion: Motion) -> bool {
    if let Motion::Custom(id) = motion {
        return crate::ext::motion(id).is_some_and(|(_, jump)| jump);
    }
    matches!(
        motion,
        Motion::FirstLine
            | Motion::LastLine
            | Motion::MatchingBracket
            | Motion::ParagraphForward
            | Motion::ParagraphBackward
            | Motion::SentenceForward
            | Motion::SentenceBackward
            | Motion::NextHeading
            | Motion::PrevHeading
    )
}

fn combine(a: Option<usize>, b: Option<usize>) -> Option<usize> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(1).saturating_mul(b.unwrap_or(1))),
    }
}

/// Whole lines `first..=last`, including the last line's newline.
pub(crate) fn line_range(text: &Rope, first: usize, last: usize) -> Range<usize> {
    let start = line_start(text, first);
    let end = if last < last_line(text) {
        line_start(text, last + 1)
    } else {
        text.len_chars()
    };
    start..end
}

/// The span an operator covers for a motion from `from` to `target`.
fn motion_range(text: &Rope, from: usize, target: Target) -> (Range<usize>, bool) {
    let len = text.len_chars();
    let (a, b) = (from.min(target.pos), from.max(target.pos));
    match target.kind {
        MotionKind::Linewise => (line_range(text, line_of(text, a), line_of(text, b)), true),
        MotionKind::Inclusive => (a..(b + 1).min(len), false),
        MotionKind::Exclusive => {
            let mut end = b;
            // An exclusive motion that ends at the start of a later line stops
            // at the end of the line before it (`dw` on a line's last word
            // keeps the newline).
            if end > a && column(text, end) == 0 && line_of(text, end) > line_of(text, a) {
                end = line_end(text, line_of(text, end) - 1);
            }
            (a..end.max(a), false)
        }
    }
}

fn ends_with_newline(text: &Rope, end: usize) -> bool {
    end > 0 && end <= text.len_chars() && text.char(end - 1) == '\n'
}

/// The leading spaces and tabs of `line`.
impl Vim {
    /// `gc` and `gb` on `range` (lines `first..=last`): the comment edits
    /// for the language there, one undo step, the cursor moving with its
    /// text. A language without comments says so.
    pub(crate) fn comment(
        &mut self,
        op: Operator,
        range: Range<usize>,
        linewise: bool,
        first: usize,
        last: usize,
        buf: &mut Buffer,
    ) {
        let at = first_non_blank(buf.text(), first);
        let (language, tokens) = crate::comment::tokens_at(buf, at.max(range.start));
        let edits = match (&tokens, op) {
            (Some(c), Operator::Comment) => {
                crate::comment::toggle_lines(buf.text(), first, last, c)
            }
            (Some(c), _) => match &c.block {
                Some((open, close)) => {
                    let range = if linewise {
                        line_start(buf.text(), first)..line_end(buf.text(), last)
                    } else {
                        range
                    };
                    crate::comment::toggle_block(buf.text(), range, linewise, open, close)
                }
                None => Vec::new(),
            },
            (None, _) => Vec::new(),
        };
        if edits.is_empty() {
            let what = language.as_deref().unwrap_or("plain text");
            let kind = if tokens.is_some() {
                "block comments"
            } else {
                "comments"
            };
            if tokens.is_none() || op == Operator::BlockComment {
                self.show_message(format!("{what} has no {kind}"));
            }
            self.done(buf, false);
            return;
        }
        buf.begin_change(self.cursor);
        let mut cursor = self.cursor;
        for (r, s) in edits {
            // Edits come last first; the cursor moves with those before it.
            if r.end <= cursor {
                cursor = cursor + s.chars().count() - r.len();
            } else if r.start < cursor {
                cursor = r.start;
            }
            self.edit(buf, r, &s);
        }
        self.cursor = clamp_normal(buf.text(), cursor);
        buf.end_change(self.cursor);
        self.done(buf, true);
    }

    /// `cols` columns of indent, with spaces, or with `noexpandtab` tabs
    /// and spaces for what's left.
    fn indent_text(&self, cols: usize) -> String {
        if self.options.expandtab {
            " ".repeat(cols)
        } else {
            let ts = self.options.tabstop.max(1);
            format!("{}{}", "\t".repeat(cols / ts), " ".repeat(cols % ts))
        }
    }

    /// The indent the syntax gives line `line` (AMI-234), as text: `None`
    /// when its language has no indent query or says to keep the indent
    /// (`@indent.auto`), and the caller does what Vim's `autoindent` does.
    fn syntax_indent(&self, buf: &mut Buffer, line: usize) -> Option<String> {
        use mnote_core::lang::indent::Indent;
        let snapshot = buf.syntax_snapshot()?;
        let sw = self.options.shiftwidth.max(1);
        match snapshot.indent(line, sw, self.options.tabstop.max(1))? {
            Indent::Columns(cols) => Some(self.indent_text(cols)),
            Indent::Keep => None,
        }
    }

    /// `=` on one line: its indent from the syntax. A blank line is
    /// emptied; a line whose language has no say keeps its indent.
    pub(crate) fn reindent_line(&mut self, line: usize, buf: &mut Buffer) {
        let text = buf.text();
        let start = line_start(text, line);
        let lead = indent_of(text, line).chars().count();
        if line_len(text, line) == lead {
            if lead > 0 {
                self.edit(buf, start..start + lead, "");
            }
            return;
        }
        if let Some(indent) = self.syntax_indent(buf, line)
            && indent != indent_of(buf.text(), line)
        {
            self.edit(buf, start..start + lead, &indent);
        }
    }

    /// While inserting, a closing bracket typed as a line's first character,
    /// a `:` (Python's `else:`), or `else` itself re-indents the line, as
    /// Vim's `indentkeys` (`0}`, `0)`, `0]`, `:`, `e`). Only brackets may
    /// move it in; the others only take it out.
    fn reindent_after_typing(&mut self, c: char, buf: &mut Buffer) {
        let text = buf.text();
        let line = line_of(text, self.cursor);
        let start = line_start(text, line);
        let before: String = text.slice(start..self.cursor).to_string();
        let typed = before.trim_start();
        let closer = matches!(typed, "}" | ")" | "]");
        let word = c == ':' || (c == 'e' && typed == "else");
        if !closer && !word {
            return;
        }
        let Some(indent) = self.syntax_indent(buf, line) else {
            return;
        };
        let lead = before.len() - typed.len();
        let old = &before[..lead];
        let ts = self.options.tabstop;
        let width = |s: &str| indent_width(s, ts);
        if indent == old || (!closer && width(&indent) >= width(old)) {
            return;
        }
        let lead_chars = old.chars().count();
        self.edit(buf, start..start + lead_chars, &indent);
        self.cursor = self.cursor - lead_chars + indent.chars().count();
    }
}

/// How many columns the indent `s` takes, with tabs `tabstop` wide.
fn indent_width(s: &str, tabstop: usize) -> usize {
    let ts = tabstop.max(1);
    s.chars()
        .fold(0, |w, c| if c == '\t' { w + ts - w % ts } else { w + 1 })
}

pub(crate) fn indent_of(text: &Rope, line: usize) -> String {
    let start = line_start(text, line);
    (0..line_len(text, line))
        .map(|i| text.char(start + i))
        .take_while(|c| matches!(c, ' ' | '\t'))
        .collect()
}

/// How many leading characters `<` removes: up to `width` spaces, or a tab.
fn outdent_width(text: &Rope, line: usize, width: usize) -> usize {
    let start = line_start(text, line);
    let len = line_len(text, line);
    if len > 0 && text.char(start) == '\t' {
        return 1;
    }
    (0..len.min(width))
        .take_while(|&i| text.char(start + i) == ' ')
        .count()
}

fn char_at_is_word(text: &Rope, pos: usize) -> bool {
    pos < text.len_chars() && !text.char(pos).is_whitespace()
}

/// End (inclusive) of `cw`: the rest of the current word, then `count - 1`
/// more words.
fn change_word_end(text: &Rope, pos: usize, count: usize, big: bool) -> usize {
    let class = |c: char| {
        if c.is_whitespace() {
            0
        } else if big || c.is_alphanumeric() || c == '_' {
            1
        } else {
            2
        }
    };
    let len = text.len_chars();
    let mut end = pos;
    for i in 0..count {
        if i > 0 {
            end += 1;
            while end < len && text.char(end).is_whitespace() && text.char(end) != '\n' {
                end += 1;
            }
            if end >= len {
                return len - 1;
            }
        }
        let cls = class(text.char(end));
        while end + 1 < len && class(text.char(end + 1)) == cls && text.char(end + 1) != '\n' {
            end += 1;
        }
    }
    end
}

/// Swaps upper and lower case (`~`, `g~`).
fn toggle_case(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            let swapped: Vec<char> = if c.is_uppercase() {
                c.to_lowercase().collect()
            } else {
                c.to_uppercase().collect()
            };
            swapped
        })
        .collect()
}
