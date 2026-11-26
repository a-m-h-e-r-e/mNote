//! Backlinks (`space b`): every line in other notes that links to this one,
//! grouped by note, then the places that name it without a link ("Unlinked
//! mentions", found in the background). Filterable, with the note around the
//! line in the preview. `⏎` opens the note there; `l` on a mention turns it
//! into a `[[link]]`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;

use gpui::SharedString;
use mnote_core::index::StringHit;
use mnote_core::links::{Relinked, wikilinks};
use mnote_core::mentions::{Mention, link_mention, mention_terms, scan_mentions};
use mnote_core::{NoteIndex, NotePath, NoteSearch, Vault};
use mnote_vim::Mode;

use super::{
    CodePreview, Confirm, Job, JobResult, PickerAction, PickerDelegate, PickerEffect,
    PreviewContent, Row, RowKind,
};
use crate::ui::Hint;

/// Preview lines, and how many of them come before the line.
const PREVIEW_LINES: usize = 14;
const CONTEXT_ABOVE: usize = 4;
/// Unlinked mentions listed at most.
const MENTION_LIMIT: usize = 100;

/// One line that links here, or one plain mention.
#[derive(Debug, Clone)]
struct Entry {
    path: NotePath,
    line: usize,
    context: String,
    /// The links to this note in `context`, or the mention.
    marks: Vec<Range<usize>>,
    /// Set for an unlinked mention.
    mention: Option<Mention>,
}

/// A row: a note's heading, one of its lines, the mentions heading, or a
/// mention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    Note(usize),
    Line(usize),
    Mentions(usize),
    Mention(usize),
}

pub struct BacklinksPicker {
    vault: Vault,
    /// The note's title and file name, for the title bar.
    title: String,
    file: String,
    /// The lowercase names that resolve to this note (`[[name]]` works).
    keys: Vec<String>,
    /// What a link to it needs when the mention isn't one of `keys`.
    link_name: String,
    /// Linked lines first (by note and line), then mentions.
    entries: Vec<Entry>,
    haystacks: Vec<String>,
    search: NoteSearch,
    query: String,
    rows: Vec<Shown>,
    /// The mention search, or a link being made, until the editor takes it.
    job: Option<Job>,
    /// The mention being turned into a link: its note and line.
    linking: Option<(NotePath, usize)>,
    searching: bool,
    effects: Vec<PickerEffect>,
    /// Notes read for the preview, by path.
    texts: RefCell<HashMap<NotePath, Option<String>>>,
}

impl BacklinksPicker {
    /// The backlinks of `note` from `index`; `vault` reads notes for the
    /// preview and the mention search.
    pub fn new(index: &NoteIndex, note: &NotePath, vault: Vault) -> Self {
        let position = index.notes().iter().position(|n| n.path == *note);
        let mut keys = Vec::new();
        let mut link_name = note.name().to_owned();
        let mut job: Option<Job> = None;
        let (title, entries) = match position {
            Some(at) => {
                let entry = &index.notes()[at];
                for name in std::iter::once(entry.path.name())
                    .chain(entry.aliases.iter().map(String::as_str))
                {
                    if index.resolve(name) == Some(at) {
                        keys.push(name.to_lowercase());
                    }
                }
                if index.resolve(&link_name) != Some(at) {
                    // Another note has the name: link by path.
                    link_name = entry
                        .path
                        .as_path()
                        .with_extension("")
                        .to_string_lossy()
                        .into_owned();
                }
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
                            mention: None,
                        }
                    })
                    .collect();
                let terms = mention_terms(entry.path.name(), &entry.title, &entry.aliases);
                let others: Vec<NotePath> = index
                    .notes()
                    .iter()
                    .filter(|n| n.path != *note)
                    .map(|n| n.path.clone())
                    .collect();
                let search_vault = vault.clone();
                job = Some(Box::new(move || {
                    Some(JobResult::Mentions(scan_mentions(
                        &search_vault,
                        &others,
                        &terms,
                        MENTION_LIMIT,
                    )))
                }));
                (entry.title.clone(), entries)
            }
            None => (note.name().to_owned(), Vec::new()),
        };
        let mut picker = BacklinksPicker {
            vault,
            title,
            file: note.file_name().to_owned(),
            keys,
            link_name,
            entries,
            haystacks: Vec::new(),
            search: NoteSearch::new(),
            query: String::new(),
            rows: Vec::new(),
            searching: job.is_some(),
            job,
            linking: None,
            effects: Vec::new(),
            texts: RefCell::default(),
        };
        picker.reindex();
        picker
    }

    /// After `entries` change: haystacks, then rows for the same query.
    fn reindex(&mut self) {
        self.haystacks = self
            .entries
            .iter()
            .map(|e| format!("{} {}", e.path, e.context))
            .collect();
        let query = std::mem::take(&mut self.query);
        self.set_query(&query);
    }

    fn entry(&self, ix: usize) -> Option<&Entry> {
        match self.rows.get(ix)? {
            Shown::Line(e) | Shown::Mention(e) => self.entries.get(*e),
            Shown::Note(_) | Shown::Mentions(_) => None,
        }
    }

    fn linked_notes(&self) -> usize {
        let mut notes: Vec<&NotePath> = self
            .entries
            .iter()
            .filter(|e| e.mention.is_none())
            .map(|e| &e.path)
            .collect();
        notes.dedup();
        notes.len()
    }

    /// The link that replaces a mention's text.
    fn link_for(&self, text: &str) -> String {
        if self.keys.contains(&text.to_lowercase()) {
            format!("[[{text}]]")
        } else {
            format!("[[{}|{text}]]", self.link_name)
        }
    }

    /// `l` on a mention: rewrite it as a link, off the UI thread.
    fn link(&mut self, e: usize) -> Confirm {
        let Some(entry) = self.entries.get(e) else {
            return Confirm::Stay;
        };
        let Some(mention) = entry.mention.clone() else {
            return Confirm::Stay;
        };
        let link = self.link_for(&entry.context[mention.range.clone()]);
        let (vault, path) = (self.vault.clone(), entry.path.clone());
        let file = path.file_name().to_owned();
        self.linking = Some((path.clone(), mention.line));
        self.job = Some(Box::new(move || {
            let changed = vault
                .read(&path)
                .ok()
                .and_then(|text| link_mention(&text, &mention, &link));
            let Some(text) = changed else {
                return Some(JobResult::Relinked {
                    done: Vec::new(),
                    message: format!("{file} changed since the search; open backlinks again"),
                    undo: false,
                });
            };
            if let Err(err) = vault.write(&path, &text) {
                return Some(JobResult::Relinked {
                    done: Vec::new(),
                    message: format!("Couldn't link in {file}: {err}"),
                    undo: false,
                });
            }
            Some(JobResult::Relinked {
                done: vec![Relinked {
                    note: path,
                    text,
                    links: 1,
                }],
                message: format!("Linked in {file}"),
                undo: false,
            })
        }));
        Confirm::Stay
    }

    /// The mention at `line` of `note` became a link (the note is now
    /// `text`): that line joins the linked lines, and its other mentions go.
    fn linked(&mut self, note: &NotePath, line: usize, text: &str) {
        self.entries
            .retain(|e| !(e.path == *note && e.line == line && e.mention.is_some()));
        let context = text.lines().nth(line).unwrap_or("").trim().to_owned();
        let marks = wikilinks(&context)
            .into_iter()
            .filter(|l| self.keys.contains(&l.key()) || l.key() == self.link_name.to_lowercase())
            .map(|l| l.range)
            .collect();
        let at = self
            .entries
            .iter()
            .position(|e| e.mention.is_some() || (&e.path, e.line) > (note, line))
            .unwrap_or(self.entries.len());
        self.entries.insert(
            at,
            Entry {
                path: note.clone(),
                line,
                context,
                marks,
                mention: None,
            },
        );
        self.texts.borrow_mut().remove(note);
        self.reindex();
    }
}

impl PickerDelegate for BacklinksPicker {
    fn opens_preview(&self) -> bool {
        true
    }

    fn title(&self) -> Option<(SharedString, Option<SharedString>)> {
        let title = match self.linked_notes() {
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
        self.query = query.to_owned();
        let haystacks = self.haystacks.iter().map(String::as_str);
        let mut hits: Vec<usize> = self
            .search
            .strings(query, haystacks, self.entries.len())
            .into_iter()
            .map(|h: StringHit| h.item)
            .collect();
        // In entry order (linked by note, then mentions), however ranked.
        hits.sort_unstable();
        self.rows.clear();
        let mentions = hits
            .iter()
            .filter(|&&e| self.entries[e].mention.is_some())
            .count();
        for e in hits {
            if self.entries[e].mention.is_some() {
                if !matches!(self.rows.last(), Some(Shown::Mention(_))) {
                    self.rows.push(Shown::Mentions(mentions));
                }
                self.rows.push(Shown::Mention(e));
                continue;
            }
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
            Shown::Mentions(count) => Row {
                label: SharedString::new_static("Unlinked mentions"),
                detail: Some(count.to_string().into()),
                kind: RowKind::Group,
                ..Row::default()
            },
            Shown::Mention(e) => {
                let entry = &self.entries[e];
                let prefix = format!("{}: ", entry.path.file_name());
                let shift = prefix.len();
                Row {
                    label: format!("{prefix}{}", entry.context.trim_end()).into(),
                    marks: entry
                        .marks
                        .iter()
                        .map(|r| r.start + shift..r.end + shift)
                        .collect(),
                    detail: Some(SharedString::new_static("l link")),
                    quiet: true,
                    ..Row::default()
                }
            }
        }
    }

    fn selectable(&self, ix: usize) -> bool {
        matches!(self.rows.get(ix), Some(Shown::Line(_) | Shown::Mention(_)))
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
                let marks = match (&entry.mention, n == entry.line) {
                    (_, false) => Vec::new(),
                    (Some(m), true) => vec![m.range.clone()],
                    (None, true) => wikilinks(line).into_iter().map(|l| l.range).collect(),
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

    fn claims(&self, c: char) -> bool {
        c == 'l'
    }

    fn claimed(&mut self, c: char, ix: usize) -> Confirm {
        match (c, self.rows.get(ix)) {
            ('l', Some(Shown::Mention(e))) => self.link(*e),
            _ => self.confirm(ix),
        }
    }

    fn take_job(&mut self) -> Option<Job> {
        self.job.take()
    }

    fn job_done(&mut self, result: JobResult) {
        match result {
            JobResult::Mentions(found) => {
                self.searching = false;
                self.entries
                    .extend(found.into_iter().map(|(path, mention)| Entry {
                        path,
                        line: mention.line,
                        context: mention.context.clone(),
                        marks: vec![mention.range.clone()],
                        mention: Some(mention),
                    }));
                self.reindex();
            }
            JobResult::Relinked { done, message, .. } => {
                if let (Some((note, line)), Some(relinked)) = (self.linking.take(), done.first()) {
                    self.linked(&note, line, &relinked.text);
                }
                self.effects.push(PickerEffect::Relinked(done));
                self.effects.push(PickerEffect::Toast {
                    text: message,
                    undo: false,
                });
            }
            JobResult::Content { .. } => {}
        }
    }

    fn take_effects(&mut self) -> Vec<PickerEffect> {
        std::mem::take(&mut self.effects)
    }

    fn hints(&self, _mode: Mode) -> Vec<Hint> {
        vec![
            Hint::new("⏎", "open at link"),
            Hint::new("l", "turn mention into link"),
        ]
    }

    fn footer(&self, _ix: usize) -> Option<SharedString> {
        self.searching
            .then(|| SharedString::new_static("Looking for unlinked mentions…"))
    }

    fn count(&self) -> Option<SharedString> {
        let shown = self
            .rows
            .iter()
            .filter(|r| matches!(r, Shown::Line(_) | Shown::Mention(_)))
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

    #[test]
    fn finds_mentions_and_links_one_with_l() {
        let dir = std::env::temp_dir().join(format!("mnote-mentions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("ui-design.md"), "# UI design\n").expect("write");
        std::fs::write(dir.join("roadmap.md"), "follows [[ui-design]]\n").expect("write");
        std::fs::write(dir.join("vim-keys.md"), "a\nthe UI design for hints\n").expect("write");
        let vault = Vault::open(&dir).expect("vault");
        let index = NoteIndex::build(&vault).expect("index");
        let mut picker = BacklinksPicker::new(&index, &path("ui-design.md"), vault);

        let search = picker.take_job().expect("mention search");
        picker.job_done(search().expect("result"));
        let rows: Vec<_> = (0..picker.len()).map(|i| picker.row(i)).collect();
        assert_eq!(
            rows.len(),
            4,
            "linked group and line, then mentions heading and one"
        );
        assert_eq!(rows[2].label.as_ref(), "Unlinked mentions");
        assert_eq!(
            rows[3].label.as_ref(),
            "vim-keys.md: the UI design for hints"
        );
        assert_eq!(rows[3].marks, vec![17..26]);

        assert_eq!(picker.claimed('l', 3), Confirm::Stay);
        let link = picker.take_job().expect("link job");
        picker.job_done(link().expect("result"));
        assert_eq!(
            std::fs::read_to_string(dir.join("vim-keys.md")).expect("read"),
            "a\nthe [[ui-design|UI design]] for hints\n"
        );
        assert_eq!(
            picker.title().map(|t| t.0.to_string()),
            Some("2 notes link to UI design".into())
        );
        assert_eq!(picker.len(), 4, "two groups of one line, no mentions left");
        assert!(
            matches!(picker.take_effects().as_slice(), [PickerEffect::Relinked(done), PickerEffect::Toast { .. }] if done.len() == 1)
        );
        assert_eq!(
            picker.claimed('l', 1),
            picker.confirm(1),
            "l on a linked line opens it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
