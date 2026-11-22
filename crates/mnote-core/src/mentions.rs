//! Unlinked mentions: places where a note is named in plain text, without
//! a `[[link]]`, and turning one into a link.
//!
//! A note is named by its file name (`ui-design`, also as `ui design`), its
//! title and its aliases, as whole words, ignoring ASCII case. Mentions
//! inside links, inline code, code fences and frontmatter don't count.

use std::ops::Range;
use std::sync::Mutex;
use std::thread;

use crate::frontmatter::Frontmatter;
use crate::links::wikilinks;
use crate::note_path::NotePath;
use crate::vault::Vault;

/// Names shorter than this are too common to look for.
const MIN_TERM: usize = 3;

/// One plain-text mention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    /// 0-based line in the note, frontmatter included.
    pub line: usize,
    /// Bytes of the mention in the line.
    pub range: Range<usize>,
    /// The whole line, as written.
    pub context: String,
}

/// The words a note goes by, lowercase, longest first.
pub fn mention_terms(name: &str, title: &str, aliases: &[String]) -> Vec<String> {
    let spaced = name.replace(['-', '_'], " ");
    let mut terms: Vec<String> = [name, &spaced, title]
        .into_iter()
        .chain(aliases.iter().map(String::as_str))
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| t.len() >= MIN_TERM)
        .collect();
    terms.sort_unstable_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    terms.dedup();
    terms
}

/// Every mention of any of `terms` in `text`.
pub fn find_mentions(text: &str, terms: &[String]) -> Vec<Mention> {
    if terms.is_empty() {
        return Vec::new();
    }
    let body_start = Frontmatter::parse(text).body_start;
    let first_line = text[..body_start].matches('\n').count();
    let mut out = Vec::new();
    let mut fenced = false;
    for (i, line) in text[body_start..].lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let skip = skipped(line);
        let mut taken: Vec<Range<usize>> = Vec::new();
        for term in terms {
            let mut from = 0;
            while let Some(at) = find_ascii_ci(&line[from..], term).map(|at| from + at) {
                let range = at..at + term.len();
                from = range.end;
                let overlaps = |r: &Range<usize>| r.start < range.end && range.start < r.end;
                if !whole_word(line, &range)
                    || skip.iter().any(overlaps)
                    || taken.iter().any(overlaps)
                {
                    continue;
                }
                taken.push(range);
            }
        }
        taken.sort_unstable_by_key(|r| r.start);
        out.extend(taken.into_iter().map(|range| Mention {
            line: first_line + i,
            range,
            context: line.to_owned(),
        }));
    }
    out
}

/// The mentions of `terms` across `notes`, read in parallel, stopping near
/// `limit`. Call it off the UI thread.
pub fn scan_mentions(
    vault: &Vault,
    notes: &[NotePath],
    terms: &[String],
    limit: usize,
) -> Vec<(NotePath, Mention)> {
    let threads = thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(16);
    let chunk = notes.len().div_ceil(threads).max(1);
    let found = Mutex::new(Vec::new());
    thread::scope(|scope| {
        for part in notes.chunks(chunk) {
            let found = &found;
            scope.spawn(move || {
                for path in part {
                    if found.lock().map_or(true, |f| f.len() >= limit) {
                        return;
                    }
                    let Ok(text) = vault.read(path) else { continue };
                    let mentions = find_mentions(&text, terms);
                    if let (false, Ok(mut found)) = (mentions.is_empty(), found.lock()) {
                        found.extend(mentions.into_iter().map(|m| (path.clone(), m)));
                    }
                }
            });
        }
    });
    let mut found = found.into_inner().unwrap_or_default();
    found.sort_by(|a, b| (&a.0, a.1.line, a.1.range.start).cmp(&(&b.0, b.1.line, b.1.range.start)));
    found.truncate(limit);
    found
}

/// `text` with the mention replaced by `link` (a whole `[[…]]`), if line
/// `mention.line` still reads as it did. `None` means the note changed.
pub fn link_mention(text: &str, mention: &Mention, link: &str) -> Option<String> {
    let mut start = 0;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        if i == mention.line {
            let content = line.strip_suffix('\n').unwrap_or(line);
            let content = content.strip_suffix('\r').unwrap_or(content);
            if content != mention.context {
                return None;
            }
            let at = start + mention.range.start;
            let end = start + mention.range.end;
            return Some(format!("{}{link}{}", &text[..at], &text[end..]));
        }
        start += line.len();
    }
    None
}

/// Spans of a line where a mention doesn't count: links, inline code and
/// the target of a Markdown link.
fn skipped(line: &str) -> Vec<Range<usize>> {
    let mut spans: Vec<Range<usize>> = wikilinks(line).into_iter().map(|l| l.range).collect();
    let mut code = None;
    for (i, c) in line.char_indices() {
        if c == '`' {
            match code.take() {
                Some(open) => spans.push(open..i + 1),
                None => code = Some(i),
            }
        }
    }
    let mut from = 0;
    while let Some(at) = line[from..].find("](").map(|i| from + i) {
        let end = line[at..].find(')').map_or(line.len(), |i| at + i + 1);
        spans.push(at..end);
        from = end;
    }
    spans
}

/// Where `needle` (lowercase ASCII-wise) first appears in `haystack`,
/// ignoring ASCII case, on a character boundary.
fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    if n.is_empty() || n.len() > h.len() {
        return None;
    }
    (0..=h.len() - n.len())
        .find(|&i| haystack.is_char_boundary(i) && h[i..i + n.len()].eq_ignore_ascii_case(n))
}

fn whole_word(line: &str, range: &Range<usize>) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    let before = line[..range.start].chars().next_back();
    let after = line.get(range.end..).and_then(|rest| rest.chars().next());
    !before.is_some_and(word) && !after.is_some_and(word)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms() -> Vec<String> {
        mention_terms("ui-design", "UI design", &["UI".into(), "Interface".into()])
    }

    #[test]
    fn terms_are_names_title_and_aliases() {
        assert_eq!(terms(), ["interface", "ui design", "ui-design"]);
    }

    #[test]
    fn finds_whole_words_outside_links_and_code() {
        let text = "---\ntitle: UI design\n---\nthe UI Design for hints\nsee [[ui-design]] and `ui design`\nui-designer is not it\n```\nui design\n```\n[the interface](interface.md), Interface!\n";
        let found = find_mentions(text, &terms());
        let at: Vec<_> = found
            .iter()
            .map(|m| (m.line, &m.context[m.range.clone()]))
            .collect();
        assert_eq!(at, [(3, "UI Design"), (9, "interface"), (9, "Interface")]);
    }

    #[test]
    fn linking_replaces_the_mention_if_the_line_is_unchanged() {
        let text = "a\nthe UI Design for hints\n";
        let mention = find_mentions(text, &terms()).remove(0);
        assert_eq!(
            link_mention(text, &mention, "[[ui-design|UI Design]]").as_deref(),
            Some("a\nthe [[ui-design|UI Design]] for hints\n")
        );
        assert_eq!(link_mention("a\nchanged\n", &mention, "[[x]]"), None);
    }
}
