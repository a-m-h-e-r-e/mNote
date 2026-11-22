//! Backlinks (`space b`): every line in other notes that links to this one,
//! grouped by note, filterable, with the linking note around the link in the
//! preview. `⏎` opens the note at the link.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;

use gpui::SharedString;
use mnote_core::index::StringHit;
use mnote_core::links::wikilinks;
use mnote_core::{NoteIndex, NotePath, NoteSearch, Vault};
use mnote_vim::Mode;

use super::{CodePreview, Confirm, PickerAction, PickerDelegate, PreviewContent, Row, RowKind};
use crate::ui::Hint;

/// Preview lines, and how many of them come before the link.
const PREVIEW_LINES: usize = 14;
const CONTEXT_ABOVE: usize = 4;

/// One linking line.
#[derive(Debug, Clone)]
struct Entry {
    path: NotePath,
    line: usize,
    context: String,
    /// The links to this note in `context`.
    marks: Vec<Range<usize>>,
}

/// A row: a note's heading, or one of its linking lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    Note(usize),
    Line(usize),
}

pub struct BacklinksPicker {
    vault: Vault,
    /// The note's title and file name, for the title bar.
    title: String,
    file: String,
    entries: Vec<Entry>,
    notes: usize,
    haystacks: Vec<String>,
    search: NoteSearch,
    rows: Vec<Shown>,
    /// Linking notes read for the preview, by path.
    texts: RefCell<HashMap<NotePath, Option<String>>>,
}

impl BacklinksPicker {
    /// The backlinks of `note` from `index`; `vault` reads notes for the
    /// preview.
    pub fn new(index: &NoteIndex, note: &NotePath, vault: Vault) -> Self {
        let position = index.notes().iter().position(|n| n.path == *note);
        let (title, entries) = match position {
            Some(at) => {
                let entries = index
                    .backlinks(at)
                    .into_iter()
                    .map(|b| {
                        let marks = wikilinks(&b.context)
                            .into_iter()
                            .filter(|l| index.resolve(&l.target) == Some(at))
                            .map(|l| l.range)
                            .collect();
                        Entry {
                            path: index.notes()[b.from].path.clone(),
                            line: b.line,
                            context: b.context,
                            marks,
                        }
                    })
                    .collect();
                (index.notes()[at].title.clone(), entries)
            }
            None => (note.name().to_owned(), Vec::new()),
        };
        let mut notes: Vec<&NotePath> = entries.iter().map(|e: &Entry| &e.path).collect();
        notes.dedup();
        let notes = notes.len();
        let haystacks = entries
            .iter()
            .map(|e| format!("{} {}", e.path, e.context))
            .collect();
        BacklinksPicker {
            vault,
            title,
            file: note.file_name().to_owned(),
            entries,
            notes,
            haystacks,
            search: NoteSearch::new(),
            rows: Vec::new(),
            texts: RefCell::default(),
        }
    }

    fn entry(&self, ix: usize) -> Option<&Entry> {
        match self.rows.get(ix)? {
            Shown::Line(e) => self.entries.get(*e),
            Shown::Note(_) => None,
        }
    }
}

impl PickerDelegate for BacklinksPicker {
    fn title(&self) -> Option<(SharedString, Option<SharedString>)> {
        let title = match self.notes {
            0 => format!("No notes link to {}", self.title),
            1 => format!("1 note links to {}", self.title),
            n => format!("{n} notes link to {}", self.title),
        };
        Some((title.into(), Some(self.file.clone().into())))
    }

    fn scope(&self, mode: Mode) -> Option<SharedString> {
        Some(SharedString::new_static(match mode {
            Mode::Normal => "j k move · i edit query",
            _ => "filter by note or text",
        }))
    }

    fn set_query(&mut self, query: &str) {
        let haystacks = self.haystacks.iter().map(String::as_str);
        let mut hits: Vec<usize> = self
            .search
            .strings(query, haystacks, self.entries.len())
            .into_iter()
            .map(|h: StringHit| h.item)
            .collect();
        // Grouped by note, in note order, however the matcher ranked them.
        hits.sort_unstable();
        self.rows.clear();
        for e in hits {
            let new_note = match self.rows.last() {
                Some(Shown::Line(prev)) => self.entries[*prev].path != self.entries[e].path,
                _ => true,
            };
            if new_note {
                self.rows.push(Shown::Note(e));
            }
            self.rows.push(Shown::Line(e));
        }
    }

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn row(&self, ix: usize) -> Row {
        match self.rows[ix] {
            Shown::Note(e) => {
                let path = &self.entries[e].path;
                let folder = path.to_string();
                let folder = folder.rsplit_once('/').map(|(dir, _)| dir.to_owned());
                Row {
                    label: path.file_name().to_owned().into(),
                    detail: folder.map(Into::into),
                    kind: RowKind::Group,
                    ..Row::default()
                }
            }
            Shown::Line(e) => {
                let entry = &self.entries[e];
                Row {
                    label: entry.context.clone().into(),
                    marks: entry.marks.clone(),
                    kind: RowKind::Hit {
                        line: entry.line + 1,
                    },
                    ..Row::default()
                }
            }
        }
    }

    fn selectable(&self, ix: usize) -> bool {
        matches!(self.rows.get(ix), Some(Shown::Line(_)))
    }

    fn preview(&self, ix: usize) -> Option<PreviewContent> {
        let entry = self.entry(ix)?;
        let mut texts = self.texts.borrow_mut();
        let text = texts
            .entry(entry.path.clone())
            .or_insert_with(|| self.vault.read(&entry.path).ok());
        let first = entry.line.saturating_sub(CONTEXT_ABOVE);
        let lines = text
            .as_deref()
            .unwrap_or("")
            .lines()
            .enumerate()
            .skip(first)
            .take(PREVIEW_LINES)
            .map(|(n, line)| {
                let marks = if n == entry.line {
                    wikilinks(line).into_iter().map(|l| l.range).collect()
                } else {
                    Vec::new()
                };
                (n + 1, SharedString::from(line.to_owned()), marks)
            })
            .collect();
        Some(PreviewContent {
            title: entry.path.name().to_owned().into(),
            meta: Some(format!("{} · line {}", entry.path, entry.line + 1).into()),
            lines: Vec::new(),
            code: Some(CodePreview {
                lines,
                current: entry.line + 1,
            }),
        })
    }

    fn confirm(&mut self, ix: usize) -> Confirm {
        match self.entry(ix) {
            Some(entry) => Confirm::Act(PickerAction::JumpTo {
                path: entry.path.clone(),
                line: entry.line,
            }),
            None => Confirm::Stay,
        }
    }

    fn hints(&self, _mode: Mode) -> Vec<Hint> {
        vec![Hint::new("⏎", "open at link")]
    }

    fn count(&self) -> Option<SharedString> {
        let shown = self
            .rows
            .iter()
            .filter(|r| matches!(r, Shown::Line(_)))
            .count();
        Some(format!("{shown} / {}", self.entries.len()).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnote_core::NoteEntry;

    fn path(p: &str) -> NotePath {
        NotePath::new(p).expect("path")
    }

    #[test]
    fn groups_links_by_note_and_jumps_to_one() {
        let dir = std::env::temp_dir().join(format!("mnote-backlinks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let vault = Vault::open(&dir).expect("vault");
        let index = NoteIndex::from_entries(vec![
            NoteEntry::new(path("ui-design.md"), "# UI design\n", None),
            NoteEntry::new(
                path("projects/roadmap.md"),
                "a\nfollows [[ui-design]]\nand [[UI-Design|again]]\n",
                None,
            ),
            NoteEntry::new(path("ideas.md"), "see [[ui-design#Modals]]\n", None),
        ]);
        let mut picker = BacklinksPicker::new(&index, &path("ui-design.md"), vault);
        picker.set_query("");
        assert_eq!(
            picker.title().map(|t| t.0.to_string()),
            Some("2 notes link to UI design".into())
        );
        let kinds: Vec<_> = (0..picker.len()).map(|i| picker.row(i).kind).collect();
        assert_eq!(
            kinds,
            [
                RowKind::Group,
                RowKind::Hit { line: 1 },
                RowKind::Group,
                RowKind::Hit { line: 2 },
                RowKind::Hit { line: 3 },
            ]
        );
        assert_eq!(picker.row(1).marks, vec![4..24]);
        assert!(!picker.selectable(0));
        assert_eq!(
            picker.confirm(3),
            Confirm::Act(PickerAction::JumpTo {
                path: path("projects/roadmap.md"),
                line: 1
            })
        );
        picker.set_query("roadmap");
        assert_eq!(picker.len(), 3, "the group and its two lines");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
