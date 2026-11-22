//! [`Vim`]: the mode state machine. It reads keys and applies them to the
//! note's [`Buffer`]: moving the cursor, editing through operators, keeping
//! registers and marks, and grouping each command into one undo step.

use std::collections::HashMap;
use std::ops::Range;

use mnote_core::Buffer;
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
}

impl Operator {
    fn from_key(c: char) -> Option<Self> {
        Some(match c {
            'd' => Operator::Delete,
            'c' => Operator::Change,
            'y' => Operator::Yank,
            '>' => Operator::Indent,
            '<' => Operator::Outdent,
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
}

#[derive(Debug, Clone, Copy)]
struct OpPending {
    op: Operator,
    count: Option<usize>,
}

/// An insert-mode session, for `3ix<esc>` style repeats.
#[derive(Debug, Clone, Default)]
struct InsertSession {
    count: usize,
    /// What each repeat inserts before the typed text (`\n` after `o`).
    prefix: String,
    typed: String,
    /// The typed text can't be repeated reliably (backspaced past it).
    broken: bool,
}

/// Vim's state for one editor or text input.
pub struct Vim {
    pub(crate) mode: Mode,
    pub(crate) cursor: usize,
    anchor: usize,
    want_col: Option<usize>,
    count: Option<usize>,
    pending: Pending,
    op: Option<OpPending>,
    pub(crate) register: Option<char>,
    last_find: Option<Find>,
    pub(crate) registers: Registers,
    pub(crate) marks: HashMap<char, usize>,
    pub(crate) options: crate::ex::Options,
    /// The last search pattern (`/`, `?`, `:s`, `:g`), for `n` and `:s//`.
    pub(crate) last_search: Option<String>,
    /// Whether matches of the last search are highlighted (`:noh` clears).
    pub(crate) search_highlight: bool,
    /// The last `:s` (pattern, replacement, flags), for `:s` and `:&`.
    pub(crate) last_substitute: Option<(String, String, String)>,
    /// Line anchors `:g` and `:normal` walk; edits keep them on their lines,
    /// and a deleted line's anchor becomes `None`.
    pub(crate) anchors: crate::anchors::Anchors,
    /// Insert-mode mappings (`:imap`): keys typed, keys they become. `jk`
    /// to `<Esc>` is there by default.
    pub(crate) imaps: Vec<(Vec<Key>, Vec<Key>)>,
    /// Keys typed in insert mode that start a mapping; they're already in
    /// the text and come out again if the mapping completes.
    imap_pending: Vec<Key>,
    /// Feeding a mapping's keys: they aren't mapped again.
    in_mapping: bool,
    /// The last search compiled, for `n`, `N` and highlighting.
    pub(crate) search_pattern: Option<crate::ex::pattern::Pattern>,
    /// Direction of the last `/` (true) or `?` search.
    pub(crate) search_forward: bool,
    /// The line being typed in command mode, and its live preview.
    pub(crate) cmdline: Option<crate::ex::cmdline::CommandLine>,
    pub(crate) preview: Option<crate::ex::cmdline::Preview>,
    /// The last command's message, until the next key.
    pub(crate) message: Option<crate::ex::cmdline::Message>,
    /// An app command from the last command line, until the app takes it.
    pub(crate) app_command: Option<crate::ex::AppCommand>,
    /// Lines run from the command line, oldest first.
    pub(crate) ex_history: Vec<String>,
    pub(crate) search_history: Vec<String>,
    pub(crate) completer: Option<Box<dyn crate::ex::cmdline::Completer>>,
    pub(crate) cmdwin: Option<Box<crate::ex::cmdwin::CmdWindow>>,
    insert: Option<InsertSession>,
    /// Keys of the command being typed, and of the last change, for `.`.
    keys: Vec<Key>,
    last_change: Option<Vec<Key>>,
    /// Buffer revision when the current command started.
    command_revision: u64,
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
            last_search: None,
            search_highlight: false,
            last_substitute: None,
            anchors: crate::anchors::Anchors::default(),
            imaps: vec![(vec![Key::Char('j'), Key::Char('k')], vec![Key::Esc])],
            imap_pending: Vec::new(),
            in_mapping: false,
            search_pattern: None,
            search_forward: true,
            cmdline: None,
            preview: None,
            message: None,
            app_command: None,
            ex_history: Vec::new(),
            search_history: Vec::new(),
            completer: None,
            cmdwin: None,
            insert: None,
            keys: Vec::new(),
            last_change: None,
            command_revision: 0,
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

    /// Moves the cursor, for example after a mouse click.
    /// Replaces `range` with `text` as one undo step, keeping marks in
    /// place (for edits the app makes, such as toggling a task).
    pub fn replace(&mut self, buf: &mut Buffer, range: Range<usize>, text: &str) {
        buf.begin_change(self.cursor);
        self.edit(buf, range, text);
        buf.end_change(self.cursor);
    }

    pub fn set_cursor(&mut self, pos: usize) {
        self.cursor = pos;
        self.want_col = None;
    }

    /// Whether a key sequence is half typed (shown in the status line).
    pub fn is_pending(&self) -> bool {
        self.pending != Pending::None
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
        if self.mode != Mode::Insert && self.is_idle() {
            self.keys.clear();
            self.command_revision = buf.revision();
        }
        self.keys.push(key);
        self.message = None;
        if self.cmdwin.is_some() {
            self.cmd_window_key(key, buf);
            return Effect::Handled;
        }
        match self.mode {
            Mode::Insert => self.insert_mapped(key, buf),
            Mode::Command => {
                self.command_key(key, buf);
                Effect::Handled
            }
            Mode::Normal | Mode::Visual | Mode::VisualLine => self.normal_key(key, buf),
        }
    }

    fn is_idle(&self) -> bool {
        !self.is_pending()
    }

    // ---- insert mode ------------------------------------------------------

    /// An insert-mode key, through the `:imap` mappings. A key that may
    /// start a mapping is typed as usual; when the mapping completes, those
    /// keys come out again and the mapping's keys run instead (so `jk`
    /// leaves insert mode with no `j` left behind).
    fn insert_mapped(&mut self, key: Key, buf: &mut Buffer) -> Effect {
        if self.in_mapping || self.imaps.is_empty() {
            return self.insert_key(key, buf);
        }
        let mut typed = std::mem::take(&mut self.imap_pending);
        typed.push(key);
        if let Some(rhs) = self
            .imaps
            .iter()
            .find(|(lhs, _)| *lhs == typed)
            .map(|(_, rhs)| rhs.clone())
        {
            for _ in 1..typed.len() {
                self.insert_key(Key::Backspace, buf);
            }
            self.in_mapping = true;
            let mut effect = Effect::Handled;
            for key in rhs {
                effect = self.handle_key(key, buf);
            }
            self.in_mapping = false;
            return effect;
        }
        let prefix = |keys: &[Key]| {
            self.imaps
                .iter()
                .any(|(lhs, _)| lhs.len() > keys.len() && lhs.starts_with(keys))
        };
        if matches!(key, Key::Char(_)) && prefix(&typed) {
            self.imap_pending = typed;
            return self.insert_key(key, buf);
        }
        if typed.len() > 1 {
            // The earlier keys didn't become a mapping; this one may start one.
            return self.insert_mapped(key, buf);
        }
        self.insert_key(key, buf)
    }

    fn insert_key(&mut self, key: Key, buf: &mut Buffer) -> Effect {
        match key {
            Key::Esc => {
                if let Some(session) = self.insert.take()
                    && session.count > 1
                    && !session.broken
                {
                    let unit = format!("{}{}", session.prefix, session.typed);
                    let repeat = unit.repeat(session.count - 1);
                    self.insert_text(buf, &repeat);
                }
                let start = line_start(buf.text(), line_of(buf.text(), self.cursor));
                if self.cursor > start {
                    self.cursor -= 1;
                }
                buf.end_change(self.cursor);
                self.enter(Mode::Normal, buf.text());
                self.done(buf, true);
            }
            Key::Char(c) => self.typed(buf, &c.to_string()),
            Key::Tab => self.typed(buf, "\t"),
            Key::Enter => {
                let indent = indent_of(buf.text(), line_of(buf.text(), self.cursor));
                self.insert_text(buf, &format!("\n{indent}"));
                if let Some(s) = &mut self.insert {
                    s.typed.push('\n');
                }
            }
            Key::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.edit(buf, self.cursor..self.cursor + 1, "");
                    if let Some(s) = &mut self.insert
                        && s.typed.pop().is_none()
                    {
                        s.broken = true;
                    }
                }
            }
            Key::Ctrl(_) | Key::BackTab | Key::Up | Key::Down | Key::Left | Key::Right => {
                return Effect::Unhandled(key);
            }
        }
        Effect::Handled
    }

    fn typed(&mut self, buf: &mut Buffer, s: &str) {
        self.insert_text(buf, s);
        if let Some(session) = &mut self.insert {
            session.typed.push_str(s);
        }
    }

    fn insert_text(&mut self, buf: &mut Buffer, s: &str) {
        self.edit(buf, self.cursor..self.cursor, s);
        self.cursor += s.chars().count();
    }

    /// Starts insert mode at the cursor as part of the current undo step.
    fn start_insert(&mut self, buf: &mut Buffer, count: usize, prefix: &str) {
        self.insert = Some(InsertSession {
            count: count.max(1),
            prefix: prefix.to_owned(),
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
            (Pending::G, Key::Char('g')) => Motion::FirstLine,
            (Pending::G, Key::Char(c @ ('u' | 'U'))) => {
                let op = if c == 'u' {
                    Operator::Lower
                } else {
                    Operator::Upper
                };
                return self.operator_key(op, buf);
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
        let c = match key {
            Key::Char(c) => c,
            // Arrows move like `h` `j` `k` `l`.
            Key::Left => 'h',
            Key::Down => 'j',
            Key::Up => 'k',
            Key::Right => 'l',
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

        if let Some(motion) = motion_for(c) {
            return self.run_motion(motion, buf);
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
            // Macros aren't recorded yet; `q:` opens the command-line window.
            'q' if self.op.is_none() && !self.mode.is_visual() => Some(Pending::Q),
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
                let indent = indent_of(text, line);
                let (start, end) = (line_start(text, line), line_end(text, line));
                buf.begin_change(self.cursor);
                if c == 'o' {
                    self.cursor = end;
                    self.insert_text(buf, &format!("\n{indent}"));
                } else {
                    self.edit(buf, start..start, &format!("{indent}\n"));
                    self.cursor = start + indent.chars().count();
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
            _ => None,
        };
        if let Some(op) = op {
            let (range, linewise) = self.visual_range(text);
            self.leave_visual(text);
            self.apply_operator(op, range, linewise, buf);
            return Effect::Handled;
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
                    self.put(&reg, false, buf, 1);
                    self.registers.delete(
                        None,
                        Register {
                            text: removed,
                            linewise,
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
            let target = motion.apply(text, self.cursor, count, None, past_end);
            return self.arrive(target, is_jump(motion), buf);
        }
        let past_end = self.mode != Mode::Normal;
        let target = motion.apply(text, self.cursor, count, self.want_col, past_end);
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
            self.marks.insert('\'', self.cursor);
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
        let text = buf.text();
        let Some(sel) = textobj::select(text, self.cursor, obj, around, count) else {
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
            Operator::Lower | Operator::Upper => {
                let changed = if op == Operator::Lower {
                    original.to_lowercase()
                } else {
                    original.to_uppercase()
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
        let count = self.count.take().unwrap_or(1);
        let Some(reg) = self.registers.get(self.register.take()) else {
            self.done(buf, false);
            return;
        };
        buf.begin_change(self.cursor);
        self.put(&reg, after, buf, count);
        buf.end_change(self.cursor);
        self.done(buf, true);
    }

    pub(crate) fn put(&mut self, reg: &Register, after: bool, buf: &mut Buffer, count: usize) {
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
        } else {
            let at = if after && line_len(text, line) > 0 {
                (self.cursor + 1).min(line_end(text, line))
            } else {
                self.cursor
            };
            self.edit(buf, at..at, &insert);
            let end = at + insert.chars().count();
            self.cursor = clamp_normal(buf.text(), end.saturating_sub(1).max(at));
        }
    }

    /// `.`: replays the keys of the last change, with a new count if given.
    fn repeat_last_change(&mut self, buf: &mut Buffer) {
        let count = self.count.take();
        let Some(mut keys) = self.last_change.clone() else {
            return;
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
        if buf.edit(range.clone(), text, self.cursor).is_err() {
            return;
        }
        for pos in self.marks.values_mut() {
            if *pos >= range.end {
                *pos = *pos - removed + added;
            } else if *pos > range.start {
                *pos = range.start;
            }
        }
        if let Some(before) = before {
            self.anchors.apply_edit(&before, range, added);
        }
    }

    /// Ends the current command. A command that changed the text becomes the
    /// one `.` repeats (changes made from visual mode aren't repeatable).
    pub(crate) fn done(&mut self, buf: &Buffer, may_repeat: bool) {
        if may_repeat && buf.revision() != self.command_revision && !self.keys_start_visual() {
            self.last_change = Some(self.keys.clone());
        }
        self.keys.clear();
        self.command_revision = buf.revision();
    }

    fn keys_start_visual(&self) -> bool {
        self.mode.is_visual() || self.keys.iter().any(|k| matches!(k, Key::Char('v' | 'V')))
    }

    fn cancel(&mut self) {
        self.pending = Pending::None;
        self.op = None;
        self.count = None;
        self.register = None;
        self.keys.clear();
    }

    fn take_count(&mut self) -> Option<usize> {
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
        self.imap_pending.clear();
        if mode.is_visual() && !self.mode.is_visual() {
            self.anchor = self.cursor;
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
        _ => return None,
    })
}

/// Motions that count as jumps: `''` returns to where they started.
fn is_jump(motion: Motion) -> bool {
    matches!(
        motion,
        Motion::FirstLine
            | Motion::LastLine
            | Motion::MatchingBracket
            | Motion::ParagraphForward
            | Motion::ParagraphBackward
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
