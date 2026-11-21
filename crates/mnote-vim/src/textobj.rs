//! Text objects: `iw aw iW aW ip ap i" a" i' a' i` a` i( a( ib i[ a[ i{ a{ iB
//! i< a<`. Each returns a range and whether it covers whole lines.

use std::ops::Range;

use ropey::Rope;

use crate::text::{char_at, is_blank_line, last_line, line_end, line_of, line_start};

/// A selected span. `end` is exclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub range: Range<usize>,
    pub linewise: bool,
}

/// Selects the text object `obj` (the key after `i` or `a`) at `pos`.
pub fn select(text: &Rope, pos: usize, obj: char, around: bool, count: usize) -> Option<Selection> {
    let count = count.max(1);
    let charwise = |range| Selection {
        range,
        linewise: false,
    };
    match obj {
        'w' | 'W' => word(text, pos, obj == 'W', around, count).map(charwise),
        'p' => paragraph(text, pos, around, count),
        '"' | '\'' | '`' => quote(text, pos, obj, around).map(charwise),
        '(' | ')' | 'b' => brackets(text, pos, '(', ')', around, count).map(charwise),
        '[' | ']' => brackets(text, pos, '[', ']', around, count).map(charwise),
        '{' | '}' | 'B' => brackets(text, pos, '{', '}', around, count).map(charwise),
        '<' | '>' => brackets(text, pos, '<', '>', around, count).map(charwise),
        _ => None,
    }
}

fn class(c: char, big: bool) -> u8 {
    if c == ' ' || c == '\t' {
        0
    } else if c == '\n' {
        3
    } else if big || c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// `iw`: the run of same-class characters (word, punctuation or blanks) at
/// `pos`. `aw`: the word plus the blanks after it, or before it if none.
fn word(text: &Rope, pos: usize, big: bool, around: bool, count: usize) -> Option<Range<usize>> {
    let len = text.len_chars();
    let c = char_at(text, pos)?;
    if c == '\n' {
        return None;
    }
    let run = |from: usize| {
        let cls = class(text.char(from), big);
        let mut start = from;
        while start > 0 && class(text.char(start - 1), big) == cls {
            start -= 1;
        }
        let mut end = from;
        while end < len && class(text.char(end), big) == cls {
            end += 1;
        }
        start..end
    };
    let first = run(pos);
    let mut range = first.clone();
    for _ in 1..count {
        if range.end >= len || text.char(range.end) == '\n' {
            break;
        }
        range.end = run(range.end).end;
    }
    if around {
        let on_blank = class(text.char(first.start), big) == 0;
        let trailing =
            (range.end < len && class(text.char(range.end), big) == 0).then(|| run(range.end).end);
        match (on_blank, trailing) {
            // On blanks, `aw` takes the blanks and the following word.
            (true, _) if range.end < len && text.char(range.end) != '\n' => {
                range.end = run(range.end).end;
            }
            (false, Some(end)) => range.end = end,
            // No blanks after: take the ones before instead.
            (false, None) if range.start > 0 && class(text.char(range.start - 1), big) == 0 => {
                range.start = run(range.start - 1).start;
            }
            _ => {}
        }
    }
    Some(range)
}

/// `ip`: the paragraph (or run of blank lines) around the cursor. `ap`: plus
/// the blank lines after it (or before it, at the end of the note).
fn paragraph(text: &Rope, pos: usize, around: bool, count: usize) -> Option<Selection> {
    let last = last_line(text);
    let line = line_of(text, pos);
    let blank = is_blank_line(text, line);
    let mut first = line;
    while first > 0 && is_blank_line(text, first - 1) == blank {
        first -= 1;
    }
    let mut end = line;
    let mut groups = 1;
    loop {
        while end < last && is_blank_line(text, end + 1) == is_blank_line(text, end) {
            end += 1;
        }
        if groups == count || end == last {
            break;
        }
        end += 1;
        groups += 1;
    }
    if around {
        if end < last && is_blank_line(text, end + 1) != is_blank_line(text, end) {
            end += 1;
            while end < last && is_blank_line(text, end + 1) == is_blank_line(text, end) {
                end += 1;
            }
        } else {
            while first > 0 && is_blank_line(text, first - 1) != blank {
                first -= 1;
                if !is_blank_line(text, first) {
                    first += 1;
                    break;
                }
            }
        }
    }
    let start = line_start(text, first);
    let stop = if end < last {
        line_start(text, end + 1)
    } else {
        text.len_chars()
    };
    Some(Selection {
        range: start..stop,
        linewise: true,
    })
}

/// `i"`: inside the quotes around (or after) the cursor on its line. `a"`:
/// the quotes too, plus the blanks after them (or before them).
fn quote(text: &Rope, pos: usize, q: char, around: bool) -> Option<Range<usize>> {
    let line = line_of(text, pos);
    let (start, end) = (line_start(text, line), line_end(text, line));
    let mut quotes = Vec::new();
    for i in start..end {
        if text.char(i) == q && (i == start || text.char(i - 1) != '\\') {
            quotes.push(i);
        }
    }
    let pairs = || quotes.as_chunks::<2>().0.iter().map(|&[o, c]| (o, c));
    let (open, close) = pairs()
        .find(|&(o, c)| o <= pos && pos <= c)
        .or_else(|| pairs().find(|&(o, _)| o > pos))?;
    if !around {
        return Some(open + 1..close);
    }
    let mut range = open..close + 1;
    let blank = |i: usize| matches!(text.char(i), ' ' | '\t');
    if range.end < end && blank(range.end) {
        while range.end < end && blank(range.end) {
            range.end += 1;
        }
    } else {
        while range.start > start && blank(range.start - 1) {
            range.start -= 1;
        }
    }
    Some(range)
}

/// `i(`: inside the `count`th pair of brackets around the cursor, across
/// lines. `a(`: the brackets too.
fn brackets(
    text: &Rope,
    pos: usize,
    open: char,
    close: char,
    around: bool,
    count: usize,
) -> Option<Range<usize>> {
    let len = text.len_chars();
    let mut from = pos.min(len.saturating_sub(1));
    let mut found = None;
    for level in 0..count {
        // Walk back to the unmatched opening bracket.
        let mut depth = 0usize;
        let mut i = from;
        let start_on_close = level == 0 && char_at(text, i) == Some(close);
        let o = loop {
            let c = char_at(text, i)?;
            if c == close && !(start_on_close && i == from) && !(level > 0 && i == from) {
                depth += 1;
            } else if c == open {
                if depth == 0 {
                    break i;
                }
                depth -= 1;
            }
            if i == 0 {
                return None;
            }
            i -= 1;
        };
        // And forward to its match.
        let mut depth = 0usize;
        let mut j = o + 1;
        let c = loop {
            match char_at(text, j)? {
                ch if ch == open => depth += 1,
                ch if ch == close => {
                    if depth == 0 {
                        break j;
                    }
                    depth -= 1;
                }
                _ => {}
            }
            j += 1;
        };
        found = Some((o, c));
        if o == 0 {
            break;
        }
        from = o - 1;
    }
    let (o, c) = found?;
    Some(if around { o..c + 1 } else { o + 1..c })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(doc: &str, obj: char, around: bool) -> String {
        let pos = doc.find('|').expect("cursor");
        let text = Rope::from_str(&doc.replacen('|', "", 1));
        let s = select(&text, text.byte_to_char(pos), obj, around, 1).expect("selection");
        text.slice(s.range).to_string()
    }

    #[test]
    fn words() {
        assert_eq!(sel("one tw|o three", 'w', false), "two");
        assert_eq!(sel("one tw|o three", 'w', true), "two ");
        assert_eq!(sel("one two thr|ee", 'w', true), " three");
        assert_eq!(sel("foo.b|ar(x)", 'w', false), "bar");
        assert_eq!(sel("foo.b|ar(x)", 'W', false), "foo.bar(x)");
        assert_eq!(sel("one | two", 'w', false), "  ");
    }

    #[test]
    fn quotes() {
        assert_eq!(sel(r#"say "he|llo" now"#, '"', false), "hello");
        assert_eq!(sel(r#"say "he|llo" now"#, '"', true), "\"hello\" ");
        assert_eq!(
            sel(r#"s|ay "hello" now"#, '"', false),
            "hello",
            "jumps to the next pair"
        );
        assert_eq!(sel(r#"a "x \" y|" b"#, '"', false), "x \\\" y");
    }

    #[test]
    fn brackets_nest_and_span_lines() {
        assert_eq!(sel("f(a, g(b|), c)", '(', false), "b");
        assert_eq!(sel("f(a, g(b|), c)", '(', true), "(b)");
        assert_eq!(sel("f(a, |g(b), c)", 'b', false), "a, g(b), c");
        assert_eq!(sel("list[1, [2|, 3]]", '[', false), "2, 3");
        assert_eq!(sel("fn x() {\n  bo|dy\n}", '{', false), "\n  body\n");
        assert_eq!(sel("call|(x)", '(', false), "x", "on the open bracket");
        assert_eq!(sel("call(x|)", '(', false), "x", "on the close bracket");
    }

    #[test]
    fn paragraphs_are_linewise() {
        let doc = "one\ntw|o\n\n\nthree\n";
        assert_eq!(sel(doc, 'p', false), "one\ntwo\n");
        assert_eq!(sel(doc, 'p', true), "one\ntwo\n\n\n");
        let pos = doc.find('|').expect("cursor");
        let text = Rope::from_str(&doc.replacen('|', "", 1));
        assert!(select(&text, pos, 'p', false, 1).expect("sel").linewise);
    }

    #[test]
    fn nothing_to_select_is_none() {
        let text = Rope::from_str("no brackets here");
        assert_eq!(select(&text, 3, '(', false, 1), None);
        assert_eq!(select(&text, 3, '"', false, 1), None);
        assert_eq!(select(&Rope::new(), 0, 'w', false, 1), None);
    }
}
