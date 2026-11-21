//! YAML frontmatter: the `---` block at the top of a note.
//!
//! mNote only needs `tags` and `aliases`, so this is a small purpose-built
//! reader rather than a YAML library. It understands the forms Obsidian
//! writes:
//!
//! ```yaml
//! ---
//! tags: [mnote, design]      # flow list
//! tags: mnote, design        # comma list
//! tag: mnote                 # single value, singular key
//! aliases:                   # block list
//!   - UI design
//!   - "Design: UI"
//! ---
//! ```
//!
//! Anything else in the block is left alone. Malformed frontmatter never fails:
//! it just yields fewer tags.

/// The parts of a note's frontmatter mNote uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    /// Tags without the leading `#`.
    pub tags: Vec<String>,
    /// Other names the note can be linked by.
    pub aliases: Vec<String>,
    /// Byte offset where the note body starts (0 when there's no frontmatter).
    pub body_start: usize,
}

#[derive(Clone, Copy)]
enum Key {
    Tags,
    Aliases,
}

impl Frontmatter {
    /// Reads the frontmatter at the very start of `text`, if there is one.
    pub fn parse(text: &str) -> Self {
        let mut fm = Frontmatter::default();
        let Some(after_open) = strip_fence(text, "---") else {
            return fm;
        };

        let mut offset = text.len() - after_open.len();
        let mut list_key: Option<Key> = None;
        for line in after_open.split_inclusive('\n') {
            offset += line.len();
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if trimmed == "---" || trimmed == "..." {
                fm.body_start = offset;
                return fm;
            }

            let indented = trimmed.starts_with([' ', '\t']);
            let content = trimmed.trim();
            if let (true, Some(key), Some(item)) = (indented, list_key, content.strip_prefix('-')) {
                fm.push(key, item);
                continue;
            }
            list_key = None;
            if indented || content.is_empty() || content.starts_with('#') {
                continue;
            }

            let Some((name, value)) = content.split_once(':') else {
                continue;
            };
            let key = match name.trim().to_ascii_lowercase().as_str() {
                "tags" | "tag" => Key::Tags,
                "aliases" | "alias" => Key::Aliases,
                _ => continue,
            };
            let value = strip_comment(value).trim();
            if value.is_empty() {
                list_key = Some(key);
            } else {
                let inner = value
                    .strip_prefix('[')
                    .and_then(|v| v.strip_suffix(']'))
                    .unwrap_or(value);
                for item in split_items(inner) {
                    fm.push(key, item);
                }
            }
        }

        // No closing fence: treat the whole thing as body, not frontmatter.
        Frontmatter::default()
    }

    fn push(&mut self, key: Key, raw: &str) {
        let value = unquote(strip_comment(raw).trim());
        let value = match key {
            Key::Tags => value.trim_start_matches('#'),
            Key::Aliases => value,
        };
        if value.is_empty() {
            return;
        }
        let list = match key {
            Key::Tags => &mut self.tags,
            Key::Aliases => &mut self.aliases,
        };
        if !list.iter().any(|v| v == value) {
            list.push(value.to_owned());
        }
    }
}

/// Returns the text after an opening fence line, if `text` starts with one.
fn strip_fence<'a>(text: &'a str, fence: &str) -> Option<&'a str> {
    let rest = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .strip_prefix(fence)?;
    rest.strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
}

/// Splits a flow or comma list, keeping commas inside quotes.
fn split_items(s: &str) -> impl Iterator<Item = &str> {
    let mut items = Vec::new();
    let (mut start, mut quote) = (0, None);
    for (i, c) in s.char_indices() {
        match (c, quote) {
            ('"' | '\'', None) => quote = Some(c),
            (c, Some(q)) if c == q => quote = None,
            (',', None) => {
                items.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    items.push(&s[start..]);
    items.into_iter()
}

/// Drops a trailing ` # comment` that isn't inside quotes.
fn strip_comment(s: &str) -> &str {
    let mut quote = None;
    let mut prev_space = true;
    for (i, c) in s.char_indices() {
        match (c, quote) {
            ('"' | '\'', None) => quote = Some(c),
            (c, Some(q)) if c == q => quote = None,
            ('#', None) if prev_space && i > 0 => return &s[..i],
            _ => {}
        }
        prev_space = c.is_whitespace();
    }
    s
}

fn unquote(s: &str) -> &str {
    for q in ['"', '\''] {
        if let Some(inner) = s.strip_prefix(q).and_then(|s| s.strip_suffix(q)) {
            return inner;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_flow_lists() {
        let fm = Frontmatter::parse("---\ntags: [mnote, \"design\", '#gpui']\n---\n# UI design\n");
        assert_eq!(fm.tags, ["mnote", "design", "gpui"]);
        assert_eq!(
            &"---\ntags: [mnote, \"design\", '#gpui']\n---\n# UI design\n"[fm.body_start..],
            "# UI design\n"
        );
    }

    #[test]
    fn reads_block_lists_and_singular_keys() {
        let text =
            "---\ntitle: UI design\naliases:\n  - UI\n  - \"Design: UI\"\ntag: mnote\n---\nBody";
        let fm = Frontmatter::parse(text);
        assert_eq!(fm.aliases, ["UI", "Design: UI"]);
        assert_eq!(fm.tags, ["mnote"]);
        assert_eq!(&text[fm.body_start..], "Body");
    }

    #[test]
    fn reads_comma_lists_and_ignores_comments_and_duplicates() {
        let fm = Frontmatter::parse("---\ntags: mnote, design, mnote # main tags\n---\n");
        assert_eq!(fm.tags, ["mnote", "design"]);
    }

    #[test]
    fn handles_crlf_and_a_byte_order_mark() {
        let fm = Frontmatter::parse("\u{feff}---\r\ntags: [a]\r\n---\r\nBody");
        assert_eq!(fm.tags, ["a"]);
    }

    #[test]
    fn no_frontmatter_or_no_closing_fence_means_none() {
        assert_eq!(
            Frontmatter::parse("# Title\n---\ntags: [x]\n"),
            Frontmatter::default()
        );
        assert_eq!(
            Frontmatter::parse("---\ntags: [x]\nnever closed"),
            Frontmatter::default()
        );
        assert_eq!(Frontmatter::parse(""), Frontmatter::default());
    }

    #[test]
    fn other_keys_are_ignored() {
        let fm = Frontmatter::parse("---\ncreated: 2026-11-21\ncssclasses:\n  - wide\n---\n");
        assert!(fm.tags.is_empty() && fm.aliases.is_empty());
    }
}
