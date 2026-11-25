//! The note index: every note's path, title, headings, tags and aliases, for
//! the finder and the other pickers.
//!
//! [`NoteIndex::build`] reads the whole vault on background threads (std
//! threads, no runtime). After that it's kept current one note at a time:
//! [`NoteIndex::apply`] for changes the file watcher reports, and
//! [`NoteIndex::note_saved`] for mNote's own saves, which the watcher skips.
//! [`NoteSearch`] queries it with nucleo's fuzzy matcher.

mod outline;
mod search;

use std::collections::BTreeMap;
use std::fs;
use std::thread;
use std::time::SystemTime;

pub use outline::{Heading, OutLink};
pub use search::{HeadingHit, HitField, NoteHit, NoteSearch, StringHit, TagHit};

use crate::error::Result;
use crate::note_path::NotePath;
use crate::vault::Vault;
use crate::watch::VaultEvent;

/// What the index knows about one note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteEntry {
    pub path: NotePath,
    /// The vault-relative path as text (`projects/gpui-notes.md`): what the
    /// finder matches and shows. Match indices count its characters.
    pub path_text: String,
    /// The first `# heading`, else the file name without `.md`.
    pub title: String,
    pub aliases: Vec<String>,
    /// Without `#`, each once.
    pub tags: Vec<String>,
    pub headings: Vec<Heading>,
    /// Its `[[links]]` to other notes, in order.
    pub links: Vec<OutLink>,
    /// When the file last changed, if the file system says.
    pub modified: Option<SystemTime>,
    /// A note (indexed and linked), not another file (a code file listed
    /// by name).
    pub note: bool,
}

impl NoteEntry {
    /// The entry for `path` with contents `text`.
    /// Markdown notes are outlined (title, aliases, tags, headings,
    /// links); a note file type in another language is listed by name only.
    pub fn new(path: NotePath, text: &str, modified: Option<SystemTime>) -> Self {
        if !path.is_note() {
            return Self::file(path, modified);
        }
        let markdown = crate::filetype::for_path(path.as_path())
            .is_none_or(|f| f.language.as_deref() == Some("markdown"));
        let outline = if markdown {
            outline::outline(text)
        } else {
            outline::Outline::default()
        };
        NoteEntry {
            path_text: path.to_string(),
            title: outline.title.unwrap_or_else(|| path.name().to_owned()),
            aliases: outline.aliases,
            tags: outline.tags,
            headings: outline.headings,
            links: outline.links,
            modified,
            note: true,
            path,
        }
    }

    /// A file that isn't a note (a code file): listed by its file name,
    /// not read, never a link's target.
    pub fn file(path: NotePath, modified: Option<SystemTime>) -> Self {
        NoteEntry {
            path_text: path.to_string(),
            title: path.file_name().to_owned(),
            aliases: Vec::new(),
            tags: Vec::new(),
            headings: Vec::new(),
            links: Vec::new(),
            modified,
            note: false,
            path,
        }
    }

    /// Where the file name starts in `path_text`, in characters: match
    /// indices at or after it fall in the file name.
    pub fn file_name_start(&self) -> usize {
        self.path_text
            .rfind('/')
            .map_or(0, |i| self.path_text[..=i].chars().count())
    }
}

/// A link to a note, from another note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlink {
    /// The linking note, by position in [`NoteIndex::notes`].
    pub from: usize,
    /// 0-based line of the link in it.
    pub line: usize,
    /// That line, trimmed.
    pub context: String,
}

/// One entry of a folder, as the explorer lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderItem {
    /// A subfolder, with how many entries (notes and folders) it holds.
    Folder { name: String, items: usize },
    /// A note, by its position in [`NoteIndex::notes`].
    Note(usize),
}

/// Every note in the vault, sorted by path.
#[derive(Debug, Clone, Default)]
pub struct NoteIndex {
    notes: Vec<NoteEntry>,
    /// Tag (lowercase) → its spelling as first seen, and how many notes have
    /// it.
    tags: BTreeMap<String, (String, usize)>,
    /// Whether the vault's code files are listed too.
    files: bool,
}

impl NoteIndex {
    /// Reads every note in `vault`. Notes are read and parsed in parallel; a
    /// note that can't be read is still listed, with its file name as title.
    /// Takes a while on a big vault, so call it off the UI thread.
    pub fn build(vault: &Vault) -> Result<Self> {
        Self::build_with(vault, false)
    }

    /// Like [`NoteIndex::build`], also listing the vault's other files of a
    /// known type (code files) when `files`, by name, unread.
    pub fn build_with(vault: &Vault, files: bool) -> Result<Self> {
        let mut index = Self::build_notes(vault)?;
        index.files = files;
        if files {
            for file in vault.files()? {
                let Ok(path) = NotePath::file(file.as_path()) else {
                    continue;
                };
                if path.is_note() {
                    continue;
                }
                let modified = fs::metadata(vault.path_of(&path))
                    .and_then(|m| m.modified())
                    .ok();
                index.insert(NoteEntry::file(path, modified));
            }
        }
        Ok(index)
    }

    fn build_notes(vault: &Vault) -> Result<Self> {
        let paths = vault.notes()?;
        let threads = thread::available_parallelism()
            .map_or(4, |n| n.get())
            .min(16);
        let chunk = paths.len().div_ceil(threads).max(1);
        let notes = thread::scope(|scope| {
            let workers: Vec<_> = paths
                .chunks(chunk)
                .map(|part| {
                    scope.spawn(move || {
                        part.iter()
                            .map(|path| read_entry(vault, path.clone()))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            workers
                .into_iter()
                .flat_map(|w| w.join().unwrap_or_default())
                .collect()
        });
        Ok(Self::from_entries(notes))
    }

    /// An index of `entries`, in any order.
    pub fn from_entries(mut entries: Vec<NoteEntry>) -> Self {
        entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        entries.dedup_by(|a, b| a.path == b.path);
        let mut index = NoteIndex {
            notes: entries,
            tags: BTreeMap::new(),
            files: false,
        };
        for i in 0..index.notes.len() {
            index.count_tags(i, true);
        }
        index
    }

    pub fn notes(&self) -> &[NoteEntry] {
        &self.notes
    }

    pub fn len(&self) -> usize {
        self.notes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    /// The note a `[[link]]` target names: by path (`projects/ui-design`)
    /// when it has a `/`, else by file name, then by alias, ignoring case and
    /// `.md`. Two notes with one name: the one nearest the vault root wins.
    pub fn resolve(&self, target: &str) -> Option<usize> {
        let key = crate::links::link_key(target);
        if key.is_empty() {
            return None;
        }
        if key.contains('/') {
            return self
                .notes
                .iter()
                .position(|n| n.note && crate::links::note_key(&n.path) == key);
        }
        let by_name = self
            .notes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.note && n.path.name().to_lowercase() == key)
            .min_by_key(|(_, n)| (n.path_text.matches('/').count(), n.path_text.len()))
            .map(|(i, _)| i);
        by_name.or_else(|| {
            self.notes
                .iter()
                .position(|n| n.aliases.iter().any(|a| a.to_lowercase() == key))
        })
    }

    /// Every link to note `note` from the other notes, by note then line.
    pub fn backlinks(&self, note: usize) -> Vec<Backlink> {
        let Some(entry) = self.notes.get(note).filter(|e| e.note) else {
            return Vec::new();
        };
        // The keys that resolve to this note: its path always, its name and
        // aliases when no other note takes them.
        let mut keys = vec![crate::links::note_key(&entry.path)];
        let names =
            std::iter::once(entry.path.name()).chain(entry.aliases.iter().map(String::as_str));
        for name in names {
            let key = name.to_lowercase();
            if !keys.contains(&key) && self.resolve(&key) == Some(note) {
                keys.push(key);
            }
        }
        let mut out = Vec::new();
        for (from, other) in self.notes.iter().enumerate() {
            if from == note {
                continue;
            }
            let mut last_line = None;
            for link in &other.links {
                // One row per line, however many links it has.
                if keys.contains(&link.key) && last_line != Some(link.line) {
                    last_line = Some(link.line);
                    out.push(Backlink {
                        from,
                        line: link.line,
                        context: link.context.clone(),
                    });
                }
            }
        }
        out
    }

    pub fn get(&self, path: &NotePath) -> Option<&NoteEntry> {
        self.position(path).ok().map(|i| &self.notes[i])
    }

    /// What's directly in `folder` (vault-relative, `""` for the root, no
    /// trailing `/`): subfolders first, then notes, each by name ignoring
    /// case. Folders come from note paths, so a folder without notes in it
    /// isn't listed.
    pub fn folder_items(&self, folder: &str) -> Vec<FolderItem> {
        use std::collections::{BTreeMap, BTreeSet};
        let prefix = if folder.is_empty() {
            String::new()
        } else {
            format!("{folder}/")
        };
        let mut folders: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
        let mut notes = Vec::new();
        for (i, note) in self.notes.iter().enumerate() {
            let Some(rest) = note.path_text.strip_prefix(&prefix) else {
                continue;
            };
            match rest.split_once('/') {
                Some((dir, inner)) => {
                    let child = inner.split('/').next().unwrap_or(inner);
                    folders.entry(dir.to_owned()).or_default().insert(child);
                }
                None => notes.push(i),
            }
        }
        let mut folders: Vec<(String, usize)> = folders
            .into_iter()
            .map(|(name, children)| (name, children.len()))
            .collect();
        folders.sort_by_key(|(name, _)| name.to_lowercase());
        notes.sort_by_key(|&i| self.notes[i].path.file_name().to_lowercase());
        folders
            .into_iter()
            .map(|(name, items)| FolderItem::Folder { name, items })
            .chain(notes.into_iter().map(FolderItem::Note))
            .collect()
    }

    /// Every tag with how many notes have it, sorted by name.
    pub fn tags(&self) -> impl Iterator<Item = (&str, usize)> {
        self.tags
            .values()
            .map(|(name, count)| (name.as_str(), *count))
    }

    /// Updates the index for a change the file watcher reported.
    pub fn apply(&mut self, vault: &Vault, event: &VaultEvent) {
        match event {
            VaultEvent::Changed(path) => {
                if !path.is_note() && !self.files {
                    // Code files aren't listed.
                } else if vault.path_of(path).is_file() {
                    self.insert(read_entry(vault, path.clone()));
                } else {
                    self.remove(path);
                }
            }
            VaultEvent::Removed(path) => {
                self.remove(path);
            }
        }
    }

    /// Updates the index after mNote saved `path` with `text` (the watcher
    /// doesn't report mNote's own saves).
    pub fn note_saved(&mut self, path: NotePath, text: &str) {
        self.insert(NoteEntry::new(path, text, Some(SystemTime::now())));
    }

    /// Adds a note, or replaces the entry with the same path.
    pub fn insert(&mut self, entry: NoteEntry) {
        match self.position(&entry.path) {
            Ok(i) => {
                self.count_tags(i, false);
                self.notes[i] = entry;
                self.count_tags(i, true);
            }
            Err(i) => {
                self.notes.insert(i, entry);
                self.count_tags(i, true);
            }
        }
    }

    /// Updates the index after the note or folder `from` moved to `to`
    /// (vault-relative), and returns each note's move, for relinking.
    pub fn moved(
        &mut self,
        from: &std::path::Path,
        to: &std::path::Path,
    ) -> Vec<crate::links::LinkMove> {
        let affected: Vec<usize> = (0..self.notes.len())
            .filter(|&i| self.notes[i].path.as_path().starts_with(from))
            .collect();
        let mut moves = Vec::new();
        let mut entries = Vec::new();
        for &i in affected.iter().rev() {
            self.count_tags(i, false);
            let mut entry = self.notes.remove(i);
            let Ok(rest) = entry.path.as_path().strip_prefix(from) else {
                continue;
            };
            let target = if rest.as_os_str().is_empty() {
                to.to_path_buf()
            } else {
                to.join(rest)
            };
            let Ok(new) = NotePath::new(&target) else {
                continue;
            };
            // A title that came from the file name follows the new name.
            let titled = entry
                .headings
                .iter()
                .any(|h| h.level == 1 && h.text == entry.title);
            if !titled {
                entry.title = new.name().to_owned();
            }
            moves.push(crate::links::LinkMove {
                from: entry.path.clone(),
                to: new.clone(),
            });
            entry.path_text = new.to_string();
            entry.path = new;
            entries.push(entry);
        }
        for entry in entries {
            self.insert(entry);
        }
        moves.reverse();
        moves
    }

    /// The note moves that moving `from` to `to` would make, without changing
    /// the index (for previews).
    pub fn would_move(
        &self,
        from: &std::path::Path,
        to: &std::path::Path,
    ) -> Vec<crate::links::LinkMove> {
        self.notes
            .iter()
            .filter_map(|n| {
                let rest = n.path.as_path().strip_prefix(from).ok()?;
                let target = if rest.as_os_str().is_empty() {
                    to.to_path_buf()
                } else {
                    to.join(rest)
                };
                Some(crate::links::LinkMove {
                    from: n.path.clone(),
                    to: NotePath::new(target).ok()?,
                })
            })
            .collect()
    }

    /// Removes the note at `path`, or every note under the folder `path`.
    /// Returns how many went.
    pub fn remove_under(&mut self, path: &std::path::Path) -> usize {
        let gone: Vec<NotePath> = self
            .notes
            .iter()
            .filter(|n| n.path.as_path().starts_with(path))
            .map(|n| n.path.clone())
            .collect();
        for note in &gone {
            self.remove(note);
        }
        gone.len()
    }

    /// Reads the note at `path`, or every note under the folder `path`, from
    /// disk into the index (after a note or folder came back from the Trash).
    pub fn reload_under(&mut self, vault: &Vault, path: &std::path::Path) {
        let Ok(notes) = vault.notes() else {
            return;
        };
        for note in notes.into_iter().filter(|n| n.as_path().starts_with(path)) {
            self.insert(read_entry(vault, note));
        }
    }

    /// Removes a note. Returns whether it was there.
    pub fn remove(&mut self, path: &NotePath) -> bool {
        let Ok(i) = self.position(path) else {
            return false;
        };
        self.count_tags(i, false);
        self.notes.remove(i);
        true
    }

    fn position(&self, path: &NotePath) -> std::result::Result<usize, usize> {
        self.notes.binary_search_by(|n| n.path.cmp(path))
    }

    /// Adds (or, with `add` false, takes away) note `i`'s tags in the counts.
    fn count_tags(&mut self, i: usize, add: bool) {
        for tag in &self.notes[i].tags {
            let key = tag.to_lowercase();
            if add {
                self.tags.entry(key).or_insert_with(|| (tag.clone(), 0)).1 += 1;
            } else if let Some(entry) = self.tags.get_mut(&key) {
                entry.1 -= 1;
                if entry.1 == 0 {
                    self.tags.remove(&key);
                }
            }
        }
    }
}

fn read_entry(vault: &Vault, path: NotePath) -> NoteEntry {
    let file = vault.path_of(&path);
    let modified = fs::metadata(&file).and_then(|m| m.modified()).ok();
    if !path.is_note() {
        return NoteEntry::file(path, modified);
    }
    let text = vault.read(&path).unwrap_or_default();
    NoteEntry::new(path, &text, modified)
}

#[cfg(test)]
mod tests {

    #[test]
    fn code_files_are_listed_by_name_not_linked() {
        use crate::vault::tests::TempDir;
        let dir = TempDir::new();
        dir.file("main.md", "# Main\n\nSee [[main]] and [[lib]].\n");
        dir.file("src/main.rs", "fn main() { let s = \"[[main]]\"; }\n");
        dir.file("src/lib.rs", "pub fn f() {}\n");
        dir.file("notes.txtx", "unknown kind");
        let vault = Vault::open(&dir.0).expect("vault");

        let notes_only = NoteIndex::build(&vault).expect("index");
        assert_eq!(notes_only.len(), 1);

        let index = NoteIndex::build_with(&vault, true).expect("index");
        let listed: Vec<(&str, bool, &str)> = index
            .notes()
            .iter()
            .map(|n| (n.path_text.as_str(), n.note, n.title.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                ("main.md", true, "Main"),
                ("src/lib.rs", false, "lib.rs"),
                ("src/main.rs", false, "main.rs"),
            ],
            "code files by file name; unknown kinds aren't listed"
        );
        // Links resolve to notes only: [[lib]] doesn't find lib.rs.
        assert_eq!(index.resolve("lib"), None);
        let main_note = index.resolve("main").expect("the note");
        assert!(index.notes()[main_note].note);
        // A code file has no backlinks, and its text isn't read for links.
        let code = index.notes().iter().position(|n| !n.note).expect("a file");
        assert!(index.backlinks(code).is_empty());
        assert!(index.notes()[code].links.is_empty());

        // Saving a code file keeps it a file entry.
        let mut index = index;
        let rs = NotePath::file("src/lib.rs").expect("a file");
        index.note_saved(rs.clone(), "pub fn g() {}");
        assert!(
            index
                .get(&rs)
                .is_some_and(|e| !e.note && e.title == "lib.rs")
        );
        // Without code files listed, a change to one is ignored.
        let mut notes_only = notes_only;
        notes_only.apply(&vault, &VaultEvent::Changed(rs));
        assert_eq!(notes_only.len(), 1);
    }

    #[test]
    fn notes_in_another_language_are_listed_not_outlined() {
        crate::filetype::register(crate::filetype::Filetype {
            name: "org-index-test".into(),
            extensions: vec!["orgtest".into()],
            language: Some("org".into()),
            note: true,
        });
        let path = NotePath::new("plans.orgtest").expect("a note path");
        let entry = NoteEntry::new(path, "# Heading\n#tag [[link]]\n", None);
        assert_eq!(entry.title, "plans");
        assert!(entry.headings.is_empty() && entry.tags.is_empty() && entry.links.is_empty());
        let md = NoteEntry::new(NotePath::new("a.md").expect("path"), "# Heading\n", None);
        assert_eq!(md.title, "Heading");
    }

    use super::*;
    use crate::vault::tests::TempDir;

    fn note(p: &str) -> NotePath {
        NotePath::new(p).expect("valid note path")
    }

    #[test]
    fn builds_from_a_vault() {
        let dir = TempDir::new();
        dir.file("inbox.md", "Quick #idea\n");
        dir.file(
            "projects/ui-design.md",
            "---\ntags: [design]\n---\n# UI design\n## Principles\n",
        );
        dir.file(".obsidian/hidden.md", "# Hidden\n");
        let vault = Vault::open(&dir.0).expect("vault");
        let index = NoteIndex::build(&vault).expect("index");
        assert_eq!(index.len(), 2);
        let ui = index.get(&note("projects/ui-design.md")).expect("indexed");
        assert_eq!(ui.title, "UI design");
        assert_eq!(ui.path_text, "projects/ui-design.md");
        assert_eq!(ui.file_name_start(), 9);
        assert!(ui.modified.is_some());
        assert_eq!(
            index.get(&note("inbox.md")).map(|n| n.title.as_str()),
            Some("inbox")
        );
        let tags: Vec<_> = index.tags().collect();
        assert_eq!(tags, [("design", 1), ("idea", 1)]);
    }

    #[test]
    fn follows_moves_of_notes_and_folders() {
        let entry = |p: &str, text: &str| NoteEntry::new(note(p), text, None);
        let mut index = NoteIndex::from_entries(vec![
            entry("a.md", "#tag"),
            entry("dir/b.md", "# Bee\n"),
            entry("dir/sub/c.md", ""),
            entry("dirt.md", ""),
        ]);
        let moves = index.moved(std::path::Path::new("a.md"), std::path::Path::new("z.md"));
        assert_eq!(moves.len(), 1);
        assert_eq!(
            index.get(&note("z.md")).map(|n| n.title.as_str()),
            Some("z"),
            "title from the name"
        );
        assert_eq!(index.tags().collect::<Vec<_>>(), [("tag", 1)]);

        let moves = index.moved(std::path::Path::new("dir"), std::path::Path::new("x/dir"));
        assert_eq!(moves.len(), 2, "dirt.md isn't inside dir/");
        assert_eq!(
            index.get(&note("x/dir/b.md")).map(|n| n.title.as_str()),
            Some("Bee")
        );
        assert!(index.get(&note("x/dir/sub/c.md")).is_some());
        assert_eq!(index.remove_under(std::path::Path::new("x")), 2);
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn lists_folders_then_notes() {
        let entry = |p: &str| NoteEntry::new(note(p), "", None);
        let index = NoteIndex::from_entries(vec![
            entry("inbox.md"),
            entry("Projects/mnote/ui-design.md"),
            entry("Projects/mnote/design/colours.md"),
            entry("Projects/mnote/design/type.md"),
            entry("Projects/gpui-notes.md"),
            entry("journal/2026-11-21.md"),
        ]);
        let names = |folder: &str| -> Vec<String> {
            index
                .folder_items(folder)
                .into_iter()
                .map(|item| match item {
                    FolderItem::Folder { name, items } => format!("{name}/ {items}"),
                    FolderItem::Note(i) => index.notes()[i].path.file_name().to_owned(),
                })
                .collect()
        };
        assert_eq!(names(""), ["journal/ 1", "Projects/ 2", "inbox.md"]);
        assert_eq!(names("Projects"), ["mnote/ 2", "gpui-notes.md"]);
        assert_eq!(names("Projects/mnote"), ["design/ 2", "ui-design.md"]);
        assert!(names("missing").is_empty());
    }

    #[test]
    fn stays_current_with_events_and_saves() {
        let dir = TempDir::new();
        dir.file("a.md", "#one\n");
        let vault = Vault::open(&dir.0).expect("vault");
        let mut index = NoteIndex::build(&vault).expect("index");

        dir.file("b.md", "# Bee #one #two\n");
        index.apply(&vault, &VaultEvent::Changed(note("b.md")));
        assert_eq!(index.len(), 2);
        assert_eq!(index.tags().collect::<Vec<_>>(), [("one", 2), ("two", 1)]);

        index.note_saved(note("a.md"), "# A\n");
        assert_eq!(
            index.get(&note("a.md")).map(|n| n.title.as_str()),
            Some("A")
        );
        assert_eq!(index.tags().collect::<Vec<_>>(), [("one", 1), ("two", 1)]);

        index.apply(&vault, &VaultEvent::Removed(note("b.md")));
        assert_eq!(index.len(), 1);
        assert_eq!(index.tags().count(), 0, "tags of removed notes go");

        // A change event for a file that's gone (moved) removes it too.
        index.apply(&vault, &VaultEvent::Changed(note("missing.md")));
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn links_resolve_by_path_name_and_alias() {
        let entry = |p: &str, text: &str| NoteEntry::new(note(p), text, None);
        let index = NoteIndex::from_entries(vec![
            entry("ui-design.md", "---\naliases: [UI]\n---\n# UI design\n"),
            entry("old/ui-design.md", ""),
            entry(
                "projects/roadmap.md",
                "phase 2 follows [[ui-design]] here\n[[UI]] and [[ui]] once\n```\n[[ui-design]]\n```\n",
            ),
            entry(
                "journal/2026-11-21.md",
                "sketched [[old/ui-design]] and [[Roadmap#Phase 2|plan]]\n",
            ),
        ]);
        let pos = |p: &str| index.notes().iter().position(|n| n.path_text == p);
        assert_eq!(
            index.resolve("UI-Design.md"),
            pos("ui-design.md"),
            "nearest the root"
        );
        assert_eq!(index.resolve("old/ui-design"), pos("old/ui-design.md"));
        assert_eq!(index.resolve("ui"), pos("ui-design.md"), "by alias");
        assert_eq!(index.resolve("roadmap"), pos("projects/roadmap.md"));
        assert_eq!(index.resolve("nowhere"), None);

        let ui = pos("ui-design.md").expect("indexed");
        let back = index.backlinks(ui);
        assert_eq!(
            back.len(),
            2,
            "one row per line, fenced links skipped: {back:?}"
        );
        assert_eq!(back[0].context, "phase 2 follows [[ui-design]] here");
        assert_eq!(
            (back[1].from, back[1].line),
            (pos("projects/roadmap.md").expect("indexed"), 1)
        );
        let old = pos("old/ui-design.md").expect("indexed");
        assert_eq!(
            index.backlinks(old).len(),
            1,
            "only the path link reaches the shadowed note"
        );
    }
}
