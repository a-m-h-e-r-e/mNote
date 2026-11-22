//! What the index reads from a note's text: its title, its headings, its
//! tags and its outgoing `[[links]]`. A single byte-level pass per note, no Markdown parser: indexing a
//! whole vault has to stay well under a second.

use crate::frontmatter::Frontmatter;

/// An ATX heading (`## Principles`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    /// 1 for `#`, up to 6.
    pub level: u8,
    /// The heading text, without the `#`s.
    pub text: String,
    /// 0-based line number in the note, frontmatter included.
    pub line: usize,
}

/// A `[[link]]` out of a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutLink {
    /// [`crate::links::link_key`] of its target.
    pub key: String,
    /// 0-based line number in the note, frontmatter included.
    pub line: usize,
    /// The line it's on, trimmed and cut short.
    pub context: String,
}

/// Context lines are cut to about this many bytes.
const CONTEXT_MAX: usize = 200;

/// The parts of a note the index keeps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Outline {
    /// The first level-1 heading.
    pub title: Option<String>,
    pub headings: Vec<Heading>,
    /// Frontmatter tags, then inline `#tags`, each once, without `#`.
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    pub links: Vec<OutLink>,
}

/// Reads the outline of a note.
pub(crate) fn outline(text: &str) -> Outline {
    let fm = Frontmatter::parse(text);
    let mut out = Outline {
        aliases: fm.aliases,
        ..Outline::default()
    };
    for tag in fm.tags {
        push_tag(&mut out.tags, &tag);
    }

    let body = &text[fm.body_start..];
    let first_line = text[..fm.body_start].matches('\n').count();
    // The open fence: its character and length.
    let mut fence: Option<(u8, usize)> = None;
    for (i, line) in body.lines().enumerate() {
        let indent = line.len() - line.trim_start_matches(' ').len();
        let rest = &line[indent..];
        if indent <= 3
            && let Some((ch, len)) = fence_marker(rest)
        {
            fence = match fence {
                None => Some((ch, len)),
                Some((open, open_len)) if open == ch && len >= open_len => None,
                still => still,
            };
            continue;
        }
        if fence.is_some() {
            continue;
        }
        if indent <= 3
            && let Some((level, heading)) = atx_heading(rest)
        {
            if level == 1 && out.title.is_none() && !heading.is_empty() {
                out.title = Some(heading.to_owned());
            }
            out.headings.push(Heading {
                level,
                text: heading.to_owned(),
                line: first_line + i,
            });
        }
        inline_tags(line, &mut out.tags);
        if line.contains("[[") {
            let links = crate::links::wikilinks(line);
            if !links.is_empty() {
                let context = cut(line.trim(), CONTEXT_MAX);
                for link in links {
                    out.links.push(OutLink {
                        key: link.key(),
                        line: first_line + i,
                        context: context.clone(),
                    });
                }
            }
        }
    }
    out
}

/// `text` cut to at most `max` bytes on a character boundary, with `…`.
fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// "```" or "~~~" (three or more) at the start of `line`.
fn fence_marker(line: &str) -> Option<(u8, usize)> {
    let ch = *line.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = line.bytes().take_while(|&b| b == ch).count();
    (len >= 3).then_some((ch, len))
}

/// `## Text ##` → `(2, "Text")`.
fn atx_heading(line: &str) -> Option<(u8, &str)> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &line[level..];
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let mut text = rest.trim();
    // A closing run of `#`s after a space is not part of the text.
    let without = text.trim_end_matches('#');
    if without.is_empty() || without.ends_with([' ', '\t']) {
        text = without.trim_end();
    }
    Some((level as u8, text))
}

/// `#tag` and `#nested/tag` outside inline code. A tag starts after a space
/// (or the line start) and needs at least one character that isn't a digit,
/// so `#1` and URL anchors (`page#part`) aren't tags.
fn inline_tags(line: &str, tags: &mut Vec<String>) {
    let bytes = line.as_bytes();
    let mut in_code = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'`' => in_code = !in_code,
            b'#' if !in_code && (i == 0 || bytes[i - 1].is_ascii_whitespace()) => {
                let rest = &line[i + 1..];
                let len = rest
                    .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '/')))
                    .unwrap_or(rest.len());
                let tag = rest[..len].trim_end_matches('/');
                if tag.chars().any(|c| !c.is_ascii_digit()) {
                    push_tag(tags, tag);
                }
                i += 1 + len;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
}

/// Adds a tag unless it's already there in any letter case.
fn push_tag(tags: &mut Vec<String>, tag: &str) {
    let tag = tag.trim_start_matches('#');
    if !tag.is_empty() && !tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
        tags.push(tag.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_headings_and_lines() {
        let o = outline(
            "---\ntags: [mnote]\n---\n# UI design\n\nText\n## Principles ##\n### C# notes\n#nope\n",
        );
        assert_eq!(o.title.as_deref(), Some("UI design"));
        let got: Vec<(u8, &str, usize)> = o
            .headings
            .iter()
            .map(|h| (h.level, h.text.as_str(), h.line))
            .collect();
        assert_eq!(
            got,
            [
                (1, "UI design", 3),
                (2, "Principles", 6),
                (3, "C# notes", 7)
            ]
        );
    }

    #[test]
    fn code_fences_hide_headings_and_tags() {
        let o = outline(
            "# Title\n```rust\n# not a heading #notag\n```\n~~~\n## also not\n````\n~~~\n## Real #yes\n",
        );
        let texts: Vec<&str> = o.headings.iter().map(|h| h.text.as_str()).collect();
        assert_eq!(texts, ["Title", "Real #yes"]);
        assert_eq!(o.tags, ["yes"]);
    }

    #[test]
    fn tags_from_frontmatter_and_text() {
        let o = outline(
            "---\ntags: mnote, Design\naliases: [UI]\n---\nSee #design and #gpui/notes, not `#code`, #42 or a.md#part.\n#rust-lang,\n",
        );
        assert_eq!(o.tags, ["mnote", "Design", "gpui/notes", "rust-lang"]);
        assert_eq!(o.aliases, ["UI"]);
        assert_eq!(o.title, None, "no level-1 heading");
    }

    #[test]
    fn odd_input_never_panics() {
        for text in [
            "",
            "#",
            "######",
            "####### seven",
            "#\u{e9}t\u{e9} caf\u{e9}",
            "```",
            "---\n",
        ] {
            let _ = outline(text);
        }
        assert_eq!(outline("#été").tags, ["été"]);
    }
}
