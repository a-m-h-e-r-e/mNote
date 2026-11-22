//! Ex commands: what runs after `:` (decision D-002).
//!
//! [`Vim::execute`] parses a command line and runs it against the buffer as
//! one undo step. Text commands run here; commands that belong to the app
//! (`:w`, `:q`, `:e`, `:view`, `:explore`, `:find`, `:daily`) come back as an
//! [`AppCommand`]. Errors use Vim's numbers and wording (`E486: Pattern not
//! found: modal`).

pub mod cmdline;
pub mod cmdwin;
pub mod complete;
pub mod parse;
pub mod pattern;

use std::fmt;
use std::ops::{Range, RangeInclusive};

use mnote_core::Buffer;
use ropey::Rope;

use crate::Mode;
use crate::anchors::Anchors;
use crate::keys::{key_notation, parse_keys};
use crate::registers::Register;
use crate::state::{Operator, Vim, line_range};
use crate::text::{
    clamp_normal, first_non_blank, last_line, line_end, line_len, line_of, line_start,
};
use parse::{Address, Base, Parsed, split_delimited};
use pattern::{Pattern, Replacement};

/// A failed ex command, shown in the command line as `E486: …`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExError {
    pub code: u16,
    pub message: String,
}

impl fmt::Display for ExError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "E{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ExError {}

impl ExError {
    fn new(code: u16, message: impl Into<String>) -> Self {
        ExError {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn not_a_command(cmd: &str) -> Self {
        Self::new(492, format!("Not an editor command: {}", cmd.trim()))
    }

    pub(crate) fn invalid_range() -> Self {
        Self::new(16, "Invalid range")
    }

    fn pattern_not_found(pattern: &str) -> Self {
        Self::new(486, format!("Pattern not found: {pattern}"))
    }

    fn invalid_pattern(why: &str) -> Self {
        Self::new(383, format!("Invalid search string: {why}"))
    }

    fn trailing(args: &str) -> Self {
        Self::new(488, format!("Trailing characters: {}", args.trim()))
    }
}

/// A command for the app rather than the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppCommand {
    /// `:w [path]`
    Write { path: Option<String>, force: bool },
    /// `:q`
    Quit { force: bool },
    /// `:wq`, `:x`
    WriteQuit { force: bool },
    /// `:e [path]`
    Edit { path: Option<String>, force: bool },
    /// `:view [styled|source|plain]` (no argument cycles).
    View(Option<String>),
    /// `:explore [folder]`
    Explore(Option<String>),
    /// `:find [query]`
    Find(Option<String>),
    /// `:daily`
    Daily,
    /// `:cnext`: the next place in the results list.
    ListNext,
    /// `:cprevious` / `:cNext`
    ListPrevious,
    /// `:copen`: the results list in a picker.
    ListOpen,
    /// `gd`: follow the `[[link]]` at the cursor.
    FollowLink,
    /// `:split [note]` (`vertical`: `:vsplit`): a new pane, on that note or
    /// this one.
    Split {
        vertical: bool,
        path: Option<String>,
    },
    /// `:close`: closes the focused pane.
    Close,
    /// `:only`: closes every other pane.
    Only,
}

/// What a successful command produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExOutcome {
    /// A message for the command line (`5 substitutions on 4 lines`).
    pub message: Option<String>,
    /// Something for the app to do.
    pub app: Option<AppCommand>,
}

impl ExOutcome {
    fn message(text: impl Into<String>) -> Self {
        ExOutcome {
            message: Some(text.into()),
            app: None,
        }
    }

    fn app(command: AppCommand) -> Self {
        ExOutcome {
            message: None,
            app: Some(command),
        }
    }
}

/// Options set with `:set`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// `shiftwidth`/`sw`: spaces added or removed by `>` and `<`.
    pub shiftwidth: usize,
    /// `ignorecase`/`ic`
    pub ignorecase: bool,
    /// `smartcase`/`scs`: with `ignorecase`, an uppercase letter makes the
    /// search match case.
    pub smartcase: bool,
    /// `relativenumber`/`rnu`: the gutter counts from the cursor.
    pub relativenumber: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            shiftwidth: 2,
            ignorecase: true,
            smartcase: true,
            relativenumber: true,
        }
    }
}

/// The edits a `:s` would make, for the command and for its live preview.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubstitutePlan {
    /// Character ranges and their replacements, in document order.
    pub edits: Vec<(Range<usize>, String)>,
    /// Number of lines with at least one match.
    pub lines: usize,
}

/// Works out every replacement of `pattern` on `lines` (first match per line,
/// or all with `global`). Matches don't cross line ends.
pub fn plan_substitute(
    text: &Rope,
    lines: RangeInclusive<usize>,
    pattern: &Pattern,
    replacement: &Replacement,
    global: bool,
) -> SubstitutePlan {
    let mut plan = SubstitutePlan::default();
    let groups = replacement.uses_groups();
    each_line(text, *lines.start(), *lines.end(), |_, start, content| {
        let mut matched = false;
        let (mut byte, mut chars) = (0usize, 0usize);
        let mut push = |m: regex::Match<'_>, with: String| {
            chars += content[byte..m.start()].chars().count();
            let from = start + chars;
            chars += m.as_str().chars().count();
            byte = m.end();
            plan.edits.push((from..start + chars, with));
        };
        if groups {
            for caps in pattern.regex().captures_iter(content) {
                if let Some(m) = caps.get(0) {
                    push(m, replacement.expand(&caps));
                    matched = true;
                }
                if !global {
                    break;
                }
            }
        } else {
            for m in pattern.regex().find_iter(content) {
                push(m, replacement.expand_match(m.as_str()));
                matched = true;
                if !global {
                    break;
                }
            }
        }
        plan.lines += usize::from(matched);
    });
    plan
}

/// Calls `f(line, start, content)` for lines `first..=last`, walking the rope
/// once: `start` is the line's first character and `content` its text without
/// the line ending. Lines held in one rope chunk aren't copied.
pub fn each_line(text: &Rope, first: usize, last: usize, mut f: impl FnMut(usize, usize, &str)) {
    if first > last {
        return;
    }
    let mut start = line_start(text, first);
    for (i, line) in text.lines_at(first).take(last - first + 1).enumerate() {
        let len = line.len_chars();
        let content: std::borrow::Cow<'_, str> = line.into();
        let content = content
            .strip_suffix('\n')
            .map(|c| c.strip_suffix('\r').unwrap_or(c))
            .unwrap_or(&content);
        f(first + i, start, content);
        start += len;
    }
}

pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Commands whose range defaults to the whole note rather than the current
/// line.
#[derive(Clone, Copy)]
enum DefaultRange {
    Current,
    Whole,
}

impl Vim {
    pub fn options(&self) -> &Options {
        &self.options
    }

    pub fn options_mut(&mut self) -> &mut Options {
        &mut self.options
    }

    /// The pattern to highlight in the note, if search highlighting is on.
    pub fn search_highlight(&self) -> Option<&str> {
        self.search_highlight
            .then_some(self.last_search.as_deref())
            .flatten()
    }

    /// Compiles a pattern with the current case options. An empty pattern
    /// means the last search (`E35` if there was none).
    pub fn compile(&self, source: &str) -> Result<Pattern, ExError> {
        let source = if source.is_empty() {
            self.last_search
                .clone()
                .ok_or_else(|| ExError::new(35, "No previous regular expression"))?
        } else {
            source.to_owned()
        };
        Pattern::new(&source, self.options.ignorecase, self.options.smartcase)
            .map_err(|e| ExError::invalid_pattern(&e.0))
    }

    /// Runs one command line (with or without the leading `:`) as a single
    /// undo step, and returns to normal mode.
    pub fn execute(&mut self, input: &str, buf: &mut Buffer) -> Result<ExOutcome, ExError> {
        if self.mode != Mode::Normal {
            self.enter(Mode::Normal, buf.text());
        }
        let input = input.trim_start();
        let input = input.strip_prefix(':').unwrap_or(input);
        if input.trim().is_empty() {
            return Ok(ExOutcome::default());
        }
        buf.begin_change(self.cursor);
        let result = self.run(input, buf);
        self.anchors = Anchors::default();
        if self.mode != Mode::Normal {
            self.enter(Mode::Normal, buf.text());
        }
        self.cursor = clamp_normal(buf.text(), self.cursor);
        buf.end_change(self.cursor);
        result
    }

    fn run(&mut self, input: &str, buf: &mut Buffer) -> Result<ExOutcome, ExError> {
        let cmd = parse::parse(input)?;
        let args = cmd.args.trim();
        let no_args = |args: &str| {
            if args.is_empty() {
                Ok(())
            } else {
                Err(ExError::trailing(args))
            }
        };
        let arg = || (!args.is_empty()).then(|| args.to_owned());
        match cmd.name {
            "goto" => {
                let (_, last) = self.lines(&cmd, buf.text(), DefaultRange::Current)?;
                self.marks.insert('\'', self.cursor);
                self.cursor = first_non_blank(buf.text(), last);
                Ok(ExOutcome::default())
            }
            "substitute" | "&" => self.substitute(&cmd, buf),
            "global" | "vglobal" => self.global(&cmd, buf),
            "delete" | "yank" => {
                let (first, last, register) = self.lines_with_count(&cmd, buf.text())?;
                self.register = register;
                let op = if cmd.name == "delete" {
                    Operator::Delete
                } else {
                    Operator::Yank
                };
                self.cursor = line_start(buf.text(), first);
                self.apply_operator(op, line_range(buf.text(), first, last), true, buf);
                let n = last - first + 1;
                Ok(match (op, n) {
                    (Operator::Yank, n) if n > 1 => ExOutcome::message(format!("{n} lines yanked")),
                    (Operator::Delete, n) if n > 2 => {
                        ExOutcome::message(format!("{n} fewer lines"))
                    }
                    _ => ExOutcome::default(),
                })
            }
            ">" | "<" => {
                let shifts = 1 + args
                    .chars()
                    .take_while(|c| c.to_string() == cmd.name)
                    .count();
                let rest = args.trim_start_matches(cmd.name).trim();
                let (mut first, mut last) = self.lines(&cmd, buf.text(), DefaultRange::Current)?;
                if !rest.is_empty() {
                    let n: usize = rest.parse().map_err(|_| ExError::trailing(rest))?;
                    first = last;
                    last = (last + n.max(1) - 1).min(last_line(buf.text()));
                }
                let op = if cmd.name == ">" {
                    Operator::Indent
                } else {
                    Operator::Outdent
                };
                for _ in 0..shifts {
                    self.apply_operator(op, line_range(buf.text(), first, last), true, buf);
                }
                Ok(ExOutcome::default())
            }
            "put" => {
                let (_, last) = self.lines(&cmd, buf.text(), DefaultRange::Current)?;
                let name = args.chars().next();
                let mut reg = self.registers.get(name).ok_or_else(|| {
                    ExError::new(353, format!("Nothing in register {}", name.unwrap_or('"')))
                })?;
                if !reg.text.ends_with('\n') {
                    reg.text.push('\n');
                }
                reg.linewise = true;
                self.cursor = line_start(buf.text(), last);
                self.put(&reg, !cmd.bang, buf, 1);
                Ok(ExOutcome::default())
            }
            "move" | "t" | "copy" => self.move_or_copy(&cmd, buf),
            "normal" => self.normal(&cmd, buf),
            "sort" => self.sort(&cmd, buf),
            "undo" | "redo" => {
                no_args(args)?;
                let pos = if cmd.name == "undo" {
                    buf.undo()
                } else {
                    buf.redo()
                };
                match pos {
                    Some(pos) => {
                        self.cursor = clamp_normal(buf.text(), pos);
                        Ok(ExOutcome::default())
                    }
                    None => Ok(ExOutcome::message(if cmd.name == "undo" {
                        "Already at oldest change"
                    } else {
                        "Already at newest change"
                    })),
                }
            }
            "nohlsearch" => {
                no_args(args)?;
                self.search_highlight = false;
                Ok(ExOutcome::default())
            }
            "set" => {
                let result = self.set(args);
                self.recompile_search();
                result
            }
            "registers" | "display" => Ok(ExOutcome::message(self.list_registers())),
            "write" => Ok(ExOutcome::app(AppCommand::Write {
                path: arg(),
                force: cmd.bang,
            })),
            "wq" | "xit" => Ok(ExOutcome::app(AppCommand::WriteQuit { force: cmd.bang })),
            "quit" => {
                no_args(args)?;
                Ok(ExOutcome::app(AppCommand::Quit { force: cmd.bang }))
            }
            "edit" => Ok(ExOutcome::app(AppCommand::Edit {
                path: arg(),
                force: cmd.bang,
            })),
            "view" => Ok(ExOutcome::app(AppCommand::View(arg()))),
            "explore" => Ok(ExOutcome::app(AppCommand::Explore(arg()))),
            "find" => Ok(ExOutcome::app(AppCommand::Find(arg()))),
            "imap" | "inoremap" => {
                if args.is_empty() {
                    let list: Vec<String> = self
                        .imaps
                        .iter()
                        .map(|(lhs, rhs)| {
                            format!("i  {:<8} {}", key_notation(lhs), key_notation(rhs))
                        })
                        .collect();
                    return Ok(ExOutcome::message(if list.is_empty() {
                        "No mapping found".to_owned()
                    } else {
                        list.join("\n")
                    }));
                }
                let (lhs, rhs) = args
                    .split_once(char::is_whitespace)
                    .ok_or_else(|| ExError::new(474, "Invalid argument"))?;
                let (lhs, rhs) = (parse_keys(lhs), parse_keys(rhs.trim()));
                if lhs.is_empty() || rhs.is_empty() {
                    return Err(ExError::new(474, "Invalid argument"));
                }
                self.imaps.retain(|(l, _)| *l != lhs);
                self.imaps.push((lhs, rhs));
                Ok(ExOutcome::default())
            }
            "iunmap" => {
                let lhs = parse_keys(args);
                let before = self.imaps.len();
                self.imaps.retain(|(l, _)| *l != lhs);
                if self.imaps.len() == before {
                    return Err(ExError::new(31, "No such mapping"));
                }
                Ok(ExOutcome::default())
            }
            "cnext" | "cprevious" | "cNext" | "copen" => {
                no_args(args)?;
                Ok(ExOutcome::app(match cmd.name {
                    "cnext" => AppCommand::ListNext,
                    "copen" => AppCommand::ListOpen,
                    _ => AppCommand::ListPrevious,
                }))
            }
            "daily" => {
                no_args(args)?;
                Ok(ExOutcome::app(AppCommand::Daily))
            }
            "split" | "vsplit" => Ok(ExOutcome::app(AppCommand::Split {
                vertical: cmd.name == "vsplit",
                path: arg(),
            })),
            "close" | "only" => {
                no_args(args)?;
                Ok(ExOutcome::app(if cmd.name == "close" {
                    AppCommand::Close
                } else {
                    AppCommand::Only
                }))
            }
            other => Err(ExError::not_a_command(other)),
        }
    }

    // ---- ranges ---------------------------------------------------------------

    /// The 0-based line range of a command, or its default.
    fn lines(
        &mut self,
        cmd: &Parsed<'_>,
        text: &Rope,
        default: DefaultRange,
    ) -> Result<(usize, usize), ExError> {
        let current = line_of(text, self.cursor);
        let last = last_line(text);
        let (a, b) = match &cmd.range {
            parse::Range::None => match default {
                DefaultRange::Current => (current, current),
                DefaultRange::Whole => (0, last),
            },
            parse::Range::Whole => (0, last),
            parse::Range::One(a) => {
                let l = self.line_of_address(a, text)?;
                (l, l)
            }
            parse::Range::Two(a, b) => {
                let first = self.line_of_address(a, text)?;
                let second = self.line_of_address(b, text)?;
                (first, second)
            }
        };
        // Vim asks before using a backwards range; we just swap it.
        Ok((a.min(b), a.max(b)))
    }

    /// `:d x 3`, `:y 5`: the range, then an optional register and count (the
    /// count's lines start at the range's last line).
    fn lines_with_count(
        &mut self,
        cmd: &Parsed<'_>,
        text: &Rope,
    ) -> Result<(usize, usize, Option<char>), ExError> {
        let (mut first, mut last) = self.lines(cmd, text, DefaultRange::Current)?;
        let mut rest = cmd.args.trim();
        let mut register = None;
        if let Some(c) = rest.chars().next()
            && !c.is_ascii_digit()
        {
            register = Some(c);
            rest = rest[c.len_utf8()..].trim_start();
        }
        if !rest.is_empty() {
            let n: usize = rest.parse().map_err(|_| ExError::trailing(rest))?;
            first = last;
            last = (last + n.max(1) - 1).min(last_line(text));
        }
        Ok((first, last, register))
    }

    /// An address as a 0-based line. Line `0` is only allowed where a command
    /// says so (see [`Self::destination`]).
    fn line_of_address(&mut self, address: &Address, text: &Rope) -> Result<usize, ExError> {
        let n = self.address_number(address, text)?;
        let last = last_line(text) as i64 + 1;
        if n < 1 || n > last {
            return Err(ExError::invalid_range());
        }
        Ok((n - 1) as usize)
    }

    /// The 1-based line number an address names (0 is "before line 1").
    fn address_number(&mut self, address: &Address, text: &Rope) -> Result<i64, ExError> {
        let current = line_of(text, self.cursor) as i64 + 1;
        let base = match &address.base {
            Base::Line(n) => *n as i64,
            Base::Current => current,
            Base::Last => last_line(text) as i64 + 1,
            Base::Mark(c) => {
                let name = if *c == '`' { '\'' } else { *c };
                let pos = self
                    .marks
                    .get(&name)
                    .copied()
                    .ok_or_else(|| ExError::new(20, "Mark not set"))?;
                line_of(text, pos.min(text.len_chars())) as i64 + 1
            }
            Base::Search { pattern, forward } => {
                let compiled = self.compile(pattern)?;
                self.remember_search(&compiled);
                let total = last_line(text) + 1;
                let from = current as usize - 1;
                let order: Box<dyn Iterator<Item = usize>> = if *forward {
                    Box::new((1..=total).map(|i| (from + i) % total))
                } else {
                    Box::new((1..=total).map(|i| (from + total - i % total) % total))
                };
                let found = order
                    .into_iter()
                    .find(|&line| {
                        let start = line_start(text, line);
                        compiled
                            .is_match(&text.slice(start..start + line_len(text, line)).to_string())
                    })
                    .ok_or_else(|| ExError::pattern_not_found(&compiled.source))?;
                found as i64 + 1
            }
        };
        Ok(base + address.offset)
    }

    /// The destination of `:m` and `:t`: a 1-based line, where 0 means "above
    /// the first line".
    fn destination(&mut self, args: &str, text: &Rope) -> Result<usize, ExError> {
        let parsed = parse::parse(&format!("{args}d"))
            .ok()
            .and_then(|p| match p.range {
                parse::Range::One(a) => Some(a),
                _ => None,
            })
            .ok_or_else(|| ExError::new(14, "Invalid address"))?;
        let n = self.address_number(&parsed, text)?;
        if n < 0 || n > last_line(text) as i64 + 1 {
            return Err(ExError::invalid_range());
        }
        Ok(n as usize)
    }

    // ---- commands ------------------------------------------------------------

    fn substitute(&mut self, cmd: &Parsed<'_>, buf: &mut Buffer) -> Result<ExOutcome, ExError> {
        let args = cmd.args;
        let delim = args.chars().next().filter(|c| {
            !c.is_alphanumeric() && !c.is_whitespace() && !matches!(c, '\\' | '"' | '|')
        });
        let (pattern_src, replacement_src, rest) = match delim {
            Some(d) if cmd.name == "substitute" => {
                let (pattern, rest) = split_delimited(&args[d.len_utf8()..], d);
                let (replacement, rest) = split_delimited(rest, d);
                (pattern, replacement, rest.to_owned())
            }
            // `:s` or `:&` alone (or with flags) repeats the last substitute.
            _ => {
                let (p, r, flags) = self
                    .last_substitute
                    .clone()
                    .ok_or_else(|| ExError::new(35, "No previous regular expression"))?;
                let extra = args.trim();
                let flags = if let Some(more) = extra.strip_prefix('&') {
                    format!("{flags}{more}")
                } else {
                    extra.to_owned()
                };
                (p, r, flags)
            }
        };

        let flag_end = rest
            .find(|c: char| !matches!(c, 'g' | 'c' | 'i' | 'I' | 'n' | '&' | 'e'))
            .unwrap_or(rest.len());
        let flags = &rest[..flag_end];
        let count_arg = rest[flag_end..].trim();
        if flags.contains('c') {
            return Err(ExError::new(
                474,
                "Invalid argument: the c (confirm) flag isn't supported yet",
            ));
        }

        let mut pattern = self.compile(&pattern_src)?;
        if flags.contains('i') || flags.contains('I') {
            pattern = Pattern::new(&pattern.source, flags.contains('i'), false)
                .map_err(|e| ExError::invalid_pattern(&e.0))?;
        }
        let previous = self.last_substitute.as_ref().map(|(_, r, _)| r.clone());
        let replacement = Replacement::new(&replacement_src, previous.as_deref());

        let text = buf.text();
        let (mut first, mut last) = self.lines(cmd, text, DefaultRange::Current)?;
        if !count_arg.is_empty() {
            let n: usize = count_arg
                .parse()
                .map_err(|_| ExError::trailing(count_arg))?;
            first = last;
            last = (last + n.max(1) - 1).min(last_line(text));
        }

        self.remember_search(&pattern);
        self.last_substitute = Some((
            pattern.source.clone(),
            replacement_src,
            flags.replace('&', ""),
        ));

        let plan = plan_substitute(
            text,
            first..=last,
            &pattern,
            &replacement,
            flags.contains('g'),
        );
        if plan.edits.is_empty() {
            if flags.contains('e') {
                return Ok(ExOutcome::default());
            }
            return Err(ExError::pattern_not_found(&pattern.source));
        }
        let n = plan.edits.len();
        let summary = format!(
            "{} on {}",
            plural(n, "substitution", "substitutions"),
            plural(plan.lines, "line", "lines")
        );
        if flags.contains('n') {
            return Ok(ExOutcome::message(format!(
                "{} on {}",
                plural(n, "match", "matches"),
                plural(plan.lines, "line", "lines")
            )));
        }
        // The cursor ends on the last line changed, shifted by any lines the
        // earlier replacements added (matches never span lines).
        let last_edit_start = plan.edits.last().map_or(0, |(r, _)| r.start);
        let added_before: usize = plan.edits[..n - 1]
            .iter()
            .map(|(_, r)| r.matches('\n').count())
            .sum();
        let added_by_last = plan.edits[n - 1].1.matches('\n').count();
        let cursor_line = line_of(buf.text(), last_edit_start) + added_before + added_by_last;
        // Applied from the end, so earlier ranges stay valid.
        for (range, replacement) in plan.edits.into_iter().rev() {
            self.edit(buf, range, &replacement);
        }
        let cursor_line = cursor_line.min(last_line(buf.text()));
        self.cursor = first_non_blank(buf.text(), cursor_line);
        Ok(ExOutcome::message(summary))
    }

    fn global(&mut self, cmd: &Parsed<'_>, buf: &mut Buffer) -> Result<ExOutcome, ExError> {
        if !self.anchors.is_empty() {
            return Err(ExError::new(147, "Cannot do :global recursive"));
        }
        let invert = cmd.name == "vglobal" || cmd.bang;
        let args = cmd.args.trim_start();
        let delim = args
            .chars()
            .next()
            .ok_or_else(|| ExError::new(35, "No previous regular expression"))?;
        let (pattern_src, command) = split_delimited(&args[delim.len_utf8()..], delim);
        let command = command.trim();
        let pattern = self.compile(&pattern_src)?;
        self.remember_search(&pattern);

        let text = buf.text();
        let (first, last) = self.lines(cmd, text, DefaultRange::Whole)?;
        let mut matching: Vec<(usize, usize)> = Vec::new();
        each_line(text, first, last, |line, start, content| {
            if pattern.is_match(content) != invert {
                matching.push((line, start));
            }
        });
        if matching.is_empty() {
            return Err(if invert {
                ExError::new(
                    538,
                    format!("Pattern found in every line: {}", pattern.source),
                )
            } else {
                ExError::pattern_not_found(&pattern.source)
            });
        }
        if command.is_empty() || command == "p" || command == "print" {
            return Ok(ExOutcome::message(plural(matching.len(), "line", "lines")));
        }

        self.anchors = Anchors::new(matching.iter().map(|&(_, start)| start).collect());
        let mut last_message = None;
        for i in 0..self.anchors.len() {
            let Some(pos) = self.anchors.get(i) else {
                continue;
            };
            self.cursor = pos;
            let outcome = self.run(command, buf)?;
            if outcome.app.is_some() {
                return Ok(outcome);
            }
            last_message = outcome.message.or(last_message);
        }
        self.anchors = Anchors::default();
        Ok(ExOutcome {
            message: last_message,
            app: None,
        })
    }

    fn normal(&mut self, cmd: &Parsed<'_>, buf: &mut Buffer) -> Result<ExOutcome, ExError> {
        let keys = parse_keys(cmd.args.trim_start());
        if keys.is_empty() {
            return Err(ExError::new(471, "Argument required"));
        }
        let lines: Vec<Option<usize>> = match cmd.range {
            parse::Range::None => vec![None],
            _ => {
                let (first, last) = self.lines(cmd, buf.text(), DefaultRange::Current)?;
                (first..=last)
                    .map(|l| Some(line_start(buf.text(), l)))
                    .collect()
            }
        };
        // Lines are tracked as anchors so edits on one don't misplace the
        // next. Inside `:g` (which owns the anchors) positions are taken once.
        let track = self.anchors.is_empty() && lines.iter().any(Option::is_some);
        if track {
            self.anchors = Anchors::new(lines.iter().flatten().copied().collect());
        }
        for (i, line) in lines.iter().enumerate() {
            if let Some(fixed) = line {
                let pos = if track {
                    self.anchors.get(i)
                } else {
                    Some(*fixed)
                };
                let Some(pos) = pos else { continue };
                self.cursor = pos.min(buf.len_chars());
            }
            for key in keys.iter().copied() {
                self.handle_key(key, buf);
            }
            if self.mode != Mode::Normal {
                self.handle_key(crate::Key::Esc, buf);
            }
        }
        if track {
            self.anchors = Anchors::default();
        }
        Ok(ExOutcome::default())
    }

    fn move_or_copy(&mut self, cmd: &Parsed<'_>, buf: &mut Buffer) -> Result<ExOutcome, ExError> {
        let text = buf.text();
        let (a, b) = self.lines(cmd, text, DefaultRange::Current)?;
        let dest = self.destination(cmd.args.trim(), text)?;
        let line_text = |l: usize| {
            let start = line_start(text, l);
            text.slice(start..start + line_len(text, l)).to_string()
        };
        let block: Vec<String> = (a..=b).map(line_text).collect();
        let count = block.len();

        let (first, last, new, cursor_line) = if cmd.name == "move" {
            // `dest` is 1-based: the moved lines go after line `dest`.
            if dest > a && dest <= b + 1 {
                if dest == b + 1 || dest == a {
                    return Ok(ExOutcome::default());
                }
                return Err(ExError::new(
                    134,
                    "Cannot move a range of lines into itself",
                ));
            }
            if dest > b + 1 {
                let between: Vec<String> = (b + 1..dest).map(line_text).collect();
                let mut new = between;
                new.extend(block);
                (a, dest - 1, new, dest - 1)
            } else {
                let between: Vec<String> = (dest..a).map(line_text).collect();
                let mut new = block;
                new.extend(between);
                (dest, b, new, dest + count - 1)
            }
        } else if dest == 0 {
            let mut new = block;
            new.push(line_text(0));
            (0, 0, new, count - 1)
        } else {
            let mut new = vec![line_text(dest - 1)];
            new.extend(block);
            (dest - 1, dest - 1, new, dest - 1 + count)
        };
        self.replace_lines(buf, first, last, &new);
        self.cursor = first_non_blank(buf.text(), cursor_line.min(last_line(buf.text())));
        Ok(ExOutcome::default())
    }

    fn sort(&mut self, cmd: &Parsed<'_>, buf: &mut Buffer) -> Result<ExOutcome, ExError> {
        let text = buf.text();
        let (first, last) = self.lines(cmd, text, DefaultRange::Whole)?;
        let opts = cmd.args.trim();
        if let Some(bad) = opts.chars().find(|c| !matches!(c, 'i' | 'n' | 'u' | ' ')) {
            return Err(ExError::new(474, format!("Invalid argument: {bad}")));
        }
        let (ignore, numeric, unique) =
            (opts.contains('i'), opts.contains('n'), opts.contains('u'));
        let mut lines: Vec<String> = (first..=last)
            .map(|l| {
                let start = line_start(text, l);
                text.slice(start..start + line_len(text, l)).to_string()
            })
            .collect();
        let key = |s: &str| {
            if ignore {
                s.to_lowercase()
            } else {
                s.to_owned()
            }
        };
        if numeric {
            lines.sort_by_key(|l| first_number(l));
        } else {
            lines.sort_by_key(|l| key(l));
        }
        if cmd.bang {
            lines.reverse();
        }
        if unique {
            lines.dedup_by(|a, b| key(a) == key(b));
        }
        self.replace_lines(buf, first, last, &lines);
        self.cursor = line_start(buf.text(), first);
        Ok(ExOutcome::default())
    }

    fn set(&mut self, args: &str) -> Result<ExOutcome, ExError> {
        if args.is_empty() {
            return Ok(ExOutcome::message(self.describe_options()));
        }
        let mut messages = Vec::new();
        for item in args.split_whitespace() {
            let (name, value) = match item.split_once(['=', ':']) {
                Some((n, v)) => (n, Some(v)),
                None => (item, None),
            };
            let (name, query, toggle) = (
                name.trim_end_matches(['?', '!']),
                name.ends_with('?'),
                name.ends_with('!'),
            );
            let (negated, base) = match name.strip_prefix("no") {
                Some(rest) if flag(&mut self.options, rest).is_some() => (true, rest),
                _ => (false, name),
            };
            if matches!(base, "shiftwidth" | "sw") {
                match value {
                    Some(v) => {
                        self.options.shiftwidth = v.parse().map_err(|_| {
                            ExError::new(521, format!("Number required after =: {item}"))
                        })?;
                    }
                    None => messages.push(format!("shiftwidth={}", self.options.shiftwidth)),
                }
                continue;
            }
            let Some(slot) = flag(&mut self.options, base) else {
                return Err(ExError::new(518, format!("Unknown option: {base}")));
            };
            if query {
                let full = match base {
                    "ic" => "ignorecase",
                    "scs" => "smartcase",
                    "rnu" => "relativenumber",
                    other => other,
                };
                messages.push(format!("{}{full}", if *slot { "" } else { "no" }));
            } else if toggle {
                *slot = !*slot;
            } else {
                *slot = !negated;
            }
        }
        Ok(if messages.is_empty() {
            ExOutcome::default()
        } else {
            ExOutcome::message(messages.join("  "))
        })
    }

    fn describe_options(&self) -> String {
        let o = &self.options;
        let on = |b: bool, name: &str| format!("{}{name}", if b { "" } else { "no" });
        format!(
            "shiftwidth={}  {}  {}  {}",
            o.shiftwidth,
            on(o.ignorecase, "ignorecase"),
            on(o.smartcase, "smartcase"),
            on(o.relativenumber, "relativenumber")
        )
    }

    fn list_registers(&mut self) -> String {
        let mut out = String::from("Type Name Content");
        let names = [
            '"', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '-', '+',
        ]
        .into_iter()
        .chain('a'..='z');
        for name in names {
            if let Some(Register { text, linewise }) = self.registers.get(Some(name)) {
                let shown: String = text.replace('\n', "^J").chars().take(60).collect();
                out.push_str(&format!(
                    "\n  {}  \"{name}   {shown}",
                    if linewise { 'l' } else { 'c' }
                ));
            }
        }
        out
    }

    /// Replaces lines `first..=last` (without the last line's newline) with
    /// `new`, joined by newlines, as one edit.
    fn replace_lines(&mut self, buf: &mut Buffer, first: usize, last: usize, new: &[String]) {
        let text = buf.text();
        let range = line_start(text, first)..line_end(text, last);
        self.edit(buf, range, &new.join("\n"));
    }
}

/// `:set` flags by long or short name.
fn flag<'a>(options: &'a mut Options, name: &str) -> Option<&'a mut bool> {
    Some(match name {
        "ignorecase" | "ic" => &mut options.ignorecase,
        "smartcase" | "scs" => &mut options.smartcase,
        "relativenumber" | "rnu" => &mut options.relativenumber,
        _ => return None,
    })
}

/// The first integer on a line, for `:sort n`. Lines without one sort first.
fn first_number(line: &str) -> Option<i64> {
    let start = line.find(|c: char| c.is_ascii_digit())?;
    let negative = line[..start].ends_with('-');
    let digits: String = line[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let n: i64 = digits.parse().ok()?;
    Some(if negative { -n } else { n })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `command` on `before` (cursor at its `|`); returns the text with
    /// `|` at the new cursor and the outcome.
    fn ex(before: &str, command: &str) -> (String, Result<ExOutcome, ExError>) {
        ex_with(&mut Vim::new(), before, command)
    }

    fn ex_with(vim: &mut Vim, before: &str, command: &str) -> (String, Result<ExOutcome, ExError>) {
        let start = before.find('|').expect("cursor marker");
        let mut buf = Buffer::new(&before.replacen('|', "", 1));
        vim.set_cursor(buf.text().byte_to_char(start));
        let result = vim.execute(command, &mut buf);
        let mut shown = buf.to_string();
        shown.insert(
            buf.text().char_to_byte(vim.cursor().min(buf.len_chars())),
            '|',
        );
        (shown, result)
    }

    #[track_caller]
    fn ok(before: &str, command: &str, after: &str) -> Option<String> {
        let (got, result) = ex(before, command);
        let outcome = result.unwrap_or_else(|e| panic!("{command:?} failed: {e}"));
        assert_eq!(got, after, "after {command:?}");
        outcome.message
    }

    #[track_caller]
    fn err(before: &str, command: &str) -> String {
        ex(before, command).1.expect_err("should fail").to_string()
    }

    #[test]
    fn substitute_basics() {
        let msg = ok("|one two two\ntwo", "%s/two/2/g", "one 2 2\n|2");
        assert_eq!(msg.as_deref(), Some("3 substitutions on 2 lines"));
        ok("|one two two\ntwo", "s/two/2/", "|one 2 two\ntwo");
        ok(
            "|hello world",
            r"s/\(\w\+\) \(\w\+\)/\2 \1/",
            "|world hello",
        );
        ok("|10-20", r"s/\v(\d+)-(\d+)/\2-\1/", "|20-10");
        ok("|a, b, c", r"s/, /\r/g", "a\nb\n|c");
        ok("|path/to/x", r"s#/#\\#g", r"|path\to\x");
        ok("|Title", r"s/\w\+/\U&/", "|TITLE");
    }

    #[test]
    fn substitute_flags_and_case() {
        let msg = ok("|one two two\ntwo", "%s/two/2/gn", "|one two two\ntwo");
        assert_eq!(msg.as_deref(), Some("3 matches on 2 lines"));
        ok("|nothing here", "%s/zzz/y/e", "|nothing here");
        ok("|HELLO", "s/hello/x/", "|x");
        assert_eq!(
            err("|hello", "s/Hello/x/"),
            "E486: Pattern not found: Hello",
            "smartcase"
        );
        ok("|hello", "s/Hello/x/i", "|x");
        assert!(err("|a", "s/a/b/c").starts_with("E474"));
    }

    #[test]
    fn substitute_repeats_and_reuses_the_last_pattern() {
        let mut vim = Vim::new();
        let (got, _) = ex_with(&mut vim, "|aa\naa", "s/a/b/");
        assert_eq!(got, "|ba\naa");
        let (got, _) = ex_with(&mut vim, "ba\n|aa", "&");
        assert_eq!(got, "ba\n|ba");
        let (got, _) = ex_with(&mut vim, "|xa", "s//y/");
        assert_eq!(got, "|xy", "empty pattern is the last search");
        let (got, _) = ex_with(&mut vim, "|aaa", "s/a/~!/g");
        assert_eq!(got, "|y!y!y!", "~ is the previous replacement");
    }

    #[test]
    fn a_substitute_is_one_undo_step() {
        let mut buf = Buffer::new("a a\na a\n");
        let mut vim = Vim::new();
        vim.execute("%s/a/b/g", &mut buf).expect("ok");
        assert_eq!(buf.to_string(), "b b\nb b\n");
        buf.undo();
        assert_eq!(buf.to_string(), "a a\na a\n");
    }

    #[test]
    fn ranges_and_addresses() {
        ok("|a\nb\nc\nd", "2,3d", "a\n|d");
        ok("|a\nb\nc\nd", ".,+1d", "|c\nd");
        ok("a\nb\nc\n|d", "$d", "a\nb\n|c");
        ok("a\nb\nc\n|d", "$-1,$d", "a\n|b");
        ok("|a\nb\nc\nd", "/c/d", "a\nb\n|d");
        ok("a\nb\n|c\nd", "?a?d", "|b\nc\nd");
        ok("|a\nb\nc", "3", "a\nb\n|c");
        ok("a\n|b\nc", "3,1d", "|");
        assert_eq!(err("|a\nb", "5,9d"), "E16: Invalid range");
        assert_eq!(err("|a", "'x,'yd"), "E20: Mark not set");
        assert_eq!(err("|a", "/zzz/d"), "E486: Pattern not found: zzz");
    }

    #[test]
    fn visual_marks_feed_ranges() {
        let mut buf = Buffer::new("a\nb\nc\nd");
        let mut vim = Vim::new();
        for key in parse_keys("jVj<esc>") {
            vim.handle_key(key, &mut buf);
        }
        vim.execute("'<,'>d", &mut buf).expect("ok");
        assert_eq!(buf.to_string(), "a\nd");
    }

    #[test]
    fn global_and_vglobal() {
        ok("|a\nxb\nc\nxd", "g/x/d", "a\n|c");
        ok("|a\nxb\nc\nxd", "v/x/d", "xb\n|xd");
        ok("|a\nxb\nc\nxd", "g!/x/d", "xb\n|xd");
        ok("|a\nxb\nc\nxd", "g/x/s/x/y/", "a\nyb\nc\n|yd");
        ok("|- a\n- b", "g/^-/normal A.", "- a.\n- b|.");
        let msg = ok("|a\nxb\nxc", "g/x/", "|a\nxb\nxc");
        assert_eq!(msg.as_deref(), Some("2 lines"));
        assert_eq!(err("|a\nb", "g/zzz/d"), "E486: Pattern not found: zzz");
        // Deleting a line the next match would visit must not misfire.
        ok("|x1\nx2\nx3\nk", "g/x/.,+1d", "|");
    }

    #[test]
    fn normal_runs_keys_on_each_line() {
        ok("|a\nb", "%normal A;", "a;\nb|;");
        ok("|item", "normal I- ", "-| item");
        ok("|one two", "normal dw", "|two");
        assert!(err("|a", "normal").starts_with("E471"));
    }

    #[test]
    fn move_and_copy() {
        ok("|a\nb\nc\nd", "1m$", "b\nc\nd\n|a");
        ok("a\nb\nc\n|d", "$m0", "|d\na\nb\nc");
        ok("|a\nb\nc\nd", "2,3m0", "b\n|c\na\nd");
        ok("|a\nb\nc\nd", "2m2", "|a\nb\nc\nd");
        ok("|a\nb\nc", "1t$", "a\nb\nc\n|a");
        ok("|a\nb\nc", "3co0", "|c\na\nb\nc");
        assert!(err("|a\nb\nc\nd", "1,3m2").starts_with("E134"));
    }

    #[test]
    fn sort_variants() {
        ok("|c\na\nb\na", "sort", "|a\na\nb\nc");
        ok("|c\na\nb\na", "sort u", "|a\nb\nc");
        ok("|a\nc\nb", "sort!", "|c\nb\na");
        ok("|x10\nx2\nx1", "sort n", "|x1\nx2\nx10");
        ok("|b\nA\nc", "sort i", "|A\nb\nc");
        ok("z\n|c\nb\na", "2,$sort", "z\n|a\nb\nc");
    }

    #[test]
    fn shift_yank_put_and_delete_registers() {
        ok("|a\nb", ">", "  |a\nb");
        ok("|a\nb\nc", "2,3>>", "a\n    |b\n    c");
        ok("|    a", "<", "  |a");
        let mut vim = Vim::new();
        let (got, _) = ex_with(&mut vim, "|one\ntwo", "1y");
        assert_eq!(got, "|one\ntwo");
        let (got, _) = ex_with(&mut vim, "one\n|two", "$put");
        assert_eq!(got, "one\ntwo\n|one");
        let (_, _) = ex_with(&mut vim, "|keep\ngone", "2d a");
        assert_eq!(vim.register('a').expect("a").text, "gone\n");
        let (got, _) = ex_with(&mut vim, "|x", "put! a");
        assert_eq!(got, "|gone\nx");
        assert!(
            ex_with(&mut Vim::new(), "|x", "put q").1.is_err(),
            "empty register"
        );
    }

    #[test]
    fn undo_redo_set_and_noh() {
        let mut buf = Buffer::new("a");
        let mut vim = Vim::new();
        vim.execute("s/a/b/", &mut buf).expect("ok");
        assert_eq!(vim.search_highlight(), Some("a"));
        vim.execute("undo", &mut buf).expect("ok");
        assert_eq!(buf.to_string(), "a");
        vim.execute("redo", &mut buf).expect("ok");
        assert_eq!(buf.to_string(), "b");
        vim.execute("noh", &mut buf).expect("ok");
        assert_eq!(vim.search_highlight(), None);

        vim.execute("set sw=4 noic", &mut buf).expect("ok");
        assert_eq!(
            (vim.options().shiftwidth, vim.options().ignorecase),
            (4, false)
        );
        let msg = vim.execute("set rnu? sw", &mut buf).expect("ok").message;
        assert_eq!(msg.as_deref(), Some("relativenumber  shiftwidth=4"));
        vim.execute("set rnu!", &mut buf).expect("ok");
        assert!(!vim.options().relativenumber);
        assert_eq!(
            vim.execute("set bogus", &mut buf)
                .expect_err("unknown")
                .to_string(),
            "E518: Unknown option: bogus"
        );
    }

    #[test]
    fn app_commands_go_back_to_the_app() {
        let app = |cmd: &str| ex("|x", cmd).1.unwrap_or_else(|e| panic!("{cmd}: {e}")).app;
        assert_eq!(
            app("w"),
            Some(AppCommand::Write {
                path: None,
                force: false
            })
        );
        assert_eq!(
            app("w! copy.md"),
            Some(AppCommand::Write {
                path: Some("copy.md".into()),
                force: true
            })
        );
        assert_eq!(app("wq"), Some(AppCommand::WriteQuit { force: false }));
        assert_eq!(app("x"), Some(AppCommand::WriteQuit { force: false }));
        assert_eq!(app("q!"), Some(AppCommand::Quit { force: true }));
        assert_eq!(
            app("e notes/x.md"),
            Some(AppCommand::Edit {
                path: Some("notes/x.md".into()),
                force: false
            })
        );
        assert_eq!(
            app("view source"),
            Some(AppCommand::View(Some("source".into())))
        );
        assert_eq!(app("explore"), Some(AppCommand::Explore(None)));
        assert_eq!(
            app("vs ideas"),
            Some(AppCommand::Split {
                vertical: true,
                path: Some("ideas".into())
            })
        );
        assert_eq!(
            app("sp"),
            Some(AppCommand::Split {
                vertical: false,
                path: None
            })
        );
        assert_eq!(app("clo"), Some(AppCommand::Close));
        assert_eq!(app("on"), Some(AppCommand::Only));
        assert_eq!(
            app("find road"),
            Some(AppCommand::Find(Some("road".into())))
        );
        assert_eq!(app("daily"), Some(AppCommand::Daily));
        assert_eq!(err("|x", "q now"), "E488: Trailing characters: now");
    }

    #[test]
    fn errors_use_vim_numbers() {
        assert_eq!(
            err("|x", "frobnicate"),
            "E492: Not an editor command: frobnicate"
        );
        assert!(err("|x", r"s/\(a\)\1/b/").starts_with("E383"));
        assert!(err("|x", "s//y/").starts_with("E35"));
        assert!(err("|x", "&").starts_with("E35"));
    }

    #[test]
    fn registers_list_shows_contents() {
        let mut buf = Buffer::new("hello\n");
        let mut vim = Vim::new();
        vim.execute("y a", &mut buf).expect("ok");
        let list = vim
            .execute("reg", &mut buf)
            .expect("ok")
            .message
            .expect("message");
        assert!(list.contains("\"a   hello^J"), "{list}");
    }

    #[test]
    fn plan_matches_what_runs() {
        let text = Rope::from_str("modal\na modal and a modal\nnone\n");
        let p = Pattern::new("modal", false, false).expect("ok");
        let r = Replacement::new("dialog", None);
        let plan = plan_substitute(&text, 0..=2, &p, &r, true);
        assert_eq!(plan.lines, 2);
        assert_eq!(plan.edits.len(), 3);
        assert_eq!(plan.edits[1].0, 8..13);
    }
}

#[cfg(test)]
mod budget {
    use super::*;
    use std::time::{Duration, Instant};

    /// Whole-note commands on a 1 MB note. `plan_substitute` is what the live
    /// `:s` preview (AMI-161) runs while you type, so it gets the tightest
    /// bound. Run with `cargo test -p mnote-vim --release -- --ignored`.
    #[test]
    #[ignore = "timing test; run with --release -- --ignored"]
    fn whole_note_commands_on_one_megabyte() {
        let block = "## Section\n\nSome modal text with a modal word and (brackets).\n\n- item 42\n- item 7\n\n";
        let note = block.repeat(1_048_576 / block.len() + 1);
        let rope = Rope::from_str(&note);
        let pattern = Pattern::new("modal", false, false).expect("ok");
        let replacement = Replacement::new("dialog", None);
        let t = Instant::now();
        let plan = plan_substitute(&rope, 0..=last_line(&rope), &pattern, &replacement, true);
        let planning = t.elapsed();

        let mut timings = vec![("plan %s preview", planning)];
        for cmd in ["%s/modal/dialog/g", "g/^- item/d", "sort u", "undo"] {
            let mut buf = Buffer::new(&note);
            let mut vim = Vim::new();
            if cmd == "undo" {
                vim.execute("%s/modal/dialog/g", &mut buf).expect("ok");
            }
            let t = Instant::now();
            vim.execute(cmd, &mut buf).expect("ok");
            timings.push((cmd, t.elapsed()));
        }
        for (name, took) in &timings {
            eprintln!("{name:>20}: {took:?}");
        }
        assert_eq!(plan.edits.len(), note.matches("modal").count());
        assert_eq!(
            plan.lines,
            note.lines().filter(|l| l.contains("modal")).count()
        );
        assert!(
            planning < Duration::from_millis(60),
            "planning took {planning:?}"
        );
        for (name, took) in &timings {
            assert!(*took < Duration::from_millis(250), "{name} took {took:?}");
        }
    }
}
