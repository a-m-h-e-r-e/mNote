//! Comments (AMI-235): `gc` comments lines in and out with the language's
//! token at that place (a Python fence in a note uses `#`), and `gb` wraps
//! text in its block comment. The tokens come from the language packs
//! (`comment = { line, block }`).
//!
//! As Comment.nvim and Neovim's built-in `gc`: lines are commented when any
//! isn't yet, else uncommented; the tokens line up at the lines' smallest
//! indent with a space after them; blank lines are left alone. A language
//! with only block comments (Markdown, HTML, CSS) comments each line with
//! them.

use std::ops::Range;

use mnote_core::Buffer;
use mnote_core::lang::pack::Comment;
use ropey::Rope;

use crate::text::{line_end, line_start};

/// The language at character `pos` (a fence's inside a note) and its
/// comment tokens, if it has any.
pub(crate) fn tokens_at(buf: &mut Buffer, pos: usize) -> (Option<String>, Option<Comment>) {
    let language = buf
        .syntax_snapshot()
        .map(|s| s.language_at(pos).to_owned())
        .or_else(|| buf.language().map(str::to_owned));
    let comment = language
        .as_deref()
        .and_then(mnote_core::lang::languages::pack)
        .map(|p| p.comment)
        .filter(|c| c.line.is_some() || c.block.is_some());
    (language, comment)
}

/// A line's text (without its break) and where it starts.
fn line(text: &Rope, line: usize) -> (String, usize) {
    let start = line_start(text, line);
    (text.slice(start..line_end(text, line)).to_string(), start)
}

fn lead(s: &str) -> usize {
    s.chars().take_while(|c| c.is_whitespace()).count()
}

/// The edits that comment lines `first..=last` in, or out when all of them
/// are commented: character ranges and their replacements, last first so
/// they can be made in order.
pub(crate) fn toggle_lines(
    text: &Rope,
    first: usize,
    last: usize,
    comment: &Comment,
) -> Vec<(Range<usize>, String)> {
    let (open, close) = match (&comment.line, &comment.block) {
        (Some(token), _) => (token.clone(), None),
        (None, Some((open, close))) => (open.clone(), Some(close.clone())),
        (None, None) => return Vec::new(),
    };
    let lines: Vec<(usize, String, usize)> = (first..=last)
        .map(|l| {
            let (s, start) = line(text, l);
            (l, s, start)
        })
        .filter(|(_, s, _)| !s.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let commented = |s: &str| {
        let body = s.trim();
        body.starts_with(&open) && close.as_ref().is_none_or(|c| body.ends_with(c.as_str()))
    };
    let mut edits = Vec::new();
    if lines.iter().all(|(_, s, _)| commented(s)) {
        for (_, s, start) in &lines {
            let chars: Vec<char> = s.chars().collect();
            let at = lead(s);
            let mut end = at + open.chars().count();
            if chars.get(end) == Some(&' ') {
                end += 1;
            }
            if let Some(close) = &close {
                // Its closing token, with the space before it.
                let trailing = chars.len() - s.trim_end().chars().count();
                let mut from = chars.len() - trailing - close.chars().count();
                if from > end && chars.get(from - 1) == Some(&' ') {
                    from -= 1;
                }
                edits.push((
                    start + from.max(end)..start + chars.len() - trailing,
                    String::new(),
                ));
            }
            edits.push((start + at..start + end, String::new()));
        }
    } else {
        let indent = lines.iter().map(|(_, s, _)| lead(s)).min().unwrap_or(0);
        for (_, s, start) in &lines {
            if let Some(close) = &close {
                let end = s.trim_end().chars().count();
                edits.push((start + end..start + end, format!(" {close}")));
            }
            edits.push((start + indent..start + indent, format!("{open} ")));
        }
    }
    // Last first: each range stays right while the ones after it change.
    edits.sort_by_key(|(r, _)| std::cmp::Reverse(r.start));
    edits
}

/// The edits that wrap `range` in the block comment `open`…`close`, or
/// unwrap it when it already is one. Whole lines (`linewise`) are wrapped
/// from their first character to their last, leaving the indent outside.
pub(crate) fn toggle_block(
    text: &Rope,
    range: Range<usize>,
    linewise: bool,
    open: &str,
    close: &str,
) -> Vec<(Range<usize>, String)> {
    let (start, end) = if linewise {
        let piece: String = text.slice(range.clone()).to_string();
        let first = range.start + lead(&piece);
        let last = range.start + piece.trim_end().chars().count();
        (first, last.max(first))
    } else {
        (range.start, range.end)
    };
    let inner: String = text.slice(start..end).to_string();
    let n = |s: &str| s.chars().count();
    if inner.starts_with(open) && inner.ends_with(close) && n(&inner) >= n(open) + n(close) {
        let chars: Vec<char> = inner.chars().collect();
        let mut after_open = n(open);
        if chars.get(after_open) == Some(&' ') {
            after_open += 1;
        }
        let mut before_close = chars.len() - n(close);
        if before_close > after_open && chars.get(before_close - 1) == Some(&' ') {
            before_close -= 1;
        }
        vec![
            (start + before_close.max(after_open)..end, String::new()),
            (start..start + after_open, String::new()),
        ]
    } else {
        vec![
            (end..end, format!(" {close}")),
            (start..start, format!("{open} ")),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, edits: Vec<(Range<usize>, String)>) -> String {
        let mut rope = Rope::from_str(text);
        for (r, s) in edits {
            rope.remove(r.clone());
            rope.insert(r.start, &s);
        }
        rope.to_string()
    }

    fn slashes() -> Comment {
        Comment {
            line: Some("//".into()),
            block: Some(("/*".into(), "*/".into())),
        }
    }

    fn html() -> Comment {
        Comment {
            line: None,
            block: Some(("<!--".into(), "-->".into())),
        }
    }

    #[test]
    fn lines_are_commented_at_their_smallest_indent_and_back() {
        let text = "fn f() {\n    a();\n\n        b();\n}\n";
        let rope = Rope::from_str(text);
        let once = apply(text, toggle_lines(&rope, 1, 3, &slashes()));
        assert_eq!(once, "fn f() {\n    // a();\n\n    //     b();\n}\n");
        let back = apply(
            &once,
            toggle_lines(&Rope::from_str(&once), 1, 3, &slashes()),
        );
        assert_eq!(back, text);
    }

    #[test]
    fn a_mix_is_commented_and_only_all_commented_is_uncommented() {
        let text = "// a\nb\n";
        let once = apply(text, toggle_lines(&Rope::from_str(text), 0, 1, &slashes()));
        assert_eq!(once, "// // a\n// b\n");
        // Without the space after the token, it still comes out.
        let tight = "//a\n";
        let out = apply(
            tight,
            toggle_lines(&Rope::from_str(tight), 0, 0, &slashes()),
        );
        assert_eq!(out, "a\n");
    }

    #[test]
    fn block_only_languages_wrap_each_line() {
        let text = "One.\n  Two.\n";
        let once = apply(text, toggle_lines(&Rope::from_str(text), 0, 1, &html()));
        assert_eq!(once, "<!-- One. -->\n<!--   Two. -->\n");
        let back = apply(&once, toggle_lines(&Rope::from_str(&once), 0, 1, &html()));
        assert_eq!(back, text);
    }

    #[test]
    fn blocks_wrap_and_unwrap() {
        let text = "    let x = f(a, b);\n";
        let rope = Rope::from_str(text);
        let lines = apply(text, toggle_block(&rope, 0..text.len(), true, "/*", "*/"));
        assert_eq!(lines, "    /* let x = f(a, b); */\n");
        let rope = Rope::from_str(&lines);
        let back = apply(
            &lines,
            toggle_block(&rope, 0..lines.len(), true, "/*", "*/"),
        );
        assert_eq!(back, text);
        // Charwise: just the range.
        let at = text.find("a, b").expect("args");
        let part = apply(
            text,
            toggle_block(&rope_of(text), at..at + 4, false, "/*", "*/"),
        );
        assert_eq!(part, "    let x = f(/* a, b */);\n");
    }

    fn rope_of(s: &str) -> Rope {
        Rope::from_str(s)
    }

    /// Runs `keys` on `before` (`|` the cursor) in `language`, after the
    /// first parse; the text with `|`, and the message shown, if any.
    fn run(language: &str, before: &str, keys: &str) -> (String, Option<String>) {
        let at = before.find('|').expect("a cursor");
        let text = before.replacen('|', "", 1);
        let mut buf = match language {
            "markdown" => Buffer::new(&text),
            "plain" => Buffer::new_plain(&text),
            _ => Buffer::with_language(&text, language),
        };
        let _ = buf.syntax_tree_now();
        let mut vim = crate::Vim::new();
        vim.set_cursor(buf.text().byte_to_char(at));
        for key in crate::parse_keys(keys) {
            vim.handle_key(key, &mut buf);
        }
        let mut shown = buf.to_string();
        shown.insert(
            buf.text().char_to_byte(vim.cursor().min(buf.len_chars())),
            '|',
        );
        (shown, vim.message().map(|m| m.text.clone()))
    }

    #[track_caller]
    fn check(language: &str, before: &str, keys: &str, after: &str) {
        assert_eq!(
            run(language, before, keys).0,
            after,
            "{keys:?} on {before:?}"
        );
    }

    #[test]
    fn gcc_and_gc_with_a_motion_in_rust_and_python() {
        check(
            "rust",
            "fn f() {\n    |a();\n}\n",
            "gcc",
            "fn f() {\n    // |a();\n}\n",
        );
        check(
            "rust",
            "fn f() {\n    // |a();\n}\n",
            "gcc",
            "fn f() {\n    |a();\n}\n",
        );
        check(
            "rust",
            "|fn f() {\n    a();\n}\n",
            "gcj",
            "// |fn f() {\n//     a();\n}\n",
        );
        check("python", "|x = 1\ny = 2\n", "2gcc", "# |x = 1\n# y = 2\n");
    }

    #[test]
    fn visual_dot_repeat_and_undo() {
        check(
            "rust",
            "|a();\nb();\nc();\n",
            "Vjgc",
            "// |a();\n// b();\nc();\n",
        );
        // The cursor stays on its character, so `j` lands three in.
        check("rust", "|a();\nb();\n", "gccj.", "// a();\n// b()|;\n");
        check("rust", "|a();\n", "gccu", "|a();\n");
    }

    #[test]
    fn block_comments_with_gb() {
        check(
            "rust",
            "    |let x = 1;\n",
            "gbc",
            "    /* |let x = 1; */\n",
        );
        check("rust", "f(|a, b);\n", "gbi(", "f(/* |a, b */);\n");
        let (out, message) = run("python", "|x = 1\n", "gbc");
        assert_eq!(out, "|x = 1\n");
        assert_eq!(message.as_deref(), Some("python has no block comments"));
    }

    #[test]
    fn notes_use_each_places_language() {
        // A Python fence uses `#`; the prose around it, HTML comments.
        check(
            "markdown",
            "Text.\n\n```python\n|x = 1\n```\n",
            "gcc",
            "Text.\n\n```python\n# |x = 1\n```\n",
        );
        check("markdown", "|Some text.\n", "gcc", "<!-- |Some text. -->\n");
    }

    #[test]
    fn languages_without_comments_say_so() {
        let (out, message) = run("json", "|{\"a\": 1}\n", "gcc");
        assert_eq!(out, "|{\"a\": 1}\n");
        assert_eq!(message.as_deref(), Some("json has no comments"));
        let (_, message) = run("plain", "|abc\n", "gcc");
        assert_eq!(message.as_deref(), Some("plain text has no comments"));
    }
}
