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
        self.cmdline = Some(CommandLine {
            kind,
            text: text.to_owned(),
        });
        self.update_preview(buf);
    }

    /// A key typed in command mode.
    pub(crate) fn command_key(&mut self, key: Key, buf: &mut Buffer) {
        let Some(line) = self.cmdline.as_mut() else {
            self.enter(Mode::Normal, buf.text());
            return;
        };
        match key {
            Key::Esc | Key::Ctrl('c') => return self.close_command_line(buf),
            Key::Enter => return self.submit(buf),
            Key::Backspace | Key::Ctrl('h') => {
                // Backspace on an empty line leaves, as in Vim.
                if line.text.pop().is_none() {
                    return self.close_command_line(buf);
                }
            }
            Key::Ctrl('w') => {
                // Deletes the word before the end, or one punctuation mark.
                let is_word = |c: char| c.is_alphanumeric() || c == '_';
                let text = line.text.trim_end();
                let cut = match text.chars().next_back() {
                    None => 0,
                    Some(c) if is_word(c) => text.trim_end_matches(is_word).len(),
                    Some(c) => text.len() - c.len_utf8(),
                };
                line.text.truncate(cut);
            }
            Key::Ctrl('u') => line.text.clear(),
            Key::Char(c) => line.text.push(c),
            // Completion and history come with AMI-162.
            _ => return,
        }
        self.update_preview(buf);
    }

    fn close_command_line(&mut self, buf: &Buffer) {
        self.cmdline = None;
        self.preview = None;
        self.enter(Mode::Normal, buf.text());
    }

    /// `enter`: runs the line.
    fn submit(&mut self, buf: &mut Buffer) {
        let Some(line) = self.cmdline.take() else {
            return;
        };
        self.preview = None;
        self.enter(Mode::Normal, buf.text());
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
        let (mut vim, mut buf) = setup("text\n");
        type_keys(&mut vim, &mut buf, ":frob<cr>");
        let message = vim.message().expect("error");
        assert!(message.error);
        assert_eq!(message.text, "E492: Not an editor command: frob");
        type_keys(&mut vim, &mut buf, ":w<cr>");
        assert_eq!(
            vim.take_app_command(),
            Some(AppCommand::Write {
                path: None,
                force: false
            })
        );
        assert_eq!(vim.take_app_command(), None);
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
