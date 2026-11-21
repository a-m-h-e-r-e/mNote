//! Line and column arithmetic on a rope, the way Vim sees text.
//!
//! All positions are character offsets. A final newline ends the last line
//! rather than starting an empty one, as in Vim.

use ropey::Rope;

/// Number of lines, never less than 1. A trailing newline doesn't add a line.
pub fn line_count(text: &Rope) -> usize {
    let lines = text.len_lines();
    let len = text.len_chars();
    if len > 0 && text.char(len - 1) == '\n' {
        lines - 1
    } else {
        lines
    }
    .max(1)
}

pub fn last_line(text: &Rope) -> usize {
    line_count(text) - 1
}

/// The line holding `pos`. A position at the very end counts as the last line.
pub fn line_of(text: &Rope, pos: usize) -> usize {
    text.char_to_line(pos.min(text.len_chars()))
        .min(last_line(text))
}

/// First character of `line`.
pub fn line_start(text: &Rope, line: usize) -> usize {
    text.line_to_char(line.min(last_line(text)))
}

/// Characters on `line`, without its line ending.
pub fn line_len(text: &Rope, line: usize) -> usize {
    let slice = text.line(line.min(last_line(text)));
    let mut len = slice.len_chars();
    if len > 0 && slice.char(len - 1) == '\n' {
        len -= 1;
        if len > 0 && slice.char(len - 1) == '\r' {
            len -= 1;
        }
    }
    len
}

/// Offset just past the last character of `line` (where its newline sits).
pub fn line_end(text: &Rope, line: usize) -> usize {
    line_start(text, line) + line_len(text, line)
}

/// Column (in characters) of `pos` within its line.
pub fn column(text: &Rope, pos: usize) -> usize {
    pos - line_start(text, line_of(text, pos))
}

/// The position at `col` on `line`, clamped to the line. In normal mode the
/// cursor sits on a character, so the last valid column is one before the end.
pub fn at_column(text: &Rope, line: usize, col: usize, past_end: bool) -> usize {
    let len = line_len(text, line);
    let max = if past_end || len == 0 { len } else { len - 1 };
    line_start(text, line) + col.min(max)
}

/// Keeps a normal-mode cursor on a character of its line.
pub fn clamp_normal(text: &Rope, pos: usize) -> usize {
    let line = line_of(text, pos);
    at_column(text, line, pos - line_start(text, line), false)
}

/// First non-blank character of `line`, or its end if it's all blank.
pub fn first_non_blank(text: &Rope, line: usize) -> usize {
    let start = line_start(text, line);
    let len = line_len(text, line);
    (0..len)
        .find(|&i| !matches!(text.char(start + i), ' ' | '\t'))
        .map_or(start + len.saturating_sub(1).min(len), |i| start + i)
}

pub fn is_blank_line(text: &Rope, line: usize) -> bool {
    line_len(text, line) == 0
}

/// Character at `pos`, or `None` past the end.
pub fn char_at(text: &Rope, pos: usize) -> Option<char> {
    (pos < text.len_chars()).then(|| text.char(pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trailing_newline_does_not_add_a_line() {
        assert_eq!(line_count(&Rope::from_str("a\nb\n")), 2);
        assert_eq!(line_count(&Rope::from_str("a\nb")), 2);
        assert_eq!(line_count(&Rope::from_str("")), 1);
        assert_eq!(line_count(&Rope::from_str("\n")), 1);
    }

    #[test]
    fn columns_and_clamping() {
        let t = Rope::from_str("hello\r\n\n  indented\n");
        assert_eq!(line_len(&t, 0), 5);
        assert_eq!(line_len(&t, 1), 0);
        assert_eq!(at_column(&t, 0, 99, false), 4);
        assert_eq!(at_column(&t, 0, 99, true), 5);
        assert_eq!(
            at_column(&t, 1, 3, false),
            7,
            "empty line: column clamps to its start"
        );
        assert_eq!(first_non_blank(&t, 2), 10);
        assert_eq!(clamp_normal(&t, 5), 4);
    }
}
