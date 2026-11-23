//! The command line: `:` ex commands and `/` `?` searches, typed in Vim's
//! command mode, with a live preview of what they'll match or replace.
//!
//! [`Vim`] keeps the line's text; the app draws it from
//! [`Vim::command_line`], colours it with [`tokens`] and paints
//! [`Vim::preview`] into the note. The preview never touches the buffer, so
//! `esc` simply drops it.

use std::borrow::Cow;
use std::ops::Range;

use mnote_core::Buffer;
use ropey::Rope;

use super::complete::{self, Completion};
use super::parse::{self, split_delimited};
use super::pattern::{Pattern, Replacement};
use super::{AppCommand, DefaultRange, ExError, plan_substitute, plural};
use crate::state::Vim;
use crate::text::{last_line, line_of, line_start};
use crate::{Key, Mode, MotionKind, Target};

/// Which kind of line is being typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    /// `:`
    Ex,
    /// `/` (forward) or `?` (backward).
    Search { forward: bool },
}

impl CommandKind {
    /// The character shown before the text.
    pub fn prefix(self) -> char {
        match self {
            CommandKind::Ex => ':',
            CommandKind::Search { forward: true } => '/',
            CommandKind::Search { forward: false } => '?',
        }
    }
}

/// The line being typed after `:`, `/` or `?`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLine {
    pub kind: CommandKind,
    pub text: String,
    /// Byte offset of the caret in `text`.
    pub cursor: usize,
    /// The `⇥` popup, while open.
    pub completion: Option<Completion>,
    /// Walking the history with `↑`/`↓` (or `⌃p`/`⌃n`).
    history: Option<HistoryWalk>,
    /// `⌃r` was typed; the next key names the register to paste.
    register_pending: bool,
}

/// Where a walk through the history is.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HistoryWalk {
    /// What was typed before the walk started; `↑` only recalls entries that
    /// start with it.
    typed: String,
    /// Whether to filter by `typed` (`↑`/`↓`) or not (`⌃p`/`⌃n`).
    filter: bool,
    /// The entry shown, or the history's length for `typed` itself.
    index: usize,
}

impl CommandLine {
    pub(crate) fn new(kind: CommandKind, text: &str) -> Self {
        CommandLine {
            kind,
            text: text.to_owned(),
            cursor: text.len(),
            completion: None,
            history: None,
            register_pending: false,
        }
    }

    fn set_text(&mut self, text: String) {
        self.cursor = text.len();
        self.text = text;
    }

    fn insert(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    fn prev_char(&self) -> Option<usize> {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
    }

    fn next_char(&self) -> Option<usize> {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
    }
}

/// Entries kept per history (`:` and search), as Vim's default `history`.
const HISTORY_LEN: usize = 100;

/// Where the command line gets note paths for `⇥` after `:e`, `:w`, `:find`
/// and `:explore`. The app plugs in the vault; it's only asked on `⇥`.
pub trait Completer {
    /// Vault-relative note paths with `.md` (`projects/mnote/ui-design.md`).
    fn paths(&mut self) -> Vec<String>;
}

/// What the last command line or search said: a result such as `5
/// substitutions on 5 lines`, or an error such as `E486: Pattern not found`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub text: String,
    pub error: bool,
    /// The line that produced it, shown beside it until the next key.
    pub line: Option<CommandLine>,
}

/// The parts of a command line, for syntax colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    /// `%`, `'<,'>`, `.,+5`
    Range,
    /// `s`, `g!`, `sort`
    Command,
    /// The delimiters of `:s/a/b/` and `:g/a/`.
    Separator,
    Pattern,
    Replacement,
    /// `:s` flags, or a search offset.
    Flags,
    /// Anything else.
    Text,
}

/// What the line being typed would do, drawn live in the note.
#[derive(Debug, Clone, Default)]
pub struct Preview {
    /// Matches of this pattern on `lines` get the `match` fill.
    pub pattern: Option<Pattern>,
    /// First and last line (0-based) the pattern applies to.
    pub lines: (usize, usize),
    /// The match the command acts on first: the `cursor` fill for a search,
    /// the outlined replacement for `:s`.
    pub current: Option<Range<usize>>,
    /// For `:s` with a replacement typed: each match and its replacement, in
    /// document order. These replace the pattern highlights.
    pub substitutions: Vec<(Range<usize>, String)>,
    /// The live count for the right end of the line.
    pub info: Option<String>,
}

impl Preview {
    /// Whether line `line` gets pattern highlights.
    pub fn covers(&self, line: usize) -> bool {
        self.pattern.is_some() && (self.lines.0..=self.lines.1).contains(&line)
    }

    /// The substitutions that start in `chars` (a line's character range).
    pub fn substitutions_in(&self, chars: Range<usize>) -> &[(Range<usize>, String)] {
        let from = self
            .substitutions
            .partition_point(|(r, _)| r.start < chars.start);
        let to = self.substitutions.partition_point(|(r, _)| {
            r.start < chars.end || (r.start == chars.end && r.is_empty())
        });
        &self.substitutions[from..to.max(from)]
    }
}

impl Vim {
    /// The line being typed, while in command mode.
    pub fn command_line(&self) -> Option<&CommandLine> {
        self.cmdline.as_ref()
    }

    /// The live preview of the line being typed.
    pub fn preview(&self) -> Option<&Preview> {
        self.preview.as_ref()
    }

    /// The message from the last command, shown until the next key.
    pub fn message(&self) -> Option<&Message> {
        self.message.as_ref()
    }

    /// A command the app should carry out (`:w`, `:q`, …), once.
    pub fn take_app_command(&mut self) -> Option<AppCommand> {
        self.app_command.take()
    }

    /// The pattern to highlight in the note after a search, until `:noh`.
    pub fn highlight_pattern(&self) -> Option<&Pattern> {
        self.search_highlight
            .then_some(self.search_pattern.as_ref())
            .flatten()
    }

    /// Remembers `pattern` for `n`, `N`, `:s//` and highlighting.
    pub(crate) fn remember_search(&mut self, pattern: &Pattern) {
        self.last_search = Some(pattern.source.clone());
        self.search_pattern = Some(pattern.clone());
        self.search_highlight = true;
    }

    /// Compiles the last search again after `ignorecase` or `smartcase`
    /// changed.
    pub(crate) fn recompile_search(&mut self) {
        if let Some(source) = self.last_search.clone() {
            self.search_pattern = self.compile(&source).ok();
        }
    }

    /// Opens the command line with `text` already typed (`'<,'>` from visual
    /// mode).
    pub(crate) fn open_command_line(&mut self, kind: CommandKind, text: &str, buf: &Buffer) {
        self.enter(Mode::Command, buf.text());
        self.cmdline = Some(CommandLine::new(kind, text));
        self.update_preview(buf);
    }

    /// Plugs in note paths for completion.
    pub fn set_completer(&mut self, completer: Box<dyn Completer>) {
        self.completer = Some(completer);
    }

    /// The `:` or search history, oldest first.
    pub fn history(&self, kind: CommandKind) -> &[String] {
        match kind {
            CommandKind::Ex => &self.ex_history,
            CommandKind::Search { .. } => &self.search_history,
        }
    }

    fn remember_line(&mut self, kind: CommandKind, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let history = match kind {
            CommandKind::Ex => &mut self.ex_history,
            CommandKind::Search { .. } => &mut self.search_history,
        };
        history.retain(|h| h != text);
        history.push(text.to_owned());
        if history.len() > HISTORY_LEN {
            history.remove(0);
        }
    }

    /// A key typed in command mode.
    pub(crate) fn command_key(&mut self, key: Key, buf: &mut Buffer) {
        let Some(mut line) = self.cmdline.take() else {
            self.enter(Mode::Normal, buf.text());
            return;
        };

        if std::mem::take(&mut line.register_pending) {
            if let Some(text) = self.register_text(key, buf) {
                line.insert(&text);
            }
            return self.edited(line, buf);
        }

        // The popup takes ⇥, ⌃n and ⌃p; any other key keeps the choice.
        if line.completion.is_some() {
            match key {
                Key::Tab | Key::Ctrl('n') => return self.cycle(line, true, buf),
                Key::BackTab | Key::Ctrl('p') => return self.cycle(line, false, buf),
                _ => line.completion = None,
            }
        }
        match key {
            Key::Up | Key::Down | Key::Ctrl('p') | Key::Ctrl('n') => {
                let older = matches!(key, Key::Up | Key::Ctrl('p'));
                self.walk_history(&mut line, older, matches!(key, Key::Up | Key::Down));
                return self.edited(line, buf);
            }
            _ => line.history = None,
        }

        match key {
            Key::Esc | Key::Ctrl('c') => return self.close_command_line(buf),
            Key::Enter => return self.run_line(line, buf),
            Key::Ctrl('f') => {
                self.close_command_line(buf);
                return self.open_cmd_window(line.kind, &line.text, buf);
            }
            Key::Backspace | Key::Ctrl('h') => {
                // Backspace on an empty line leaves, as in Vim.
                if line.text.is_empty() {
                    return self.close_command_line(buf);
                }
                if let Some(at) = line.prev_char() {
                    line.text.replace_range(at..line.cursor, "");
                    line.cursor = at;
                }
            }
            Key::Ctrl('w') => {
                // Deletes the word before the caret, or one punctuation mark.
                let is_word = |c: char| c.is_alphanumeric() || c == '_';
                let before = line.text[..line.cursor].trim_end();
                let cut = match before.chars().next_back() {
                    None => 0,
                    Some(c) if is_word(c) => before.trim_end_matches(is_word).len(),
                    Some(c) => before.len() - c.len_utf8(),
                };
                line.text.replace_range(cut..line.cursor, "");
                line.cursor = cut;
            }
            Key::Ctrl('u') => {
                line.text.replace_range(..line.cursor, "");
                line.cursor = 0;
            }
            Key::Left => line.cursor = line.prev_char().unwrap_or(0),
            Key::Right => line.cursor = line.next_char().unwrap_or(line.cursor),
            Key::Ctrl('b') | Key::Home => line.cursor = 0,
            Key::Ctrl('e') | Key::End => line.cursor = line.text.len(),
            Key::Ctrl('r') => line.register_pending = true,
            Key::Tab if line.kind == CommandKind::Ex => return self.complete(line, buf),
            Key::Char(c) => line.insert(c.encode_utf8(&mut [0; 4])),
            _ => {}
        }
        self.edited(line, buf);
    }

    /// `⌘V` on the command line: the text at the caret, on one line.
    pub(crate) fn paste_into_command_line(&mut self, text: &str, buf: &mut Buffer) -> bool {
        let Some(mut line) = self.cmdline.take() else {
            return false;
        };
        line.insert(&text.replace(['\r', '\n'], " "));
        self.edited(line, buf);
        true
    }

    /// Puts the line back after an edit and refreshes the preview.
    fn edited(&mut self, line: CommandLine, buf: &Buffer) {
        self.cmdline = Some(line);
        self.update_preview(buf);
    }

    /// Whether `⌃r` is waiting for a register name, so the app can read the
    /// system clipboard first if it's `+` or `*`.
    pub fn awaits_register(&self) -> bool {
        self.cmdline.as_ref().is_some_and(|l| l.register_pending)
    }

    /// What `⌃r {key}` pastes: a register, `:` the last command, `/` the
    /// last search, or with `⌃w` the word under the cursor in the note. Line
    /// breaks become spaces; the line holds one line.
    fn register_text(&mut self, key: Key, buf: &Buffer) -> Option<String> {
        let text = match key {
            Key::Ctrl('w') => word_under(buf.text(), self.cursor)?,
            Key::Char(':') => self.ex_history.last()?.clone(),
            Key::Char('/') => self.last_search.clone()?,
            Key::Char(c) if crate::registers::is_register_name(c) => {
                self.registers.get(Some(c))?.text
            }
            _ => return None,
        };
        Some(text.trim_end_matches('\n').replace('\n', " "))
    }

    /// `↑`/`⌃p` (older) and `↓`/`⌃n` (newer). With `filter`, only entries
    /// starting with what was typed come up.
    fn walk_history(&self, line: &mut CommandLine, older: bool, filter: bool) {
        let history = self.history(line.kind);
        let walk = line.history.get_or_insert_with(|| HistoryWalk {
            typed: line.text.clone(),
            filter,
            index: history.len(),
        });
        walk.filter = filter;
        let matches = |i: &usize| !walk.filter || history[*i].starts_with(&walk.typed);
        let next = if older {
            (0..walk.index).rev().find(matches)
        } else {
            (walk.index + 1..history.len()).find(matches)
        };
        match next {
            Some(i) => {
                walk.index = i;
                let text = history[i].clone();
                line.set_text(text);
            }
            // Past the newest entry: back to what was typed.
            None if !older => {
                walk.index = history.len();
                let text = walk.typed.clone();
                line.set_text(text);
            }
            None => {}
        }
    }

    /// The first `⇥`: completes the word before the caret. One candidate is
    /// filled in; several open the popup with the first one chosen.
    fn complete(&mut self, mut line: CommandLine, buf: &Buffer) {
        let mut completer = self.completer.take();
        let found = complete::candidates(&line.text, line.cursor, &self.options, || {
            completer.as_mut().map(|c| c.paths()).unwrap_or_default()
        });
        self.completer = completer;
        if let Some((start, items)) = found.filter(|(_, items)| !items.is_empty()) {
            let original = line.text[start..line.cursor].to_owned();
            if items.len() == 1 {
                line.text.replace_range(start..line.cursor, &items[0].text);
                line.cursor = start + items[0].text.len();
            } else {
                line.completion = Some(Completion {
                    start,
                    original,
                    items,
                    selected: None,
                });
                return self.cycle(line, true, buf);
            }
        }
        self.edited(line, buf);
    }

    /// `⇥`/`⌃n` and `⇧⇥`/`⌃p` in the popup: the next or previous candidate,
    /// and after the last one the text as typed.
    fn cycle(&mut self, mut line: CommandLine, forward: bool, buf: &Buffer) {
        let Some(c) = line.completion.as_mut() else {
            return self.edited(line, buf);
        };
        let n = c.items.len();
        c.selected = match (c.selected, forward) {
            (None, true) => Some(0),
            (None, false) => Some(n - 1),
            (Some(i), true) => (i + 1 < n).then_some(i + 1),
            (Some(i), false) => i.checked_sub(1),
        };
        let fill = c
            .selected
            .map_or(c.original.as_str(), |i| c.items[i].text.as_str());
        let fill = fill.to_owned();
        let start = c.start;
        line.text.replace_range(start..line.cursor, &fill);
        line.cursor = start + fill.len();
        self.edited(line, buf);
    }

    fn close_command_line(&mut self, buf: &Buffer) {
        self.cmdline = None;
        self.preview = None;
        self.enter(Mode::Normal, buf.text());
    }

    /// `enter`: runs the line (typed, or picked in the command-line window).
    pub(crate) fn run_line(&mut self, mut line: CommandLine, buf: &mut Buffer) {
        self.cmdline = None;
        self.preview = None;
        self.enter(Mode::Normal, buf.text());
        line.completion = None;
        line.history = None;
        line.cursor = line.text.len();
        self.remember_line(line.kind, &line.text);
        let result = match line.kind {
            CommandKind::Ex => self.execute(&line.text, buf).map(|outcome| {
                self.app_command = outcome.app;
                outcome.message
            }),
            CommandKind::Search { forward } => self.search(&line.text, forward, buf),
        };
        self.message = match result {
            Ok(Some(text)) => Some(Message {
                text,
                error: false,
                line: Some(line),
            }),
            Ok(None) => None,
            Err(err) => Some(Message {
                text: err.to_string(),
                error: true,
                line: Some(line),
            }),
        };
    }

    /// `/pattern` or `?pattern`: jumps to the next match and highlights all
    /// of them. An empty pattern repeats the last search.
    fn search(
        &mut self,
        input: &str,
        forward: bool,
        buf: &mut Buffer,
    ) -> Result<Option<String>, ExError> {
        let delim = if forward { '/' } else { '?' };
        let (source, _offset) = split_delimited(input, delim);
        let pattern = self.compile(&source)?;
        self.remember_search(&pattern);
        self.search_forward = forward;
        self.jump_to_match(1, false, buf)
    }

    /// `n` (or `N` with `reverse`): the next match of the last search.
    pub(crate) fn search_next(&mut self, reverse: bool, count: usize, buf: &mut Buffer) {
        match self.jump_to_match(count, reverse, buf) {
            Ok(text) => {
                self.message = text.map(|text| Message {
                    text,
                    error: false,
                    line: None,
                })
            }
            Err(err) => {
                self.message = Some(Message {
                    text: err.to_string(),
                    error: true,
                    line: None,
                })
            }
        }
    }

    /// Moves to the `count`th match of the last search, in its direction (or
    /// against it with `reverse`). An operator pending applies up to it.
    fn jump_to_match(
        &mut self,
        count: usize,
        reverse: bool,
        buf: &mut Buffer,
    ) -> Result<Option<String>, ExError> {
        let Some(pattern) = self.search_pattern.clone() else {
            return Err(ExError::new(35, "No previous regular expression"));
        };
        self.search_highlight = true;
        let forward = self.search_forward != reverse;
        let text = buf.text();
        let mut pos = self.cursor;
        let mut wrapped = false;
        for _ in 0..count.max(1) {
            let (found, wrap) = find_next(text, &pattern, pos, forward)
                .ok_or_else(|| ExError::pattern_not_found(&pattern.source))?;
            pos = found.start;
            wrapped |= wrap;
        }
        let target = Target {
            pos,
            kind: MotionKind::Exclusive,
            keep_column: false,
        };
        self.arrive_at(target, buf);
        Ok(wrapped.then(|| {
            if forward {
                "search hit BOTTOM, continuing at TOP".to_owned()
            } else {
                "search hit TOP, continuing at BOTTOM".to_owned()
            }
        }))
    }

    /// Works out the preview for the line being typed.
    fn update_preview(&mut self, buf: &Buffer) {
        self.preview = None;
        let Some(line) = self.cmdline.clone() else {
            return;
        };
        let text = buf.text();
        self.preview = match line.kind {
            CommandKind::Search { forward } => {
                let (source, _) = split_delimited(&line.text, line.kind.prefix());
                if source.is_empty() {
                    return;
                }
                let Ok(pattern) = self.compile(&source) else {
                    return;
                };
                let last = last_line(text);
                let (info, any) = match_count(text, &pattern, 0, last);
                // With no match anywhere, don't walk the note again.
                let current = any
                    .then(|| find_next(text, &pattern, self.cursor, forward))
                    .flatten()
                    .map(|(r, _)| r);
                Some(Preview {
                    pattern: Some(pattern),
                    lines: (0, last),
                    current,
                    substitutions: Vec::new(),
                    info: Some(info),
                })
            }
            CommandKind::Ex => self.ex_preview(&line.text, text),
        };
    }

    /// The preview of `:s/pat/rep/` and `:g/pat/`.
    fn ex_preview(&mut self, input: &str, text: &Rope) -> Option<Preview> {
        let cmd = parse::parse(input.trim_start().trim_start_matches(':')).ok()?;
        let (substitute, default) = match cmd.name {
            "substitute" => (true, DefaultRange::Current),
            "global" | "vglobal" => (false, DefaultRange::Whole),
            _ => return None,
        };
        let delim = cmd.args.chars().next().filter(|&c| is_delimiter(c))?;
        let (source, rest) = split_found(&cmd.args[delim.len_utf8()..], delim);
        if source.is_empty() && self.last_search.is_none() {
            return None;
        }

        // Resolving the range may run a search address; that mustn't count
        // as a search yet.
        let saved = (
            self.last_search.clone(),
            self.search_pattern.clone(),
            self.search_highlight,
        );
        let range = self.lines(&cmd, text, default);
        (self.last_search, self.search_pattern, self.search_highlight) = saved;
        let (first, last) = range.ok()?;

        let mut pattern = self.compile(&source).ok()?;
        let replacement = rest.filter(|_| substitute).map(|rest| {
            let (replacement, after) = split_delimited(rest, delim);
            let flags: String = after
                .chars()
                .take_while(|c| matches!(c, 'g' | 'c' | 'i' | 'I' | 'n' | '&' | 'e'))
                .collect();
            (replacement, flags)
        });
        if let Some((_, flags)) = &replacement
            && (flags.contains('i') || flags.contains('I'))
        {
            pattern = Pattern::new(&pattern.source, flags.contains('i'), false).ok()?;
        }

        let mut preview = Preview {
            lines: (first, last),
            ..Preview::default()
        };
        match replacement {
            Some((replacement, flags)) if !flags.contains('n') => {
                let previous = self.last_substitute.as_ref().map(|(_, r, _)| r.as_str());
                let replacement = Replacement::new(&replacement, previous);
                let plan = plan_substitute(
                    text,
                    first..=last,
                    &pattern,
                    &replacement,
                    flags.contains('g'),
                );
                let n = plan.edits.len();
                preview.info = Some(if n == 0 {
                    "no matches".to_owned()
                } else {
                    format!(
                        "{} on {}",
                        plural(n, "substitution", "substitutions"),
                        plural(plan.lines, "line", "lines")
                    )
                });
                preview.current = plan.edits.first().map(|(r, _)| r.clone());
                preview.substitutions = plan.edits;
            }
            _ => {
                let from = self.cursor.max(line_start(text, first));
                preview.current = first_match(text, &pattern, first, last, from)
                    .or_else(|| first_match(text, &pattern, first, last, 0));
                preview.info = Some(match_count(text, &pattern, first, last).0);
                preview.pattern = Some(pattern);
            }
        }
        Some(preview)
    }

    /// Moves the cursor to a search target (a jump), or applies a pending
    /// operator up to it.
    fn arrive_at(&mut self, target: Target, buf: &mut Buffer) {
        self.arrive(Some(target), true, buf);
    }
}

/// Characters that can delimit `:s` and `:g` patterns.
fn is_delimiter(c: char) -> bool {
    !c.is_alphanumeric() && !c.is_whitespace() && !matches!(c, '\\' | '"' | '|')
}

/// Like [`split_delimited`], but says whether the delimiter was found.
fn split_found(s: &str, delim: char) -> (String, Option<&str>) {
    match delimited_end(s, delim) {
        Some(end) => (
            split_delimited(s, delim).0,
            Some(&s[end + delim.len_utf8()..]),
        ),
        None => (split_delimited(s, delim).0, None),
    }
}

/// Byte offset of the first `delim` in `s` not escaped with a backslash.
fn delimited_end(s: &str, delim: char) -> Option<usize> {
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            chars.next();
        } else if c == delim {
            return Some(i);
        }
    }
    None
}

/// The word under (or after, on the same line) `pos`, for `⌃r ⌃w`.
fn word_under(text: &Rope, pos: usize) -> Option<String> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let line = line_of(text, pos);
    let start = line_start(text, line);
    let content: String = text.line(line).chars().take_while(|&c| c != '\n').collect();
    let chars: Vec<char> = content.chars().collect();
    let mut at = pos.saturating_sub(start).min(chars.len());
    while at < chars.len() && !is_word(chars[at]) {
        at += 1;
    }
    if at == chars.len() {
        return None;
    }
    let from = (0..at)
        .rev()
        .take_while(|&i| is_word(chars[i]))
        .last()
        .unwrap_or(at);
    let to = (at..chars.len())
        .find(|&i| !is_word(chars[i]))
        .unwrap_or(chars.len());
    Some(chars[from..to].iter().collect())
}

// ---- searching ---------------------------------------------------------------

/// The lines of `text` from `first` on, as `(line, start, content)`: `start`
/// is the line's first character and `content` its text without the line
/// ending. Lines in one rope chunk aren't copied.
fn lines_from(text: &Rope, first: usize) -> impl Iterator<Item = (usize, usize, Cow<'_, str>)> {
    let count = (last_line(text) + 1).saturating_sub(first);
    let mut start = if count > 0 {
        line_start(text, first)
    } else {
        0
    };
    text.lines_at(first.min(text.len_lines()))
        .take(count)
        .enumerate()
        .map(move |(i, line)| {
            let at = start;
            start += line.len_chars();
            (first + i, at, content(line.into()))
        })
}

fn line_at(text: &Rope, line: usize) -> (usize, Cow<'_, str>) {
    (line_start(text, line), content(text.line(line).into()))
}

/// A rope line without its `\n` or `\r\n`.
fn content(mut line: Cow<'_, str>) -> Cow<'_, str> {
    let keep = {
        let s: &str = &line;
        let s = s
            .strip_suffix('\n')
            .map_or(s, |s| s.strip_suffix('\r').unwrap_or(s));
        s.len()
    };
    match &mut line {
        Cow::Borrowed(s) => *s = &s[..keep],
        Cow::Owned(s) => s.truncate(keep),
    }
    line
}

/// Matches of `pattern` in one line's `content`, as character ranges in the
/// note (`start` is the line's first character).
pub fn line_matches<'a>(
    pattern: &'a Pattern,
    content: &'a str,
    start: usize,
) -> impl Iterator<Item = Range<usize>> + 'a {
    let (mut byte, mut chars) = (0usize, start);
    pattern.regex().find_iter(content).map(move |m| {
        chars += content[byte..m.start()].chars().count();
        let from = chars;
        chars += m.as_str().chars().count();
        byte = m.end();
        from..chars
    })
}

/// The next match after `from` (forward) or before it (backward), wrapping
/// around the note. Returns the match and whether the search wrapped.
pub fn find_next(
    text: &Rope,
    pattern: &Pattern,
    from: usize,
    forward: bool,
) -> Option<(Range<usize>, bool)> {
    let from = from.min(text.len_chars());
    let here = line_of(text, from);
    let last = last_line(text);
    if forward {
        for (_, start, content) in lines_from(text, here) {
            if let Some(m) = line_matches(pattern, &content, start).find(|m| m.start > from) {
                return Some((m, false));
            }
        }
        for (_, start, content) in lines_from(text, 0).take(here + 1) {
            if let Some(m) = line_matches(pattern, &content, start).next() {
                return Some((m, true));
            }
        }
    } else {
        let order = (0..=here).rev().map(|l| (l, false));
        let wrapped = (here..=last).rev().map(|l| (l, true));
        for (line, wrap) in order.chain(wrapped) {
            let (start, content) = line_at(text, line);
            let found = line_matches(pattern, &content, start)
                .filter(|m| wrap || m.start < from)
                .last();
            if let Some(m) = found {
                return Some((m, wrap));
            }
        }
    }
    None
}

/// The first match on lines `first..=last` that starts at or after `from`.
fn first_match(
    text: &Rope,
    pattern: &Pattern,
    first: usize,
    last: usize,
    from: usize,
) -> Option<Range<usize>> {
    lines_from(text, first)
        .take(last + 1 - first)
        .find_map(|(_, start, content)| {
            line_matches(pattern, &content, start).find(|m| m.start >= from)
        })
}

/// `5 matches on 5 lines`, for lines `first..=last`, and whether there were
/// any.
fn match_count(text: &Rope, pattern: &Pattern, first: usize, last: usize) -> (String, bool) {
    let (mut matches, mut lines) = (0, 0);
    for (_, _, content) in lines_from(text, first).take(last + 1 - first) {
        let n = pattern.regex().find_iter(&content).count();
        matches += n;
        lines += usize::from(n > 0);
    }
    if matches == 0 {
        return ("no matches".to_owned(), false);
    }
    let info = format!(
        "{} on {}",
        plural(matches, "match", "matches"),
        plural(lines, "line", "lines")
    );
    (info, true)
}

// ---- syntax colours ------------------------------------------------------------

/// Splits a command line into coloured parts (byte ranges of `text`).
pub fn tokens(kind: CommandKind, text: &str) -> Vec<(Range<usize>, Token)> {
    let mut out = Vec::new();
    match kind {
        CommandKind::Ex => ex_tokens(text, 0, &mut out),
        CommandKind::Search { .. } => {
            let delim = kind.prefix();
            match delimited_end(text, delim) {
                Some(end) => {
                    push(&mut out, 0..end, Token::Pattern);
                    push(&mut out, end..end + 1, Token::Separator);
                    push(&mut out, end + 1..text.len(), Token::Flags);
                }
                None => push(&mut out, 0..text.len(), Token::Pattern),
            }
        }
    }
    out
}

fn push(out: &mut Vec<(Range<usize>, Token)>, range: Range<usize>, token: Token) {
    if !range.is_empty() {
        out.push((range, token));
    }
}

fn ex_tokens(input: &str, base: usize, out: &mut Vec<(Range<usize>, Token)>) {
    let end = base + input.len();
    let lead = input.len() - input.trim_start_matches([' ', ':']).len();
    let s = &input[lead..];
    let Ok((_, rest)) = parse::parse_range(s) else {
        push(out, base..end, Token::Text);
        return;
    };
    let mut at = base + lead;
    let range_len = s.len() - rest.len();
    push(out, at..at + range_len, Token::Range);
    at += range_len;
    let trimmed = rest.trim_start();
    at += rest.len() - trimmed.len();

    // The name: letters, or one of the symbol commands.
    let name_len = match trimmed.chars().next() {
        Some(c) if c.is_ascii_alphabetic() => trimmed
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(trimmed.len()),
        Some('>' | '<' | '&') => 1,
        _ => 0,
    };
    let word = &trimmed[..name_len];
    let name = parse::lookup(word).or(match word {
        ">" => Some(">"),
        "<" => Some("<"),
        "&" => Some("&"),
        _ => None,
    });
    let known = name.is_some() || (!word.is_empty() && parse::is_command_prefix(word));
    push(
        out,
        at..at + name_len,
        if known { Token::Command } else { Token::Text },
    );
    at += name_len;
    let mut args = &trimmed[name_len..];
    if let Some(after) = args.strip_prefix('!').filter(|_| name.is_some()) {
        push(out, at..at + 1, Token::Command);
        at += 1;
        args = after;
    }

    let delimited = matches!(name, Some("substitute" | "global" | "vglobal"));
    let Some(delim) = args
        .chars()
        .next()
        .filter(|&c| delimited && is_delimiter(c))
    else {
        push(out, at..end, Token::Text);
        return;
    };
    let d = delim.len_utf8();
    push(out, at..at + d, Token::Separator);
    at += d;
    args = &args[d..];
    let Some(pat_end) = delimited_end(args, delim) else {
        push(out, at..end, Token::Pattern);
        return;
    };
    push(out, at..at + pat_end, Token::Pattern);
    push(out, at + pat_end..at + pat_end + d, Token::Separator);
    at += pat_end + d;
    args = &args[pat_end + d..];

    if name != Some("substitute") {
        // `:g/pat/cmd`: the command is coloured like any other.
        ex_tokens(args, at, out);
        return;
    }
    let Some(rep_end) = delimited_end(args, delim) else {
        push(out, at..end, Token::Replacement);
        return;
    };
    push(out, at..at + rep_end, Token::Replacement);
    push(out, at + rep_end..at + rep_end + d, Token::Separator);
    at += rep_end + d;
    args = &args[rep_end + d..];
    let flags = args
        .find(|c: char| !matches!(c, 'g' | 'c' | 'i' | 'I' | 'n' | '&' | 'e'))
        .unwrap_or(args.len());
    push(out, at..at + flags, Token::Flags);
    push(out, at + flags..end, Token::Text);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_keys;

    fn type_keys(vim: &mut Vim, buf: &mut Buffer, keys: &str) {
        for key in parse_keys(keys) {
            vim.handle_key(key, buf);
        }
    }

    fn setup(text: &str) -> (Vim, Buffer) {
        (Vim::new(), Buffer::new(text))
    }

    #[test]
    fn typing_editing_and_leaving_the_line() {
        let (mut vim, mut buf) = setup("one two\n");
        type_keys(&mut vim, &mut buf, ":sort u");
        assert_eq!(vim.mode(), Mode::Command);
        assert_eq!(vim.command_line().map(|l| l.text.as_str()), Some("sort u"));
        type_keys(&mut vim, &mut buf, "<c-w>");
        assert_eq!(vim.command_line().map(|l| l.text.as_str()), Some("sort "));
        type_keys(&mut vim, &mut buf, "<c-u>");
        assert_eq!(vim.command_line().map(|l| l.text.as_str()), Some(""));
        type_keys(&mut vim, &mut buf, "<bs>");
        assert_eq!(
            vim.mode(),
            Mode::Normal,
            "backspace on an empty line leaves"
        );
        type_keys(&mut vim, &mut buf, ":%s/one/1/<esc>");
        assert_eq!(
            (vim.mode(), buf.to_string()),
            (Mode::Normal, "one two\n".into())
        );
        assert!(vim.preview().is_none(), "esc drops the preview");
    }

    #[test]
    fn enter_runs_the_command_as_one_undo_step() {
        let (mut vim, mut buf) = setup("modal\nmodal\nx\n");
        type_keys(&mut vim, &mut buf, ":%s/modal/dialog/<cr>");
        assert_eq!(buf.to_string(), "dialog\ndialog\nx\n");
        let message = vim.message().expect("a message");
        assert_eq!(message.text, "2 substitutions on 2 lines");
        assert_eq!(
            message.line.as_ref().map(|l| l.text.as_str()),
            Some("%s/modal/dialog/")
        );
        type_keys(&mut vim, &mut buf, "j");
        assert!(vim.message().is_none(), "the next key clears it");
        type_keys(&mut vim, &mut buf, "u");
        assert_eq!(buf.to_string(), "modal\nmodal\nx\n");
    }

    #[test]
    fn errors_and_app_commands() {
        crate::ex::tests::test_app_commands();
        let (mut vim, mut buf) = setup("text\n");
        type_keys(&mut vim, &mut buf, ":frob<cr>");
        let message = vim.message().expect("error");
        assert!(message.error);
        assert_eq!(message.text, "E492: Not an editor command: frob");
        type_keys(&mut vim, &mut buf, ":w<cr>");
        assert_eq!(vim.take_app_command(), Some(AppCommand::new("write")));
        assert_eq!(vim.take_app_command(), None);
    }

    #[test]
    fn gd_asks_the_app_to_follow_a_link() {
        let (mut vim, mut buf) = setup("see [[ui-design]]\n");
        type_keys(&mut vim, &mut buf, "wwgd");
        assert_eq!(vim.take_app_command(), Some(AppCommand::new("follow")));
        assert_eq!(vim.mode(), crate::Mode::Normal);
        type_keys(&mut vim, &mut buf, "dgd");
        assert_eq!(
            buf.to_string(),
            "see [[ui-design]]\n",
            "d then gd deletes nothing"
        );
    }

    #[test]
    fn visual_colon_starts_with_the_selection_range() {
        let (mut vim, mut buf) = setup("b\na\nc\n");
        type_keys(&mut vim, &mut buf, "Vj:");
        assert_eq!(vim.command_line().map(|l| l.text.as_str()), Some("'<,'>"));
        type_keys(&mut vim, &mut buf, "sort<cr>");
        assert_eq!(buf.to_string(), "a\nb\nc\n");
    }

    #[test]
    fn search_n_and_wrapping() {
        let (mut vim, mut buf) = setup("a x\nb x\nc x\n");
        type_keys(&mut vim, &mut buf, "/x<cr>");
        assert_eq!(vim.cursor(), 2);
        assert!(vim.highlight_pattern().is_some());
        type_keys(&mut vim, &mut buf, "n");
        assert_eq!(vim.cursor(), 6);
        type_keys(&mut vim, &mut buf, "nn");
        assert_eq!(vim.cursor(), 2, "wraps to the top");
        assert_eq!(
            vim.message().map(|m| m.text.as_str()),
            Some("search hit BOTTOM, continuing at TOP")
        );
        type_keys(&mut vim, &mut buf, "N");
        assert_eq!(vim.cursor(), 10, "N goes back, wrapping to the bottom");
        type_keys(&mut vim, &mut buf, "gg?x<cr>");
        assert_eq!(vim.cursor(), 10, "? searches backward");
        type_keys(&mut vim, &mut buf, "n");
        assert_eq!(vim.cursor(), 6, "n follows the ? direction");
        type_keys(&mut vim, &mut buf, "gg2n");
        assert_eq!(vim.cursor(), 6, "counts");
        type_keys(&mut vim, &mut buf, ":noh<cr>");
        assert!(vim.highlight_pattern().is_none());
        type_keys(&mut vim, &mut buf, "n");
        assert!(
            vim.highlight_pattern().is_some(),
            "n turns highlights back on"
        );
    }

    #[test]
    fn search_errors_and_operators() {
        let (mut vim, mut buf) = setup("one two three\n");
        type_keys(&mut vim, &mut buf, "/zzz<cr>");
        assert_eq!(
            vim.message().map(|m| (m.text.as_str(), m.error)),
            Some(("E486: Pattern not found: zzz", true))
        );
        assert_eq!(vim.cursor(), 0);
        type_keys(&mut vim, &mut buf, "/three<cr>0dn");
        assert_eq!(buf.to_string(), "three\n", "dn deletes up to the match");
    }

    #[test]
    fn search_preview_highlights_and_counts() {
        let (mut vim, mut buf) = setup("x a\nb\nx c x\n");
        type_keys(&mut vim, &mut buf, "/x");
        let preview = vim.preview().expect("preview");
        assert_eq!(
            preview.current,
            Some(6..7),
            "the next match after the cursor"
        );
        assert_eq!(preview.info.as_deref(), Some("3 matches on 2 lines"));
        assert!(preview.covers(0) && preview.covers(2));
        assert_eq!(vim.cursor(), 0, "typing doesn't move the cursor");
        type_keys(&mut vim, &mut buf, "zz");
        assert_eq!(
            vim.preview().and_then(|p| p.info.as_deref()),
            Some("no matches")
        );
    }

    #[test]
    fn substitute_preview() {
        let (mut vim, mut buf) = setup("modal x\nnone\nmodal modal\n");
        type_keys(&mut vim, &mut buf, ":%s/modal");
        let preview = vim.preview().expect("pattern stage");
        assert_eq!(preview.info.as_deref(), Some("3 matches on 2 lines"));
        assert_eq!(preview.current, Some(0..5));
        assert!(preview.substitutions.is_empty());

        type_keys(&mut vim, &mut buf, "/dialog/g");
        let preview = vim.preview().expect("replace stage");
        assert_eq!(preview.info.as_deref(), Some("3 substitutions on 2 lines"));
        assert_eq!(preview.substitutions.len(), 3);
        assert_eq!(preview.substitutions_in(13..24).len(), 2);
        assert_eq!(preview.substitutions_in(8..12).len(), 0);
        assert_eq!(
            buf.to_string(),
            "modal x\nnone\nmodal modal\n",
            "preview only"
        );

        type_keys(&mut vim, &mut buf, "<esc>:s/modal/m/");
        let preview = vim.preview().expect("current line only");
        assert_eq!((preview.lines, preview.substitutions.len()), ((0, 0), 1));
        assert!(
            vim.last_search.is_none(),
            "previews don't count as searches"
        );
    }

    #[test]
    fn tokens_colour_the_parts() {
        let text = "%s/modal/dialog/g";
        let got: Vec<(&str, Token)> = tokens(CommandKind::Ex, text)
            .into_iter()
            .map(|(r, t)| (&text[r], t))
            .collect();
        assert_eq!(
            got,
            [
                ("%", Token::Range),
                ("s", Token::Command),
                ("/", Token::Separator),
                ("modal", Token::Pattern),
                ("/", Token::Separator),
                ("dialog", Token::Replacement),
                ("/", Token::Separator),
                ("g", Token::Flags),
            ]
        );
        let text = "'<,'>g!/^- /normal Ax";
        let got: Vec<(&str, Token)> = tokens(CommandKind::Ex, text)
            .into_iter()
            .map(|(r, t)| (&text[r], t))
            .collect();
        assert_eq!(
            got,
            [
                ("'<,'>", Token::Range),
                ("g", Token::Command),
                ("!", Token::Command),
                ("/", Token::Separator),
                ("^- ", Token::Pattern),
                ("/", Token::Separator),
                ("normal", Token::Command),
                (" Ax", Token::Text),
            ]
        );
        let got = tokens(CommandKind::Search { forward: true }, r"a\/b/e");
        assert_eq!(
            got,
            [
                (0..4, Token::Pattern),
                (4..5, Token::Separator),
                (5..6, Token::Flags)
            ]
        );
        assert_eq!(
            tokens(CommandKind::Ex, "so")[0].1,
            Token::Command,
            "a prefix of sort"
        );
        assert_eq!(tokens(CommandKind::Ex, "zz")[0].1, Token::Text);
    }

    #[test]
    fn find_next_both_ways() {
        let text = Rope::from_str("ab\nab\n");
        let p = Pattern::new("b", false, false).expect("ok");
        assert_eq!(find_next(&text, &p, 0, true), Some((1..2, false)));
        assert_eq!(find_next(&text, &p, 4, true), Some((1..2, true)));
        assert_eq!(find_next(&text, &p, 4, false), Some((1..2, false)));
        assert_eq!(find_next(&text, &p, 0, false), Some((4..5, true)));
        let empty = Rope::from_str("");
        assert_eq!(find_next(&empty, &p, 0, true), None);
    }
}

#[cfg(test)]
mod budget {
    use super::*;
    use std::time::{Duration, Instant};

    /// Each key typed on the command line recomputes the preview, so it has
    /// to fit the keystroke budget on a 1 MB note. Run with `cargo test -p
    /// mnote-vim --release -- --ignored`.
    #[test]
    #[ignore = "timing test; run with --release -- --ignored"]
    fn preview_per_key_on_one_megabyte() {
        let block = "## Section\n\nSome modal text with a modal word and (brackets).\n\n- item 42\n- item 7\n\n";
        let note = block.repeat(1_048_576 / block.len() + 1);
        let mut buf = Buffer::new(&note);
        for (kind, typed, last) in [
            (CommandKind::Search { forward: true }, "moda", 'l'),
            (CommandKind::Search { forward: true }, "modal", 'l'),
            (CommandKind::Ex, "%s/modal/dialog/", 'g'),
        ] {
            let mut vim = Vim::new();
            vim.open_command_line(kind, typed, &buf);
            let t = Instant::now();
            vim.command_key(Key::Char(last), &mut buf);
            let took = t.elapsed();
            eprintln!(
                "{:>20}: {took:?}",
                format!("{}{typed}{last}", kind.prefix())
            );
            assert!(vim.preview().is_some());
            assert!(took < Duration::from_millis(16), "{typed} took {took:?}");
        }
    }
}

#[cfg(test)]
mod editing_tests {
    use super::*;
    use crate::parse_keys;

    fn type_keys(vim: &mut Vim, buf: &mut Buffer, keys: &str) {
        for key in parse_keys(keys) {
            vim.handle_key(key, buf);
        }
    }

    fn line(vim: &Vim) -> (String, usize) {
        let l = vim.command_line().expect("open");
        (l.text.clone(), l.cursor)
    }

    struct Paths;
    impl Completer for Paths {
        fn paths(&mut self) -> Vec<String> {
            vec!["inbox.md".into(), "ideas.md".into()]
        }
    }

    #[test]
    fn the_caret_moves_and_edits_happen_at_it() {
        let (mut vim, mut buf) = (Vim::new(), Buffer::new("x\n"));
        type_keys(&mut vim, &mut buf, ":sort<left><left>X");
        assert_eq!(line(&vim), ("soXrt".into(), 3));
        type_keys(&mut vim, &mut buf, "<bs><c-b>%<c-e>!");
        assert_eq!(line(&vim), ("%sort!".into(), 6));
        type_keys(&mut vim, &mut buf, "<left><left><c-w>");
        assert_eq!(
            line(&vim),
            ("%t!".into(), 1),
            "⌃w deletes the word before the caret"
        );
        type_keys(&mut vim, &mut buf, "<right><c-u>");
        assert_eq!(line(&vim), ("!".into(), 0));
    }

    #[test]
    fn history_filters_by_what_is_typed() {
        let (mut vim, mut buf) = (Vim::new(), Buffer::new("b\na\n"));
        type_keys(
            &mut vim,
            &mut buf,
            ":sort<cr>:set sw=4<cr>:sort u<cr>:set sw=2<cr>",
        );
        assert_eq!(
            vim.history(CommandKind::Ex),
            ["sort", "set sw=4", "sort u", "set sw=2"]
        );
        type_keys(&mut vim, &mut buf, ":so<up>");
        assert_eq!(line(&vim).0, "sort u");
        type_keys(&mut vim, &mut buf, "<up>");
        assert_eq!(line(&vim).0, "sort");
        type_keys(&mut vim, &mut buf, "<up>");
        assert_eq!(line(&vim).0, "sort", "no older match: stays");
        type_keys(&mut vim, &mut buf, "<down><down>");
        assert_eq!(line(&vim).0, "so", "past the newest: what was typed");
        type_keys(&mut vim, &mut buf, "<c-p>");
        assert_eq!(line(&vim).0, "set sw=2", "⌃p doesn't filter");
        type_keys(&mut vim, &mut buf, "<esc>:sort<cr>");
        assert_eq!(
            vim.history(CommandKind::Ex).last().map(String::as_str),
            Some("sort"),
            "a repeat moves to the end, once"
        );
        assert_eq!(vim.history(CommandKind::Ex).len(), 4);
        type_keys(&mut vim, &mut buf, "/a<cr>/<up>");
        assert_eq!(line(&vim).0, "a", "search has its own history");
    }

    #[test]
    fn ctrl_r_pastes_registers_and_the_word_under_the_cursor() {
        let (mut vim, mut buf) = (Vim::new(), Buffer::new("hello world\n"));
        type_keys(&mut vim, &mut buf, "yiww:s/<c-r><c-w>/<c-r>0/");
        assert_eq!(line(&vim).0, "s/world/hello/");
        type_keys(&mut vim, &mut buf, "<cr>:<c-r>:");
        assert_eq!(line(&vim).0, "s/world/hello/", "⌃r : is the last command");
        type_keys(&mut vim, &mut buf, "<esc>yy:<c-r>\"");
        assert_eq!(
            line(&vim).0,
            "hello hello",
            "a linewise register drops its newline"
        );
        type_keys(&mut vim, &mut buf, "<esc>:x<c-r>q");
        assert_eq!(line(&vim).0, "x", "an empty register pastes nothing");
    }

    #[test]
    fn tab_completes_commands_with_a_popup() {
        let (mut vim, mut buf) = (Vim::new(), Buffer::new("b\na\n"));
        type_keys(&mut vim, &mut buf, ":'<,'>so<tab>");
        let l = vim.command_line().expect("open");
        assert_eq!(l.text, "'<,'>sort");
        let popup = l
            .completion
            .as_ref()
            .expect("several candidates open a popup");
        assert_eq!(popup.selected, Some(0));
        type_keys(&mut vim, &mut buf, "<tab><c-n>");
        assert_eq!(line(&vim).0, "'<,'>sort i");
        type_keys(&mut vim, &mut buf, "<c-p><s-tab><s-tab>");
        assert_eq!(line(&vim).0, "'<,'>so", "before the first: what was typed");
        type_keys(&mut vim, &mut buf, "<tab><tab>");
        assert_eq!(line(&vim).0, "'<,'>sort!");
        type_keys(&mut vim, &mut buf, " ");
        let l = vim.command_line().expect("open");
        assert!(
            l.completion.is_none(),
            "typing keeps the choice and closes the popup"
        );
        assert_eq!(l.text, "'<,'>sort! ");

        type_keys(&mut vim, &mut buf, "<esc>:noh<tab>");
        let l = vim.command_line().expect("open");
        assert_eq!(
            (l.text.as_str(), l.completion.is_none()),
            ("nohlsearch", true),
            "one candidate fills in"
        );
    }

    #[test]
    fn tab_completes_paths_from_the_app() {
        crate::ex::tests::test_app_commands();
        let (mut vim, mut buf) = (Vim::new(), Buffer::new("x\n"));
        vim.set_completer(Box::new(Paths));
        type_keys(&mut vim, &mut buf, ":e in<tab>");
        assert_eq!(line(&vim).0, "e inbox.md");
        type_keys(&mut vim, &mut buf, "<esc>/x<tab>");
        assert_eq!(line(&vim).0, "x", "search lines don't complete");
    }
}

#[cfg(test)]
mod mapping_tests {
    use crate::Mode;
    use crate::harness::{check, run};

    #[test]
    fn arrows_home_end_and_pages() {
        // NORMAL: Home and End are `0` and `$`; arrows are h j k l.
        check("ab|cd\nef", "<home>", "|abcd\nef");
        check("ab|cd\nef", "<end>", "abc|d\nef");
        check("ab|cd\nef", "<down>", "abcd\ne|f");
        // INSERT: the cursor moves and typing goes on there.
        check("ab|cd", "i<home>X<end>Y<esc>", "Xabcd|Y");
        check("ab\nc|d", "i<up>X<esc>", "a|Xb\ncd");
        check("abc|", "i<left><left>X<esc>", "a|Xbc");
        // The move starts a new undo step: `u` takes back only the last part.
        check("|", "iab<left>X<esc>u", "a|b");
    }

    #[test]
    fn the_unnamed_register_shares_the_system_clipboard() {
        use std::cell::RefCell;
        use std::rc::Rc;
        struct Shared(Rc<RefCell<Option<String>>>);
        impl crate::Clipboard for Shared {
            fn read(&mut self) -> Option<String> {
                self.0.borrow().clone()
            }
            fn write(&mut self, text: &str) {
                *self.0.borrow_mut() = Some(text.to_owned());
            }
        }
        let system = Rc::new(RefCell::new(None));
        let mut buf = mnote_core::Buffer::new("one\ntwo\n");
        let mut vim = crate::Vim::new();
        vim.set_clipboard(Box::new(Shared(system.clone())));
        vim.share_clipboard(true);
        let keys = |vim: &mut crate::Vim, buf: &mut mnote_core::Buffer, k: &str| {
            for key in crate::parse_keys(k) {
                vim.handle_key(key, buf);
            }
        };
        keys(&mut vim, &mut buf, "yy");
        assert_eq!(
            system.borrow().as_deref(),
            Some("one\n"),
            "a yank reaches the clipboard"
        );
        keys(&mut vim, &mut buf, "jp");
        assert_eq!(
            buf.to_string(),
            "one\ntwo\none\n",
            "its own linewise yank pastes as a line"
        );
        *system.borrow_mut() = Some("NEW".into());
        keys(&mut vim, &mut buf, "ggp");
        assert_eq!(
            buf.to_string(),
            "oNEWne\ntwo\none\n",
            "text copied elsewhere wins"
        );
        keys(&mut vim, &mut buf, "\"_dd");
        assert_eq!(
            system.borrow().as_deref(),
            Some("NEW"),
            "the black hole keeps nothing"
        );
        // ⌘V's paste_text: into an insert, and into the command line.
        keys(&mut vim, &mut buf, "Gi");
        assert!(vim.paste_text("pasted", &mut buf));
        keys(&mut vim, &mut buf, "<esc>:");
        assert!(vim.paste_text("s/a\nb", &mut buf));
        assert_eq!(vim.command_line().map(|l| l.text.as_str()), Some("s/a b"));
        assert!(buf.to_string().contains("pasted"));
    }

    #[test]
    fn clicks_and_selections_from_the_mouse() {
        let mut buf = mnote_core::Buffer::new("one two\nthree\n");
        let mut vim = crate::Vim::new();
        vim.click(4, &mut buf);
        assert_eq!((vim.cursor(), vim.mode()), (4, crate::Mode::Normal));
        vim.select(4, 6, false, &mut buf);
        assert_eq!(vim.selection(), Some(4..7), "both ends inclusive");
        vim.click(0, &mut buf);
        assert_eq!(
            (vim.cursor(), vim.mode()),
            (0, crate::Mode::Normal),
            "a click ends it"
        );
        // In INSERT a click moves the caret and keeps inserting.
        for key in crate::parse_keys("A") {
            vim.handle_key(key, &mut buf);
        }
        vim.click(3, &mut buf);
        for key in crate::parse_keys("X<esc>") {
            vim.handle_key(key, &mut buf);
        }
        assert_eq!(buf.to_string(), "oneX two\nthree\n");
        vim.select(8, 8, true, &mut buf);
        assert_eq!(vim.mode(), crate::Mode::VisualLine);
    }

    #[test]
    fn j_joins_lines_like_vim() {
        check("o|ne\n   two\nthree\n", "J", "one| two\nthree\n");
        check("o|ne\ntwo\nthree\n", "3J", "one two| three\n");
        check("|\ntwo\n", "J", "|two\n");
        check("o|ne \ntwo\n", "J", "one |two\n");
        check("f|(x\n)\n", "J", "f(x|)\n");
        check("o|ne\n  two\n", "gJ", "one|  two\n");
        check(
            "o|ne\ntwo\nthree\nfour\n",
            "Vj<esc>gvgJ",
            "one|two\nthree\nfour\n",
        );
        check("o|ne\ntwo\nthree\n", "Ju", "o|ne\ntwo\nthree\n");
        check("o|ne\ntwo\nthree\nfour\n", "J.", "one two| three\nfour\n");
    }

    #[test]
    fn alt_j_k_and_visual_j_k_move_lines() {
        check("o|ne\ntwo\nthree\n", "<A-j>", "two\no|ne\nthree\n");
        check("one\ntwo\nth|ree\n", "<A-k>", "one\nth|ree\ntwo\n");
        check("o|ne\ntwo\nthree\n", "2<A-j>", "two\nthree\no|ne\n");
        check("o|ne\ntwo\nthree\n", "5<A-j>", "two\nthree\no|ne\n");
        check("o|ne\ntwo\n", "<A-k>", "o|ne\ntwo\n");
        check("one\nt|wo\n", "<A-j>", "one\nt|wo\n");
        check("o|ne\ntwo\nthree\n", "<A-j><A-j>u", "two\no|ne\nthree\n");
        // The selection moves, and stays on the moved lines.
        check(
            "o|ne\ntwo\nthree\nfour\n",
            "VjJ<esc>",
            "three\no|ne\ntwo\nfour\n",
        );
        check(
            "one\ntwo\nth|ree\nfour\n",
            "VjK<esc>",
            "one\nth|ree\nfour\ntwo\n",
        );
        check("o|ne\ntwo\nthree\nfour\n", "VjJJd", "three\n|four\n");
        check("o|ne\ntwo\nthree\n", "V<A-j><esc>", "two\no|ne\nthree\n");
        check("o|ne\ntwo\nthree\n", "VJ<esc>u", "o|ne\ntwo\nthree\n");
        // ⌥j types ∆ in INSERT, so Vim leaves it to the input system.
        let mut vim = crate::Vim::new();
        let mut buf = mnote_core::Buffer::new("one\n");
        vim.handle_key(crate::Key::Char('i'), &mut buf);
        assert!(matches!(
            vim.handle_key(crate::Key::Alt('j'), &mut buf),
            crate::Effect::Unhandled(_)
        ));
        assert_eq!(
            crate::parse_keys("<A-j><M-k>"),
            vec![crate::Key::Alt('j'), crate::Key::Alt('k')]
        );
    }

    #[test]
    fn gj_and_gk_move_by_screen_row() {
        struct Rows;
        impl crate::ScreenRows for Rows {
            // Line 0 wraps after 4 and 8 characters; others fit on a row.
            fn row_starts(&self, line: usize) -> Vec<usize> {
                if line == 0 { vec![4, 8] } else { Vec::new() }
            }
        }
        let mut buf = mnote_core::Buffer::new("abcdefghijkl\nxy\n");
        let mut vim = crate::Vim::new();
        let keys = |vim: &mut crate::Vim, buf: &mut mnote_core::Buffer, k: &str| {
            for key in crate::parse_keys(k) {
                vim.handle_key(key, buf);
            }
        };
        keys(&mut vim, &mut buf, "lgj");
        assert_eq!(vim.cursor(), 14, "without wrap information gj is j");
        vim.set_screen_rows(Box::new(Rows));
        vim.set_cursor(1);
        keys(&mut vim, &mut buf, "gj");
        assert_eq!(vim.cursor(), 5, "row 2, same column");
        keys(&mut vim, &mut buf, "gj");
        assert_eq!(vim.cursor(), 9);
        keys(&mut vim, &mut buf, "gj");
        assert_eq!(vim.cursor(), 14, "next line, column 1");
        keys(&mut vim, &mut buf, "gk");
        assert_eq!(vim.cursor(), 9, "back to the last row of line 0");
        keys(&mut vim, &mut buf, "2gk");
        assert_eq!(vim.cursor(), 1);
        vim.set_wrap_motion(true);
        keys(&mut vim, &mut buf, "j");
        assert_eq!(vim.cursor(), 5, "plain j by screen row");
    }

    #[test]
    fn normal_and_visual_mappings() {
        // A mapping runs its keys; a count before it still applies.
        check("|a\nb\nc\n", ":nnoremap Q dd<cr>Q", "|b\nc\n");
        check("|a\nb\nc\n", ":nnoremap Q dd<cr>2Q", "|c\n");
        // Two keys: the first waits, the pair runs the mapping.
        check("|a\nb\n", ":nmap ,d dd<cr>,d", "|b\n");
        // VISUAL mappings apply in VISUAL only.
        check("|abc\n", ":vnoremap q d<cr>vlq", "|c\n");
        check("a|bc\n", ":vnoremap q d<cr>x", "a|c\n");
        // :nunmap takes one away.
        check("|a\nb\n", ":nnoremap Q dd<cr>:nunmap Q<cr>Q", "|a\nb\n");
    }

    #[test]
    fn a_waiting_prefix_runs_as_typed_when_flushed() {
        let mut buf = mnote_core::Buffer::new("abc\n");
        let mut vim = crate::Vim::new();
        for key in crate::parse_keys(":nmap xy dd<cr>x") {
            vim.handle_key(key, &mut buf);
        }
        assert!(vim.map_pending(), "x waits: it may become xy");
        assert_eq!(buf.to_string(), "abc\n");
        vim.flush_mapping(&mut buf);
        assert!(!vim.map_pending());
        assert_eq!(buf.to_string(), "bc\n", "then x runs as typed");
    }

    #[test]
    fn map_lists_and_unmap_reports() {
        let (_, vim, _) = run("|a\n", ":nnoremap Q dd<cr>:vmap q d<cr>");
        let mut vim = vim;
        let mut buf = mnote_core::Buffer::new("a\n");
        let listed = vim
            .execute("map", &mut buf)
            .expect("lists")
            .message
            .unwrap_or_default();
        assert!(
            listed.contains("n  Q") && listed.contains("v  q"),
            "{listed}"
        );
        let err = vim.execute("nunmap Z", &mut buf).expect_err("none");
        assert_eq!(err.code, 31);
    }

    #[test]
    fn pages_move_by_the_window_height() {
        let text: String = (0..60).map(|i| format!("{i}\n")).collect();
        let mut buf = mnote_core::Buffer::new(&text);
        let mut vim = crate::Vim::new();
        vim.set_page_lines(20);
        let line = |vim: &crate::Vim, buf: &mnote_core::Buffer| {
            crate::text::line_of(buf.text(), vim.cursor())
        };
        for key in crate::parse_keys("<pagedown>") {
            vim.handle_key(key, &mut buf);
        }
        assert_eq!(line(&vim, &buf), 20);
        for key in crate::parse_keys("<c-u>") {
            vim.handle_key(key, &mut buf);
        }
        assert_eq!(line(&vim, &buf), 10);
    }

    #[test]
    fn jk_leaves_insert_mode_by_default() {
        check("|", "ihellojk", "hell|o");
        check("|", "ijx<esc>", "j|x");
        check("|", "ijjk", "|j");
        let (_, vim, _) = run("|", "iajk");
        assert_eq!(vim.mode(), Mode::Normal);
    }

    #[test]
    fn imap_iunmap_and_listing() {
        let (text, vim, _) = run("|", ":iunmap jk<cr>ijk<esc>");
        assert_eq!(text, "j|k", "without the mapping jk is text");
        assert_eq!(vim.mode(), Mode::Normal);
        // `<lt>` types a literal `<` into the command line.
        let (text, _, _) = run("|", ":imap kj <lt>Esc><cr>ihikj");
        assert_eq!(text, "h|i");
        let (_, mut vim, mut buf) = run("|", "");
        let outcome = vim.execute("imap", &mut buf).expect("lists");
        assert_eq!(outcome.message.as_deref(), Some("i  jk       <Esc>"));
        assert!(vim.execute("iunmap zz", &mut buf).is_err());
    }
}
