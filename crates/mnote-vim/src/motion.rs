//! Motions: where a key sends the cursor, and how an operator should treat the
//! span it covers.

use std::ops::ControlFlow;

use ropey::Rope;

use crate::text::{
    at_column, char_at, column, first_non_blank, is_blank_line, last_line, line_count, line_end,
    line_len, line_of, line_start,
};

/// A cursor movement. Counts are applied by [`Motion::apply`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    /// `w` / `W`
    WordForward {
        big: bool,
    },
    /// `b` / `B`
    WordBackward {
        big: bool,
    },
    /// `e` / `E`
    WordEnd {
        big: bool,
    },
    /// `0`
    LineStart,
    /// `^`
    FirstNonBlank,
    /// `$`
    LineEnd,
    /// `gg`: first line, or line `count`.
    FirstLine,
    /// `G`: last line, or line `count`.
    LastLine,
    /// `f` `t` `F` `T`
    Find(Find),
    /// `%`: the matching bracket, or with a count, that percentage of the note.
    MatchingBracket,
    /// `}`
    ParagraphForward,
    /// `{`
    ParagraphBackward,
    /// `]]`: next Markdown heading.
    NextHeading,
    /// `[[`: previous Markdown heading.
    PrevHeading,
    /// One added with [`crate::ext::register_motion`], by id.
    Custom(u16),
}

/// A character search within the line: `f`, `t`, `F` or `T`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Find {
    pub ch: char,
    pub forward: bool,
    /// `t`/`T`: stop one character before the match.
    pub till: bool,
}

impl Find {
    /// The same search in the other direction, for `,`.
    pub fn reversed(self) -> Self {
        Find {
            forward: !self.forward,
            ..self
        }
    }
}

/// How an operator treats the text between the cursor and a motion's target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionKind {
    /// The target character is not included (`w`, `b`, `h`).
    Exclusive,
    /// The target character is included (`e`, `$`, `f`).
    Inclusive,
    /// Whole lines (`j`, `k`, `G`).
    Linewise,
}

/// Where a motion lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub pos: usize,
    pub kind: MotionKind,
    /// For `j`/`k`: keep aiming for this column on later vertical moves.
    pub keep_column: bool,
}

impl Motion {
    pub fn kind(self) -> MotionKind {
        use Motion::*;
        match self {
            Up | Down | FirstLine | LastLine => MotionKind::Linewise,
            WordEnd { .. } | LineEnd | MatchingBracket => MotionKind::Inclusive,
            Find(f) if f.forward => MotionKind::Inclusive,
            _ => MotionKind::Exclusive,
        }
    }

    /// Applies the motion `count` times (at least once) from `pos`.
    ///
    /// `want_col` is the column `j`/`k` aim for. `past_end` allows the cursor
    /// to stop after the last character of a line (insert and visual mode).
    /// Returns `None` when the motion can't move (an `f` with no match), so
    /// the cursor stays put, as in Vim.
    pub fn apply(
        self,
        text: &Rope,
        pos: usize,
        count: Option<usize>,
        want_col: Option<usize>,
        past_end: bool,
    ) -> Option<Target> {
        let n = count.unwrap_or(1).max(1);
        if let Motion::Custom(id) = self {
            let (run, _) = crate::ext::motion(id)?;
            return run(text, pos, n);
        }
        let len = text.len_chars();
        let line = line_of(text, pos);
        let target = |pos| Target {
            pos,
            kind: self.kind(),
            keep_column: false,
        };
        let pos = match self {
            Motion::Left => pos.saturating_sub(n).max(line_start(text, line)),
            Motion::Right => {
                let end = line_end(text, line);
                let max = if past_end {
                    end
                } else {
                    end.saturating_sub(1).max(line_start(text, line))
                };
                (pos + n).min(max)
            }
            Motion::Up | Motion::Down => {
                let to = if self == Motion::Up {
                    line.saturating_sub(n)
                } else {
                    (line + n).min(last_line(text))
                };
                let col = want_col.unwrap_or_else(|| column(text, pos));
                return Some(Target {
                    pos: at_column(text, to, col, past_end),
                    kind: MotionKind::Linewise,
                    keep_column: true,
                });
            }
            Motion::WordForward { big } => repeat(n, pos, |p| word_forward(text, p, big)),
            Motion::WordBackward { big } => repeat(n, pos, |p| word_backward(text, p, big)),
            Motion::WordEnd { big } => repeat(n, pos, |p| word_end(text, p, big)),
            Motion::LineStart => line_start(text, line),
            Motion::FirstNonBlank => first_non_blank(text, line),
            Motion::LineEnd => {
                let to = (line + n - 1).min(last_line(text));
                let end = line_end(text, to);
                if past_end || line_len(text, to) == 0 {
                    end
                } else {
                    end - 1
                }
            }
            Motion::FirstLine => first_non_blank(text, count.unwrap_or(1).max(1) - 1),
            Motion::LastLine => {
                let to = count.map_or(last_line(text), |c| c.max(1) - 1);
                first_non_blank(text, to.min(last_line(text)))
            }
            Motion::Find(find) => find_in_line(text, pos, find, n, false)?,
            Motion::MatchingBracket => match count {
                Some(percent) => {
                    let to = (percent.min(100) * line_count(text)).div_ceil(100).max(1) - 1;
                    return Some(Target {
                        pos: first_non_blank(text, to),
                        kind: MotionKind::Linewise,
                        keep_column: false,
                    });
                }
                None => matching_bracket(text, pos)?,
            },
            Motion::ParagraphForward => repeat(n, pos, |p| paragraph_forward(text, p)),
            Motion::ParagraphBackward => repeat(n, pos, |p| paragraph_backward(text, p)),
            Motion::NextHeading => {
                // The nth heading after the cursor's line, or the last line.
                let mut seen = 0;
                let mut to = last_line(text);
                scan_headings(text, |h| {
                    if h > line {
                        seen += 1;
                        if seen == n {
                            to = h;
                            return ControlFlow::Break(());
                        }
                    }
                    ControlFlow::Continue(())
                });
                line_start(text, to)
            }
            // Handled above.
            Motion::Custom(_) => return None,
            Motion::PrevHeading => {
                // The nth heading before the cursor's line, or the first line.
                let mut before = Vec::with_capacity(n);
                scan_headings(text, |h| {
                    if h >= line {
                        return ControlFlow::Break(());
                    }
                    if before.len() == n {
                        before.remove(0);
                    }
                    before.push(h);
                    ControlFlow::Continue(())
                });
                let to = if before.len() == n { before[0] } else { 0 };
                line_start(text, to)
            }
        };
        Some(target(pos.min(len)))
    }
}

/// Repeats a remembered `f`/`t` search for `;` and `,`. A repeated `t`/`T` skips a
/// match right next to the cursor, so it doesn't get stuck (Vim's `cpo-;`).
pub fn repeat_find(text: &Rope, pos: usize, find: Find, count: Option<usize>) -> Option<Target> {
    let n = count.unwrap_or(1).max(1);
    let pos = find_in_line(text, pos, find, n, true)?;
    Some(Target {
        pos,
        kind: Motion::Find(find).kind(),
        keep_column: false,
    })
}

fn repeat(n: usize, start: usize, step: impl Fn(usize) -> usize) -> usize {
    (0..n).fold(start, |p, _| step(p))
}

/// Vim's three character classes: blanks, keyword characters, punctuation.
/// With `big` (WORD motions) there are only blanks and everything else.
fn class(c: char, big: bool) -> u8 {
    if c.is_whitespace() {
        0
    } else if big || c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

fn is_empty_line_at(text: &Rope, pos: usize) -> bool {
    char_at(text, pos) == Some('\n') && (pos == 0 || text.char(pos - 1) == '\n')
}

fn word_forward(text: &Rope, pos: usize, big: bool) -> usize {
    let len = text.len_chars();
    let Some(c) = char_at(text, pos) else {
        return pos;
    };
    let mut i = pos;
    let start = class(c, big);
    if start != 0 {
        while i < len && class(text.char(i), big) == start {
            i += 1;
        }
    }
    while i < len && class(text.char(i), big) == 0 {
        // An empty line counts as a word.
        if i != pos && is_empty_line_at(text, i) {
            return i;
        }
        i += 1;
    }
    // Running off the end lands just past the last character: an operator
    // (`dw` on the last word) then covers it, and a plain cursor move is
    // clamped back onto it.
    i.min(len)
}

fn word_end(text: &Rope, pos: usize, big: bool) -> usize {
    let len = text.len_chars();
    let mut i = pos + 1;
    while i < len && class(text.char(i), big) == 0 {
        i += 1;
    }
    if i >= len {
        return len.saturating_sub(1).max(pos);
    }
    let cls = class(text.char(i), big);
    while i + 1 < len && class(text.char(i + 1), big) == cls {
        i += 1;
    }
    i
}

fn word_backward(text: &Rope, pos: usize, big: bool) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut i = pos - 1;
    while i > 0 && class(text.char(i), big) == 0 {
        if is_empty_line_at(text, i) {
            return i;
        }
        i -= 1;
    }
    let cls = class(text.char(i), big);
    if cls == 0 {
        return i;
    }
    while i > 0 && class(text.char(i - 1), big) == cls {
        i -= 1;
    }
    i
}

/// The `n`th match of `find` on the cursor's line.
fn find_in_line(text: &Rope, pos: usize, find: Find, n: usize, repeating: bool) -> Option<usize> {
    let line = line_of(text, pos);
    let (start, end) = (line_start(text, line), line_end(text, line));
    let skip_adjacent = repeating && find.till;
    let mut found = 0;
    if find.forward {
        let from = pos + 1 + usize::from(skip_adjacent);
        for i in from..end {
            if text.char(i) == find.ch {
                found += 1;
                if found == n {
                    return Some(if find.till { i - 1 } else { i });
                }
            }
        }
    } else {
        let below = pos.checked_sub(usize::from(skip_adjacent))?;
        for i in (start..below).rev() {
            if text.char(i) == find.ch {
                found += 1;
                if found == n {
                    return Some(if find.till { i + 1 } else { i });
                }
            }
        }
    }
    None
}

fn matching_bracket(text: &Rope, pos: usize) -> Option<usize> {
    let line = line_of(text, pos);
    let end = line_end(text, line);
    // Use the bracket under the cursor, or the first one after it on the line.
    let (at, open, close, forward) = (pos..end).find_map(|i| {
        let pair = match text.char(i) {
            '(' => ('(', ')', true),
            '[' => ('[', ']', true),
            '{' => ('{', '}', true),
            ')' => ('(', ')', false),
            ']' => ('[', ']', false),
            '}' => ('{', '}', false),
            _ => return None,
        };
        Some((i, pair.0, pair.1, pair.2))
    })?;
    let mut depth = 0usize;
    if forward {
        for i in at..text.len_chars() {
            let c = text.char(i);
            if c == open {
                depth += 1;
            } else if c == close {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
        }
    } else {
        for i in (0..=at).rev() {
            let c = text.char(i);
            if c == close {
                depth += 1;
            } else if c == open {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
        }
    }
    None
}

fn paragraph_forward(text: &Rope, pos: usize) -> usize {
    let last = last_line(text);
    let mut line = line_of(text, pos);
    // Leave the blank lines we're on, then stop at the next blank line.
    while line < last && is_blank_line(text, line) {
        line += 1;
    }
    while line < last {
        line += 1;
        if is_blank_line(text, line) {
            return line_start(text, line);
        }
    }
    let len = text.len_chars();
    let end = line_end(text, last);
    if line_len(text, last) == 0 {
        end
    } else {
        end - 1
    }
    .min(len)
}

fn paragraph_backward(text: &Rope, pos: usize) -> usize {
    let mut line = line_of(text, pos);
    while line > 0 && is_blank_line(text, line) {
        line -= 1;
    }
    while line > 0 {
        line -= 1;
        if is_blank_line(text, line) {
            return line_start(text, line);
        }
    }
    0
}

/// Calls `found` with each line that holds an ATX heading (`#` to `######`)
/// outside fenced code, in order, until it returns `Break`.
///
/// A byte-level state machine over the rope's chunks: no per-line lookups and
/// no allocation, so `]]` stays far inside the keystroke budget on long notes.
/// Markdown markers are ASCII, so bytes are enough.
fn scan_headings(text: &Rope, mut found: impl FnMut(usize) -> ControlFlow<()>) {
    #[derive(Clone, Copy)]
    enum State {
        Indent(u8),
        Run { marker: u8, len: usize },
        Rest,
    }
    let mut fence: Option<(u8, usize)> = None;
    let mut line = 0;
    let mut state = State::Indent(0);

    // Decides a line once its leading run of `#`, backticks or tildes ends.
    // Returns true when the line is a heading.
    let finish =
        |marker: u8, len: usize, next: Option<u8>, fence: &mut Option<(u8, usize)>| match marker {
            b'`' | b'~' if len >= 3 => {
                match *fence {
                    None => *fence = Some((marker, len)),
                    Some((m, l)) if m == marker && len >= l => *fence = None,
                    Some(_) => {}
                }
                false
            }
            b'#' => {
                fence.is_none()
                    && len <= 6
                    && next.is_none_or(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
            }
            _ => false,
        };

    for chunk in text.chunks() {
        for &b in chunk.as_bytes() {
            state = match state {
                State::Indent(n) if b == b' ' => {
                    if n == 3 {
                        State::Rest
                    } else {
                        State::Indent(n + 1)
                    }
                }
                State::Indent(_) if matches!(b, b'#' | b'`' | b'~') => {
                    State::Run { marker: b, len: 1 }
                }
                State::Run { marker, len } if b == marker => State::Run {
                    marker,
                    len: len + 1,
                },
                State::Run { marker, len } => {
                    if finish(marker, len, Some(b), &mut fence) && found(line).is_break() {
                        return;
                    }
                    State::Rest
                }
                State::Indent(_) | State::Rest => State::Rest,
            };
            if b == b'\n' {
                line += 1;
                state = State::Indent(0);
            }
        }
    }
    if let State::Run { marker, len } = state
        && finish(marker, len, None, &mut fence)
    {
        let _ = found(line);
    }
}
