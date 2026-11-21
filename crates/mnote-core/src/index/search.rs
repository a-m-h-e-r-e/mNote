//! Fuzzy search over the [`NoteIndex`] with nucleo's matcher.
//!
//! Queries use fzf syntax: words match separately, `'word` exactly, `^word`
//! as a prefix, `word$` as a suffix, `!word` excludes. Case is smart: an
//! uppercase letter makes the word match case. Every candidate is scored, then
//! only the results that are shown get match indices, for bold letters.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use super::NoteIndex;

/// Which text of a note matched best.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitField {
    /// [`NoteEntry::path_text`](super::NoteEntry::path_text).
    Path,
    Title,
    /// One of the note's aliases, by position.
    Alias(usize),
}

/// A note that matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteHit {
    /// Position in [`NoteIndex::notes`].
    pub note: usize,
    /// The best score of path, title and aliases.
    pub score: u32,
    /// The field `indices` are in: the path whenever it matched, else the
    /// title or alias that did.
    pub field: HitField,
    /// Matched character positions in that field, sorted.
    pub indices: Vec<u32>,
}

/// A heading that matched (the finder's `@` scope).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadingHit {
    pub note: usize,
    /// Position in that note's headings.
    pub heading: usize,
    pub score: u32,
    pub indices: Vec<u32>,
}

/// A tag that matched (the finder's `#` scope).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagHit {
    pub tag: String,
    /// How many notes have it.
    pub count: usize,
    pub score: u32,
    pub indices: Vec<u32>,
}

/// Reusable search state: matchers and buffers stay allocated between
/// keystrokes.
pub struct NoteSearch {
    paths: Matcher,
    text: Matcher,
    pattern: Pattern,
    buf: Vec<char>,
}

impl std::fmt::Debug for NoteSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NoteSearch")
    }
}

impl Default for NoteSearch {
    fn default() -> Self {
        Self::new()
    }
}

impl NoteSearch {
    pub fn new() -> Self {
        NoteSearch {
            paths: Matcher::new(Config::DEFAULT.match_paths()),
            text: Matcher::new(Config::DEFAULT),
            pattern: Pattern::default(),
            buf: Vec::new(),
        }
    }

    fn set_query(&mut self, query: &str) {
        self.pattern
            .reparse(query, CaseMatching::Smart, Normalization::Smart);
    }

    /// The best `limit` notes for `query`, matching path, title and aliases.
    /// An empty query lists the most recently changed notes first.
    pub fn notes(&mut self, index: &NoteIndex, query: &str, limit: usize) -> Vec<NoteHit> {
        let notes = index.notes();
        self.set_query(query);
        if self.pattern.atoms.is_empty() {
            let mut recent: Vec<usize> = (0..notes.len()).collect();
            let key = |&i: &usize| std::cmp::Reverse(notes[i].modified);
            if limit < recent.len() {
                recent.select_nth_unstable_by_key(limit, key);
                recent.truncate(limit);
            }
            recent.sort_by_key(key);
            return recent
                .into_iter()
                .map(|note| NoteHit {
                    note,
                    score: 0,
                    field: HitField::Path,
                    indices: Vec::new(),
                })
                .collect();
        }

        let mut scored: Vec<(u32, usize, HitField)> = Vec::new();
        for (i, note) in notes.iter().enumerate() {
            let mut best = self
                .pattern
                .score(
                    Utf32Str::new(&note.path_text, &mut self.buf),
                    &mut self.paths,
                )
                .map(|s| (s, HitField::Path));
            let path_matched = best.is_some();
            let others = std::iter::once((HitField::Title, note.title.as_str())).chain(
                note.aliases
                    .iter()
                    .enumerate()
                    .map(|(a, alias)| (HitField::Alias(a), alias.as_str())),
            );
            for (field, text) in others {
                if let Some(s) = self
                    .pattern
                    .score(Utf32Str::new(text, &mut self.buf), &mut self.text)
                    && best.is_none_or(|(b, _)| s > b)
                {
                    best = Some((s, field));
                }
            }
            if let Some((score, field)) = best {
                // Rank by the best field, but highlight the path whenever it
                // matched: that's what the finder shows.
                let field = if path_matched { HitField::Path } else { field };
                scored.push((score, i, field));
            }
        }
        let order = |a: &(u32, usize, HitField), b: &(u32, usize, HitField)| {
            b.0.cmp(&a.0)
                .then(notes[a.1].path_text.len().cmp(&notes[b.1].path_text.len()))
                .then(a.1.cmp(&b.1))
        };
        top(&mut scored, limit, order);

        scored
            .into_iter()
            .map(|(score, note, field)| {
                let entry = &notes[note];
                let (text, matcher) = match field {
                    HitField::Path => (entry.path_text.as_str(), &mut self.paths),
                    HitField::Title => (entry.title.as_str(), &mut self.text),
                    HitField::Alias(a) => (entry.aliases[a].as_str(), &mut self.text),
                };
                let indices = indices(&self.pattern, text, matcher, &mut self.buf);
                NoteHit {
                    note,
                    score,
                    field,
                    indices,
                }
            })
            .collect()
    }

    /// The best `limit` headings in the vault for `query`.
    pub fn headings(&mut self, index: &NoteIndex, query: &str, limit: usize) -> Vec<HeadingHit> {
        self.set_query(query);
        let notes = index.notes();
        let mut scored: Vec<(u32, usize, usize)> = Vec::new();
        for (n, note) in notes.iter().enumerate() {
            for (h, heading) in note.headings.iter().enumerate() {
                if let Some(s) = self
                    .pattern
                    .score(Utf32Str::new(&heading.text, &mut self.buf), &mut self.text)
                {
                    scored.push((s, n, h));
                }
            }
        }
        top(&mut scored, limit, |a, b| {
            b.0.cmp(&a.0).then((a.1, a.2).cmp(&(b.1, b.2)))
        });
        scored
            .into_iter()
            .map(|(score, note, heading)| HeadingHit {
                note,
                heading,
                score,
                indices: indices(
                    &self.pattern,
                    &notes[note].headings[heading].text,
                    &mut self.text,
                    &mut self.buf,
                ),
            })
            .collect()
    }

    /// The best `limit` tags for `query`; an empty query lists the most used.
    pub fn tags(&mut self, index: &NoteIndex, query: &str, limit: usize) -> Vec<TagHit> {
        self.set_query(query);
        let mut scored: Vec<(u32, usize, &str)> = Vec::new();
        for (tag, count) in index.tags() {
            if let Some(s) = self
                .pattern
                .score(Utf32Str::new(tag, &mut self.buf), &mut self.text)
            {
                scored.push((s, count, tag));
            }
        }
        top(&mut scored, limit, |a, b| {
            b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(b.2))
        });
        scored
            .into_iter()
            .map(|(score, count, tag)| TagHit {
                tag: tag.to_owned(),
                count,
                score,
                indices: indices(&self.pattern, tag, &mut self.text, &mut self.buf),
            })
            .collect()
    }
}

/// Keeps the first `limit` of `items` in `order`, sorted.
fn top<T>(items: &mut Vec<T>, limit: usize, order: impl Fn(&T, &T) -> std::cmp::Ordering) {
    if limit == 0 {
        items.clear();
        return;
    }
    if limit < items.len() {
        items.select_nth_unstable_by(limit - 1, &order);
        items.truncate(limit);
    }
    items.sort_unstable_by(order);
}

/// Sorted, unique match positions of `pattern` in `text`.
fn indices(pattern: &Pattern, text: &str, matcher: &mut Matcher, buf: &mut Vec<char>) -> Vec<u32> {
    let mut out = Vec::new();
    pattern.indices(Utf32Str::new(text, buf), matcher, &mut out);
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::NoteEntry;
    use crate::note_path::NotePath;
    use std::time::{Duration, SystemTime};

    fn entry(path: &str, text: &str, age_days: u64) -> NoteEntry {
        let modified =
            SystemTime::UNIX_EPOCH + Duration::from_secs(100_000_000 - age_days * 86_400);
        NoteEntry::new(NotePath::new(path).expect("path"), text, Some(modified))
    }

    fn index() -> NoteIndex {
        NoteIndex::from_entries(vec![
            entry(
                "projects/gpui-notes.md",
                "# GPUI notes\n## Render trait\n#rust\n",
                2,
            ),
            entry("inbox/gpu-benchmarks.md", "#rust #gpu\n", 9),
            entry("ref/zed-gpui-anatomy.md", "", 30),
            entry("journal/2026-11-21.md", "---\naliases: [Today]\n---\n", 0),
            entry("reading-list.md", "## Rendering books\n", 5),
        ])
    }

    fn paths(index: &NoteIndex, hits: &[NoteHit]) -> Vec<String> {
        hits.iter()
            .map(|h| index.notes()[h.note].path_text.clone())
            .collect()
    }

    #[test]
    fn notes_rank_and_highlight() {
        let index = index();
        let mut search = NoteSearch::new();
        let hits = search.notes(&index, "gpu not", 10);
        assert_eq!(paths(&index, &hits)[0], "projects/gpui-notes.md");
        let best = &hits[0];
        assert_eq!(best.field, HitField::Path);
        let name_start = index.notes()[best.note].file_name_start() as u32;
        assert!(
            best.indices.iter().all(|&i| i >= name_start),
            "{:?}",
            best.indices
        );
        assert!(
            best.indices.windows(2).all(|w| w[0] < w[1]),
            "sorted and unique"
        );

        let hits = search.notes(&index, "today", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].field, HitField::Alias(0));

        assert!(search.notes(&index, "zzzz", 10).is_empty());
        assert_eq!(search.notes(&index, "gpu", 1).len(), 1, "limit");
        assert!(
            search
                .notes(&index, "gpu !bench", 10)
                .iter()
                .all(|h| !index.notes()[h.note].path_text.contains("bench"))
        );
    }

    #[test]
    fn an_empty_query_lists_recent_notes() {
        let index = index();
        let hits = NoteSearch::new().notes(&index, "", 3);
        assert_eq!(
            paths(&index, &hits),
            [
                "journal/2026-11-21.md",
                "projects/gpui-notes.md",
                "reading-list.md"
            ]
        );
    }

    #[test]
    fn headings_and_tags() {
        let index = index();
        let mut search = NoteSearch::new();
        let hits = search.headings(&index, "rend", 10);
        let texts: Vec<&str> = hits
            .iter()
            .map(|h| index.notes()[h.note].headings[h.heading].text.as_str())
            .collect();
        assert_eq!(texts.len(), 2);
        assert!(texts.contains(&"Render trait") && texts.contains(&"Rendering books"));
        assert_eq!(hits[0].indices, [0, 1, 2, 3]);

        let tags = search.tags(&index, "", 10);
        assert_eq!(tags[0].tag, "rust", "most used first");
        assert_eq!(tags[0].count, 2);
        let tags = search.tags(&index, "gp", 10);
        assert_eq!((tags.len(), tags[0].tag.as_str()), (1, "gpu"));
    }
}

#[cfg(test)]
mod budget {
    use super::*;
    use crate::vault::Vault;
    use crate::vault::tests::TempDir;
    use std::time::{Duration, Instant};

    /// The finder budget: results update in under 5 ms per keystroke on a
    /// 10 000-note vault, and the index builds in under a second. Run with
    /// `cargo test -p mnote-core --release -- --ignored`.
    #[test]
    #[ignore = "timing test; run with --release -- --ignored"]
    fn ten_thousand_notes() {
        let words = [
            "design", "gpui", "notes", "journal", "rust", "vim", "explorer", "finder", "ideas",
            "reading",
        ];
        let dir = TempDir::new();
        for i in 0..10_000 {
            let (a, b) = (words[i % 10], words[(i / 10) % 10]);
            let folder = words[(i / 100) % 10];
            dir.file(
                &format!("{folder}/{a}-{b}-{i}.md"),
                &format!("---\ntags: [{a}]\n---\n# {a} {b} {i}\n\nSome text about #{b}.\n\n## Part one\n\n## Part two {a}\n\nMore text.\n"),
            );
        }
        let vault = Vault::open(&dir.0).expect("vault");
        let t = Instant::now();
        let index = NoteIndex::build(&vault).expect("index");
        let built = t.elapsed();
        assert_eq!(index.len(), 10_000);

        let mut search = NoteSearch::new();
        let mut worst = Duration::ZERO;
        // Typing "gpui not", one key at a time.
        let query = "gpui not";
        for end in 1..=query.len() {
            let t = Instant::now();
            let hits = search.notes(&index, &query[..end], 50);
            worst = worst.max(t.elapsed());
            assert!(!hits.is_empty());
        }
        let t = Instant::now();
        let headings = search.headings(&index, "part two rust", 50);
        let heading_time = t.elapsed();
        assert!(!headings.is_empty());

        eprintln!("      build 10k notes: {built:?}");
        eprintln!(" worst notes keystroke: {worst:?}");
        eprintln!("   headings (30k)    : {heading_time:?}");
        assert!(built < Duration::from_secs(1), "build took {built:?}");
        assert!(
            worst < Duration::from_millis(5),
            "a keystroke took {worst:?}"
        );
    }
}
