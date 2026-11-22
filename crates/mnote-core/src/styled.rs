//! The styled view of a note line by line: what kind of block a line is in,
//! and its inline formatting with the Markdown markers hidden.
//!
//! On the cursor line the markers stay, drawn quietly, so editing is plain
//! text; on every other line they're hidden and the formatting shows. A
//! [`StyledLine`] keeps a map from source characters to displayed bytes so
//! the caret, selections and search highlights land in the right place.
//!
//! Block structure (code fences, frontmatter, quotes) comes from the
//! tree-sitter tree the buffer keeps; headings, lists and tasks are read from
//! the line itself, which is cheap and right even before a re-parse lands.

use std::ops::Range;

use ropey::Rope;
use tree_sitter::{Node, Point, Tree};

/// Inline formatting of a run of displayed text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inline {
    Bold,
    Italic,
    Code,
    Strike,
    /// Link text (`[text](url)`) or a wikilink.
    Link,
    /// A URL shown on the cursor line.
    Url,
    Tag,
    /// A Markdown marker shown on the cursor line (`**`, `[`, `#`).
    Marker,
}

/// A run of styled text, in displayed bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: Inline,
}

/// One line as the styled view shows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StyledLine {
    pub text: String,
    pub spans: Vec<Span>,
    /// Displayed byte for each source character (and one past the end).
    map: Vec<usize>,
}

impl StyledLine {
    /// Where source character `ch` of the line is in the displayed text. A
    /// hidden character maps to where it would have been.
    pub fn display_byte(&self, ch: usize) -> usize {
        self.map.get(ch).copied().unwrap_or(self.text.len())
    }

    /// The line shown as it is, with no formatting.
    pub fn plain(line: &str) -> Self {
        let mut map: Vec<usize> = line.char_indices().map(|(b, _)| b).collect();
        map.push(line.len());
        StyledLine {
            text: line.to_owned(),
            spans: Vec::new(),
            map,
        }
    }
}

/// Builds a [`StyledLine`]: characters are pushed shown or hidden, and
/// spans are recorded against the displayed text.
struct Builder<'a> {
    chars: Vec<(usize, char)>,
    source: &'a str,
    out: StyledLine,
    markers: bool,
}

impl<'a> Builder<'a> {
    fn new(source: &'a str, markers: bool) -> Self {
        Builder {
            chars: source.char_indices().collect(),
            source,
            out: StyledLine::default(),
            markers,
        }
    }

    /// Shows source characters `range` (char indices).
    fn show(&mut self, range: Range<usize>, style: Option<Inline>) {
        let start = self.out.text.len();
        for i in range {
            self.out.map.push(self.out.text.len());
            self.out.text.push(self.chars[i].1);
        }
        if let Some(style) = style {
            let end = self.out.text.len();
            if end > start {
                self.out.spans.push(Span {
                    range: start..end,
                    style,
                });
            }
        }
    }

    /// A marker: shown quietly on the cursor line, hidden elsewhere.
    fn marker(&mut self, range: Range<usize>) {
        if self.markers {
            self.show(range, Some(Inline::Marker));
        } else {
            for _ in range {
                self.out.map.push(self.out.text.len());
            }
        }
    }

    fn finish(mut self) -> StyledLine {
        self.out.map.push(self.out.text.len());
        let _ = self.source;
        self.out
    }
}

/// Formats one line's inline Markdown. `markers` keeps the markers (the
/// cursor line); otherwise they're hidden. `skip` source characters at the
/// start (a list or heading marker the caller draws) are dropped when
/// markers are hidden.
pub fn style_inline(line: &str, markers: bool, skip: usize) -> StyledLine {
    let mut b = Builder::new(line, markers);
    let n = b.chars.len();
    let skip = skip.min(n);
    b.marker(0..skip);
    let cs: Vec<char> = line.chars().collect();
    let c = |i: usize| cs[i];
    let mut i = skip;
    // Characters inside a code span aren't formatted.
    while i < n {
        let ch = c(i);
        // `code`
        if ch == '`' {
            let ticks = (i..n).take_while(|&k| c(k) == '`').count();
            if let Some(close) = find_run(&b.chars, i + ticks, '`', ticks) {
                b.marker(i..i + ticks);
                b.show(i + ticks..close, Some(Inline::Code));
                b.marker(close..close + ticks);
                i = close + ticks;
                continue;
            }
        }
        // \* escapes
        if ch == '\\' && i + 1 < n && c(i + 1).is_ascii_punctuation() {
            b.marker(i..i + 1);
            b.show(i + 1..i + 2, None);
            i += 2;
            continue;
        }
        // [[wikilink|alias]]
        if ch == '['
            && i + 1 < n
            && c(i + 1) == '['
            && let Some(close) = find_str(&b.chars, i + 2, "]]")
        {
            let pipe = (i + 2..close).find(|&k| c(k) == '|');
            match pipe {
                Some(p) if !markers => {
                    b.marker(i..p + 1);
                    b.show(p + 1..close, Some(Inline::Link));
                }
                _ => {
                    b.marker(i..i + 2);
                    b.show(i + 2..close, Some(Inline::Link));
                }
            }
            b.marker(close..close + 2);
            i = close + 2;
            continue;
        }
        // [text](url)
        if ch == '['
            && let Some(mid) = find_str(&b.chars, i + 1, "](")
            && let Some(end) = (mid + 2..n).find(|&k| c(k) == ')')
            && !(i + 1..mid).any(|k| c(k) == '[')
        {
            b.marker(i..i + 1);
            b.show(i + 1..mid, Some(Inline::Link));
            b.marker(mid..mid + 2);
            if markers {
                b.show(mid + 2..end, Some(Inline::Url));
            } else {
                b.marker(mid + 2..end);
            }
            b.marker(end..end + 1);
            i = end + 1;
            continue;
        }
        // **bold**, __bold__, ~~strike~~
        if (ch == '*' || ch == '_' || ch == '~') && i + 1 < n && c(i + 1) == ch {
            let style = if ch == '~' {
                Inline::Strike
            } else {
                Inline::Bold
            };
            if let Some(close) = find_run(&b.chars, i + 2, ch, 2).filter(|&e| e > i + 2) {
                b.marker(i..i + 2);
                b.show(i + 2..close, Some(style));
                b.marker(close..close + 2);
                i = close + 2;
                continue;
            }
        }
        // *italic*, _italic_ (underscores only at word edges)
        if (ch == '*' || ch == '_') && i + 1 < n && !c(i + 1).is_whitespace() {
            let at_edge = ch == '*' || i == 0 || !c(i - 1).is_alphanumeric();
            if at_edge
                && let Some(close) = find_run(&b.chars, i + 1, ch, 1).filter(|&e| {
                    e > i + 1
                        && !c(e - 1).is_whitespace()
                        && (ch == '*' || e + 1 >= n || !c(e + 1).is_alphanumeric())
                })
            {
                b.marker(i..i + 1);
                b.show(i + 1..close, Some(Inline::Italic));
                b.marker(close..close + 1);
                i = close + 1;
                continue;
            }
        }
        // #tag
        if ch == '#' && (i == 0 || c(i - 1).is_whitespace()) {
            let len = (i + 1..n)
                .take_while(|&k| c(k).is_alphanumeric() || matches!(c(k), '_' | '-' | '/'))
                .count();
            if len > 0 && (i + 1..i + 1 + len).any(|k| !c(k).is_ascii_digit()) {
                b.show(i..i + 1 + len, Some(Inline::Tag));
                i += 1 + len;
                continue;
            }
        }
        b.show(i..i + 1, None);
        i += 1;
    }
    b.finish()
}

/// The first run of exactly `len` `ch`s at or after `from`.
fn find_run(chars: &[(usize, char)], from: usize, ch: char, len: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i].1 == ch {
            let run = chars[i..].iter().take_while(|(_, c)| *c == ch).count();
            if run == len {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

fn find_str(chars: &[(usize, char)], from: usize, s: &str) -> Option<usize> {
    let pat: Vec<char> = s.chars().collect();
    (from..chars.len().saturating_sub(pat.len() - 1)).find(|&i| {
        pat.iter()
            .enumerate()
            .all(|(k, p)| chars.get(i + k).map(|c| c.1) == Some(*p))
    })
}

/// What a line is, for the styled view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    Text,
    /// `#`..`######`; `marker` characters (`## `) come before the text.
    Heading {
        level: u8,
        marker: usize,
    },
    /// `- item`, `* item`, `1. item`; `indent` in columns.
    Bullet {
        indent: usize,
        marker: usize,
        ordered: bool,
    },
    /// `- [ ] item` / `- [x] item`.
    Task {
        indent: usize,
        marker: usize,
        checked: bool,
    },
    /// A line of a `>` quote; `marker` characters (`> `) come first.
    Quote {
        marker: usize,
    },
    /// The first line of a callout (`> [!note] Title`).
    CalloutHead {
        kind: String,
        marker: usize,
    },
    /// A later line of a callout.
    Callout {
        marker: usize,
    },
    /// The opening fence of a code block, with its language and the rows
    /// of its content.
    FenceOpen {
        lang: String,
        block: Range<usize>,
    },
    FenceClose,
    /// A line inside a code block; `block` is the rows of its content.
    Code {
        lang: String,
        block: Range<usize>,
    },
    /// A line of the YAML frontmatter; `first` is the opening `---`.
    Frontmatter {
        first: bool,
    },
    /// `---`, `***` on their own.
    Rule,
    /// A row of a `|` table. `block` is the table's rows; the delimiter
    /// row (`|---|`) draws as a rule.
    Table {
        block: Range<usize>,
        delimiter: bool,
    },
    /// A line that is only an image: `![alt](path)` or `![[path]]`.
    Image {
        src: String,
        alt: String,
    },
}

/// The kind of `line` (0-based). `tree` is the buffer's Markdown tree, if
/// it has one; without it code blocks and frontmatter read as text.
pub fn line_kind(tree: Option<&Tree>, rope: &Rope, line: usize) -> LineKind {
    let text: String = rope
        .line(line)
        .chars()
        .take_while(|&c| c != '\n' && c != '\r')
        .collect();
    if let Some(tree) = tree
        && let Some(kind) = block_kind(tree, rope, line, &text)
    {
        return kind;
    }
    local_kind(&text)
}

/// Kinds that need the surrounding blocks.
fn block_kind(tree: &Tree, rope: &Rope, line: usize, text: &str) -> Option<LineKind> {
    let indent = text.len() - text.trim_start().len();
    let point = Point::new(line, indent);
    let node = tree.root_node().descendant_for_point_range(point, point)?;
    let mut at: Option<Node<'_>> = Some(node);
    while let Some(n) = at {
        match n.kind() {
            "minus_metadata" => {
                return Some(LineKind::Frontmatter {
                    first: n.start_position().row == line,
                });
            }
            "fenced_code_block" => {
                let lang = info_string(n, rope);
                let (start, end) = (n.start_position().row, n.end_position().row);
                let last = if n.end_position().column == 0 {
                    end.saturating_sub(1)
                } else {
                    end
                };
                let closed = last > start && is_fence(&rope.line(last).to_string());
                let block = start + 1..if closed { last } else { last + 1 };
                return Some(if line == start {
                    LineKind::FenceOpen { lang, block }
                } else if line == last && closed {
                    LineKind::FenceClose
                } else {
                    LineKind::Code { lang, block }
                });
            }
            "block_quote" => {
                let first = n.start_position().row;
                let head: String = rope
                    .line(first)
                    .chars()
                    .take_while(|&c| c != '\n')
                    .collect();
                let marker = quote_marker(text);
                return Some(match callout_kind(&head) {
                    Some(kind) if first == line => LineKind::CalloutHead { kind, marker },
                    Some(_) => LineKind::Callout { marker },
                    None => LineKind::Quote { marker },
                });
            }
            "pipe_table" => {
                let (start, end) = (n.start_position().row, n.end_position().row);
                let end = if n.end_position().column == 0 {
                    end
                } else {
                    end + 1
                };
                return Some(LineKind::Table {
                    block: start..end.max(start + 1),
                    delimiter: is_delimiter_row(text),
                });
            }
            _ => {}
        }
        at = n.parent();
    }
    None
}

fn info_string(block: Node<'_>, rope: &Rope) -> String {
    let mut cursor = block.walk();
    for child in block.children(&mut cursor) {
        if child.kind() == "info_string" {
            let range = child.byte_range();
            let (start, end) = (rope.byte_to_char(range.start), rope.byte_to_char(range.end));
            return rope
                .slice(start..end)
                .to_string()
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_lowercase();
        }
    }
    String::new()
}

fn is_fence(text: &str) -> bool {
    let t = text.trim();
    t.starts_with("```") || t.starts_with("~~~")
}

/// Characters of the `>` markers (and one space after each) at the start.
fn quote_marker(text: &str) -> usize {
    let mut chars = 0;
    let mut rest = text;
    loop {
        let trimmed = rest.trim_start_matches(' ');
        let Some(after) = trimmed.strip_prefix('>') else {
            break;
        };
        let after = after.strip_prefix(' ').unwrap_or(after);
        chars += rest.chars().count() - after.chars().count();
        rest = after;
    }
    chars
}

/// `> [!note] Title` → `note`.
fn callout_kind(line: &str) -> Option<String> {
    let rest = line
        .trim_start()
        .strip_prefix('>')?
        .trim_start()
        .strip_prefix("[!")?;
    let end = rest.find(']')?;
    let kind = &rest[..end];
    (!kind.is_empty() && kind.chars().all(|c| c.is_alphanumeric() || c == '-'))
        .then(|| kind.to_lowercase())
}

/// Kinds read from the line alone.
fn local_kind(text: &str) -> LineKind {
    let trimmed = text.trim_start();
    let indent = text.len() - trimmed.len();
    if let Some(level) = heading_level(trimmed) {
        let marker = indent + level + usize::from(trimmed.len() > level);
        return LineKind::Heading {
            level: level as u8,
            marker,
        };
    }
    let rule = trimmed.trim();
    if rule.len() >= 3
        && (rule.chars().all(|c| c == '-' || c == ' ')
            || rule.chars().all(|c| c == '*' || c == ' '))
        && rule.chars().filter(|c| !c.is_whitespace()).count() >= 3
    {
        return LineKind::Rule;
    }
    if trimmed.starts_with('>') {
        return LineKind::Quote {
            marker: quote_marker(text),
        };
    }
    let bullet = ["- ", "* ", "+ "]
        .iter()
        .find(|b| trimmed.starts_with(**b))
        .map(|b| b.len());
    let ordered = {
        let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
        (digits > 0 && trimmed[digits..].starts_with(". ")).then_some(digits + 2)
    };
    if let Some(len) = bullet.or(ordered) {
        let rest = &trimmed[len..];
        let task = [("[ ] ", false), ("[x] ", true), ("[X] ", true)]
            .into_iter()
            .find(|(m, _)| rest.starts_with(m) || rest == m.trim_end());
        let indent_cols = text[..indent]
            .chars()
            .map(|c| if c == '\t' { 4 } else { 1 })
            .sum();
        return match task {
            Some((m, checked)) => LineKind::Task {
                indent: indent_cols,
                marker: indent + len + m.len().min(rest.len()),
                checked,
            },
            None => LineKind::Bullet {
                indent: indent_cols,
                marker: indent + len,
                ordered: ordered.is_some() && bullet.is_none(),
            },
        };
    }
    if let Some((src, alt)) = image_line(trimmed) {
        return LineKind::Image { src, alt };
    }
    if trimmed.starts_with('|') {
        return LineKind::Table {
            block: 0..0,
            delimiter: is_delimiter_row(text),
        };
    }
    LineKind::Text
}

/// `![alt](path "title")` or `![[path|size]]` alone on a line.
fn image_line(text: &str) -> Option<(String, String)> {
    let text = text.trim();
    if let Some(inner) = text.strip_prefix("![[").and_then(|r| r.strip_suffix("]]")) {
        let src = inner.split('|').next().unwrap_or(inner).trim();
        return (!src.is_empty()).then(|| (src.to_owned(), String::new()));
    }
    let rest = text.strip_prefix("![")?;
    let mid = rest.find("](")?;
    let alt = &rest[..mid];
    let target = rest[mid + 2..].strip_suffix(')')?;
    let src = target.split(" \"").next().unwrap_or(target).trim();
    let src = src.trim_start_matches('<').trim_end_matches('>');
    (!src.is_empty() && !src.contains(' ')).then(|| (src.to_owned(), alt.to_owned()))
}

/// `|---|:--:|` and the like.
fn is_delimiter_row(text: &str) -> bool {
    let t = text.trim();
    t.contains('-')
        && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
        && t.contains('|')
}

/// How a table column lines up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// The cells of a table row: character ranges of each cell's trimmed text.
fn cell_ranges(line: &str) -> Vec<Range<usize>> {
    let chars: Vec<char> = line.chars().collect();
    let mut cells = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let mut code = false;
    while i <= chars.len() {
        let end_cell =
            i == chars.len() || (chars[i] == '|' && !code && (i == 0 || chars[i - 1] != '\\'));
        if i < chars.len() && chars[i] == '`' {
            code = !code;
        }
        if end_cell {
            let (mut a, mut b) = (start, i);
            while a < b && chars[a].is_whitespace() {
                a += 1;
            }
            while b > a && chars[b - 1].is_whitespace() {
                b -= 1;
            }
            cells.push(a..b);
            start = i + 1;
        }
        i += 1;
    }
    // A leading and a trailing pipe leave empty cells at the ends.
    let trimmed = line.trim();
    if trimmed.starts_with('|') && cells.first().is_some_and(|c| c.is_empty()) {
        cells.remove(0);
    }
    if trimmed.ends_with('|') && trimmed.len() > 1 && cells.last().is_some_and(|c| c.is_empty()) {
        cells.pop();
    }
    cells
}

/// The text of each cell of a table row, trimmed, pipes removed.
pub fn table_cells(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    cell_ranges(line)
        .into_iter()
        .map(|r| chars[r].iter().collect())
        .collect()
}

/// Column widths (in characters, as displayed) and alignments of a table
/// whose rows are `rows`.
pub fn table_layout(rows: &[String]) -> (Vec<usize>, Vec<Align>) {
    let mut widths: Vec<usize> = Vec::new();
    let mut aligns: Vec<Align> = Vec::new();
    for row in rows {
        let chars: Vec<char> = row.chars().collect();
        let cells = cell_ranges(row);
        if is_delimiter_row(row) {
            aligns = cells
                .iter()
                .map(|r| {
                    let cell: String = chars[r.clone()].iter().collect();
                    match (cell.starts_with(':'), cell.ends_with(':')) {
                        (true, true) => Align::Center,
                        (false, true) => Align::Right,
                        _ => Align::Left,
                    }
                })
                .collect();
            continue;
        }
        for (c, r) in cells.iter().enumerate() {
            let cell: String = chars[r.clone()].iter().collect();
            let shown = style_inline(&cell, false, 0).text.chars().count();
            if c >= widths.len() {
                widths.resize(c + 1, 0);
            }
            widths[c] = widths[c].max(shown);
        }
    }
    (widths, aligns)
}

fn heading_level(text: &str) -> Option<usize> {
    let level = text.bytes().take_while(|&b| b == b'#').count();
    ((1..=6).contains(&level) && (text.len() == level || text[level..].starts_with([' ', '\t'])))
        .then_some(level)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(line: &str, markers: bool) -> (String, Vec<(String, Inline)>) {
        let styled = style_inline(line, markers, 0);
        let spans = styled
            .spans
            .iter()
            .map(|s| (styled.text[s.range.clone()].to_owned(), s.style))
            .collect();
        (styled.text, spans)
    }

    #[test]
    fn markers_hide_off_the_cursor_line() {
        let (text, spans) = shown("A **bold** and *it* with `code`, ~~gone~~.", false);
        assert_eq!(text, "A bold and it with code, gone.");
        assert_eq!(
            spans,
            [
                ("bold".into(), Inline::Bold),
                ("it".into(), Inline::Italic),
                ("code".into(), Inline::Code),
                ("gone".into(), Inline::Strike),
            ]
        );
        let (text, spans) = shown("A **bold** word", true);
        assert_eq!(text, "A **bold** word", "the cursor line keeps its markers");
        assert_eq!(spans[0], ("**".into(), Inline::Marker));
        assert_eq!(spans[1], ("bold".into(), Inline::Bold));
    }

    #[test]
    fn links_wikilinks_and_tags() {
        let (text, spans) = shown(
            "See [the blog](https://zed.dev) and [[gpui notes|GPUI]] #mnote",
            false,
        );
        assert_eq!(text, "See the blog and GPUI #mnote");
        assert_eq!(
            spans,
            [
                ("the blog".into(), Inline::Link),
                ("GPUI".into(), Inline::Link),
                ("#mnote".into(), Inline::Tag),
            ]
        );
        let (text, _) = shown("[[ui-design]] snake_case_word a*b", false);
        assert_eq!(
            text, "ui-design snake_case_word a*b",
            "underscores inside words and lone stars stay"
        );
    }

    #[test]
    fn the_map_follows_hidden_markers() {
        let styled = style_inline("a **b** c", false, 0);
        assert_eq!(styled.text, "a b c");
        assert_eq!(styled.display_byte(0), 0);
        assert_eq!(styled.display_byte(4), 2, "the b after **");
        assert_eq!(styled.display_byte(8), 4, "the c");
        assert_eq!(styled.display_byte(9), 5, "the end");
        let skipped = style_inline("## Title", false, 3);
        assert_eq!(skipped.text, "Title");
    }

    #[test]
    fn kinds_from_the_line() {
        assert_eq!(
            local_kind("## Principles"),
            LineKind::Heading {
                level: 2,
                marker: 3
            }
        );
        assert_eq!(
            local_kind("- item"),
            LineKind::Bullet {
                indent: 0,
                marker: 2,
                ordered: false
            }
        );
        assert_eq!(
            local_kind("  1. one"),
            LineKind::Bullet {
                indent: 2,
                marker: 5,
                ordered: true
            }
        );
        assert_eq!(
            local_kind("- [x] done"),
            LineKind::Task {
                indent: 0,
                marker: 6,
                checked: true
            }
        );
        assert_eq!(
            local_kind("- [ ]"),
            LineKind::Task {
                indent: 0,
                marker: 5,
                checked: false
            }
        );
        assert_eq!(local_kind("---"), LineKind::Rule);
        assert_eq!(local_kind("#tag text"), LineKind::Text);
        assert_eq!(local_kind("> quoted"), LineKind::Quote { marker: 2 });
    }

    #[test]
    fn tables_line_up_in_columns() {
        let rows: Vec<String> = [
            "| Key | Opens |",
            "|---|--:|",
            "| `space f` | Finder |",
            "| e | **Explorer** |",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (widths, aligns) = table_layout(&rows);
        assert_eq!(
            widths,
            [7, 8],
            "widths count shown characters, markers hidden"
        );
        assert_eq!(aligns, [Align::Left, Align::Right]);
        assert_eq!(
            local_kind("|---|:-:|"),
            LineKind::Table {
                block: 0..0,
                delimiter: true
            }
        );
    }

    #[test]
    fn cells_of_a_row() {
        assert_eq!(table_cells("| a | `b|c` | d |"), ["a", "`b|c`", "d"]);
        assert_eq!(table_cells("a | b"), ["a", "b"]);
    }

    #[test]
    fn image_lines() {
        assert_eq!(
            local_kind("![The explorer](assets/explorer.png)"),
            LineKind::Image {
                src: "assets/explorer.png".into(),
                alt: "The explorer".into()
            }
        );
        assert_eq!(
            local_kind("![[diagram.png|300]]"),
            LineKind::Image {
                src: "diagram.png".into(),
                alt: String::new()
            }
        );
        assert_eq!(local_kind("text ![a](b.png)"), LineKind::Text);
    }

    #[test]
    fn kinds_from_the_tree() {
        let text =
            "---\ntags: [a]\n---\n# T\n\n```rust\nlet x = 1;\n```\n\n> [!note] Head\n> body\n";
        let rope = Rope::from_str(text);
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_md::LANGUAGE.into())
            .expect("markdown grammar");
        let tree = parser.parse(text, None).expect("parsed");
        let kinds: Vec<LineKind> = (0..11).map(|l| line_kind(Some(&tree), &rope, l)).collect();
        assert_eq!(kinds[0], LineKind::Frontmatter { first: true });
        assert_eq!(kinds[1], LineKind::Frontmatter { first: false });
        assert_eq!(
            kinds[3],
            LineKind::Heading {
                level: 1,
                marker: 2
            }
        );
        assert_eq!(
            kinds[5],
            LineKind::FenceOpen {
                lang: "rust".into(),
                block: 6..7
            }
        );
        assert_eq!(
            kinds[6],
            LineKind::Code {
                lang: "rust".into(),
                block: 6..7
            }
        );
        assert_eq!(kinds[7], LineKind::FenceClose);
        assert_eq!(
            kinds[9],
            LineKind::CalloutHead {
                kind: "note".into(),
                marker: 2
            }
        );
        assert_eq!(kinds[10], LineKind::Callout { marker: 2 });
    }
}
