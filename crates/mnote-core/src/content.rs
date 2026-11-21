//! Full-text search over note contents (the finder's `/` scope).
//!
//! A query is words; a line matches when it holds every word, in any order
//! and also inside longer words (`event tap` finds `CGEventTap`). Case is
//! smart: all-lowercase words ignore case, a capital makes it exact. Files are
//! read and searched in parallel on std threads, and a search can be
//! cancelled between files, so typing never waits for a stale search.
//!
//! The `regex` crate (already used for Vim patterns) does the matching. Notes
//! are read whole because results keep each file's text for the preview, so a
//! line-oriented searcher such as ripgrep's `grep` crates would add nothing
//! but dependencies.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::thread;

use regex::{Regex, RegexBuilder};

use crate::note_path::NotePath;
use crate::vault::Vault;

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineHit {
    /// 0-based line number.
    pub line: usize,
    /// Byte range of the line in the file's text, without the line ending.
    pub span: Range<usize>,
    /// Byte ranges of the matched words, relative to the line.
    pub marks: Vec<Range<usize>>,
}

/// The matches in one note, with its text for previews.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHits {
    pub path: NotePath,
    pub text: Arc<str>,
    pub hits: Vec<LineHit>,
}

impl FileHits {
    /// The text of hit `i`'s line.
    pub fn line_text(&self, i: usize) -> &str {
        &self.text[self.hits[i].span.clone()]
    }
}

/// A compiled content query.
#[derive(Debug, Clone)]
pub struct ContentQuery {
    regex: Regex,
    /// The words, lowercased when matching ignores case.
    words: Vec<String>,
    ignore_case: bool,
}

impl ContentQuery {
    /// Compiles `query`, or `None` if it has no words.
    pub fn new(query: &str) -> Option<Self> {
        let words: Vec<&str> = query.split_whitespace().collect();
        if words.is_empty() {
            return None;
        }
        let ignore_case = !query.chars().any(char::is_uppercase);
        let alternation = words
            .iter()
            .map(|w| regex::escape(w))
            .collect::<Vec<_>>()
            .join("|");
        let regex = RegexBuilder::new(&alternation)
            .case_insensitive(ignore_case)
            .build()
            .ok()?;
        let words = words
            .iter()
            .map(|w| {
                if ignore_case {
                    w.to_lowercase()
                } else {
                    (*w).to_owned()
                }
            })
            .collect();
        Some(ContentQuery {
            regex,
            words,
            ignore_case,
        })
    }

    /// The matching lines of `text`.
    pub fn search(&self, text: &str) -> Vec<LineHit> {
        let mut hits = Vec::new();
        let mut current: Option<(LineHit, u64)> = None;
        let all: u64 = if self.words.len() >= 64 {
            u64::MAX
        } else {
            (1 << self.words.len()) - 1
        };
        let mut line = 0;
        let mut scanned = 0;
        for m in self.regex.find_iter(text) {
            // Count the line breaks since the last match.
            let newlines = count_newlines(&text.as_bytes()[scanned..m.start()]);
            if newlines > 0 {
                if let Some((hit, found)) = current.take()
                    && found == all
                {
                    hits.push(hit);
                }
                line += newlines;
            }
            scanned = m.start();
            let (hit, found) = current.get_or_insert_with(|| {
                let start = text[..m.start()].rfind('\n').map_or(0, |i| i + 1);
                let end = text[m.start()..]
                    .find('\n')
                    .map_or(text.len(), |i| m.start() + i);
                let end = if text[..end].ends_with('\r') {
                    end - 1
                } else {
                    end
                };
                (
                    LineHit {
                        line,
                        span: start..end,
                        marks: Vec::new(),
                    },
                    0,
                )
            });
            let base = hit.span.start;
            hit.marks.push(m.start() - base..m.end() - base);
            *found |= self.word_bit(m.as_str());
        }
        if let Some((hit, found)) = current
            && found == all
        {
            hits.push(hit);
        }
        hits
    }

    /// Which word a match is, as a bit.
    fn word_bit(&self, matched: &str) -> u64 {
        let matched = if self.ignore_case {
            std::borrow::Cow::Owned(matched.to_lowercase())
        } else {
            std::borrow::Cow::Borrowed(matched)
        };
        self.words
            .iter()
            .enumerate()
            .filter(|(_, w)| **w == *matched)
            .fold(0, |bits, (i, _)| bits | (1u64 << i.min(63)))
    }
}

fn count_newlines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| b == b'\n').count()
}

/// Lets a newer search stop an older one: each search remembers the
/// generation it started in and gives up once it changes.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicU64>);

impl Cancel {
    /// Starts a new generation, cancelling searches of earlier ones, and
    /// returns it.
    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.0.load(Ordering::SeqCst) == generation
    }
}

/// Searches `notes` in `vault` for `query`. Files are returned with the most
/// hits first, at most `limit` hits in all; the search stops reading files
/// once it has that many. Returns `None` if `cancel` moved
/// past `generation` before the search finished. Call it off the UI thread.
pub fn search_notes(
    vault: &Vault,
    notes: &[NotePath],
    query: &ContentQuery,
    limit: usize,
    cancel: &Cancel,
    generation: u64,
) -> Option<Vec<FileHits>> {
    let threads = thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(16);
    let chunk = notes.len().div_ceil(threads).max(1);
    let found_hits = AtomicUsize::new(0);
    let found_hits = &found_hits;
    let mut files: Vec<FileHits> = thread::scope(|scope| {
        let workers: Vec<_> = notes
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    let mut found = Vec::new();
                    for path in part {
                        if !cancel.is_current(generation)
                            || found_hits.load(Ordering::Relaxed) >= limit
                        {
                            break;
                        }
                        let Ok(text) = vault.read(path) else { continue };
                        let hits = query.search(&text);
                        if !hits.is_empty() {
                            found_hits.fetch_add(hits.len(), Ordering::Relaxed);
                            found.push(FileHits {
                                path: path.clone(),
                                text: text.into(),
                                hits,
                            });
                        }
                    }
                    found
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_default())
            .collect()
    });
    if !cancel.is_current(generation) {
        return None;
    }
    files.sort_by(|a, b| b.hits.len().cmp(&a.hits.len()).then(a.path.cmp(&b.path)));
    let mut left = limit;
    files.retain_mut(|f| {
        if left == 0 {
            return false;
        }
        f.hits.truncate(left);
        left -= f.hits.len();
        true
    });
    Some(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::tests::TempDir;

    fn lines(query: &str, text: &str) -> Vec<(usize, String, Vec<String>)> {
        let q = ContentQuery::new(query).expect("words");
        q.search(text)
            .into_iter()
            .map(|h| {
                let line = &text[h.span.clone()];
                let marks = h.marks.iter().map(|m| line[m.clone()].to_owned()).collect();
                (h.line, line.to_owned(), marks)
            })
            .collect()
    }

    #[test]
    fn every_word_on_the_line_in_any_order() {
        let text = "## Space as leader\n\nneeds a CGEventTap with Accessibility\nthe tap of an event\nonly event here\r\nTAP event\n";
        let got = lines("event tap", text);
        assert_eq!(
            got,
            [
                (
                    2,
                    "needs a CGEventTap with Accessibility".into(),
                    vec!["Event".into(), "Tap".into()]
                ),
                (
                    3,
                    "the tap of an event".into(),
                    vec!["tap".into(), "event".into()]
                ),
                (5, "TAP event".into(), vec!["TAP".into(), "event".into()]),
            ]
        );
        assert_eq!(lines("Event", text).len(), 1, "a capital matches case");
        assert!(ContentQuery::new("   ").is_none());
        assert_eq!(lines("a.b", "a.b axb\n").len(), 1, "words are literal");
    }

    #[test]
    fn searches_a_vault_and_cancels() {
        let dir = TempDir::new();
        dir.file("a.md", "one tap\ntwo tap\n");
        dir.file("b.md", "tap\n");
        dir.file("c.md", "nothing\n");
        let vault = Vault::open(&dir.0).expect("vault");
        let notes = vault.notes().expect("notes");
        let query = ContentQuery::new("tap").expect("query");
        let cancel = Cancel::default();
        let generation = cancel.next();
        let files = search_notes(&vault, &notes, &query, 10, &cancel, generation).expect("done");
        let got: Vec<(String, usize)> = files
            .iter()
            .map(|f| (f.path.to_string(), f.hits.len()))
            .collect();
        assert_eq!(got, [("a.md".into(), 2), ("b.md".into(), 1)]);
        assert_eq!(files[0].line_text(1), "two tap");

        let limited = search_notes(&vault, &notes, &query, 2, &cancel, generation).expect("done");
        assert_eq!(limited.iter().map(|f| f.hits.len()).sum::<usize>(), 2);

        let stale = generation;
        cancel.next();
        assert!(search_notes(&vault, &notes, &query, 10, &cancel, stale).is_none());
    }
}

#[cfg(test)]
mod budget {
    use super::*;
    use crate::vault::tests::TempDir;
    use std::time::{Duration, Instant};

    /// Content search runs off the UI thread, but should still feel live on a
    /// 10 000-note vault with the files cached: a common word stops at the
    /// hit limit, a rare one reads every note. Run with `cargo test -p mnote-core --release -- --ignored`.
    #[test]
    #[ignore = "timing test; run with --release -- --ignored"]
    fn ten_thousand_notes() {
        let dir = TempDir::new();
        for i in 0..10_000 {
            dir.file(
                &format!("f{}/note-{i}.md", i % 50),
                &format!("# Note {i}\n\nSome text about gpui and vim, line {i}.\nAn event tap waits here.\n\n- item\n- item\n"),
            );
        }
        let vault = Vault::open(&dir.0).expect("vault");
        let notes = vault.notes().expect("notes");
        let cancel = Cancel::default();
        let time = |words: &str| {
            let query = ContentQuery::new(words).expect("query");
            let t = Instant::now();
            let files = search_notes(&vault, &notes, &query, 500, &cancel, cancel.next());
            (t.elapsed(), files.expect("done"))
        };
        let _warm = time("warm");
        let (common, files) = time("event tap");
        assert_eq!(files.iter().map(|f| f.hits.len()).sum::<usize>(), 500);
        let (rare, files) = time("line 9999.");
        assert_eq!(files.len(), 1);
        eprintln!("content search, 10k notes: common {common:?}, rare (every file) {rare:?}");
        assert!(common < Duration::from_millis(50), "common took {common:?}");
        assert!(rare < Duration::from_millis(250), "rare took {rare:?}");
    }
}
