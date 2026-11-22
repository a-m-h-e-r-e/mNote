//! The explorer (`space e`): one folder at a time, its folders then its
//! notes, with a preview of the selected one. A folder previews what's in it;
//! a note previews its outline.
//!
//! The filter is a Vim field (D-001): typing fuzzy-filters this folder's
//! list. In NORMAL mode `j` / `k` move, `l` / `⏎` open, and `h` goes up a
//! folder and lands on the one you came from. `esc` in NORMAL first clears
//! the filter, then closes.
//!
//! File operations, all in NORMAL mode and none behind a confirm dialog:
//! `a` new note, `r` rename (inline, the extension outside the field), `dd`
//! then `p` move, `D` to the Trash. Each shows a toast; `u` undoes the last
//! one (in the explorer, `u` undoes file operations, not filter edits).
//! Renames and moves rewrite wikilinks to the notes they touch, on a
//! background thread.
//!
//! Listings come from the note index, so moving around reads no files. A
//! folder with no notes in it isn't listed.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::SharedString;
use mnote_core::files::{FileOp, VaultPath};
use mnote_core::index::StringHit;
use mnote_core::links::{LinkMove, relink_vault};
use mnote_core::{FolderItem, NoteIndex, NotePath, NoteSearch, Vault};
use mnote_vim::{InputAction, Key, Mode, VimInput};

use super::finder::{age, note_preview, plural};
use super::{
    Confirm, Job, JobResult, PickerAction, PickerDelegate, PickerEffect, PreviewContent, Row,
    RowKind,
};
use crate::ui::Hint;

/// The explorer's width (wider than the finder, for the folder path).
const WIDTH: f32 = 900.;
/// Items a folder preview lists.
const PREVIEW_ITEMS: usize = 12;

/// A file operation that can be undone, and the link rewrites it made.
#[derive(Debug, Clone)]
pub struct FileUndo {
    pub op: FileOp,
    /// What the toast said it did (`Renamed a.md to b.md`).
    pub label: String,
}

/// File operations of this session, newest last. Shared with the editor so
/// `u` still works after the explorer is closed and opened again.
pub type FileHistory = Rc<RefCell<Vec<FileUndo>>>;

/// What the inline field is naming.
enum Naming {
    /// A new note in the folder shown.
    New,
    /// The item at this path, keeping `ext` outside the field.
    Rename { from: VaultPath, ext: &'static str },
}

struct Editing {
    input: VimInput,
    naming: Naming,
}

pub struct Explorer {
    index: Rc<RefCell<NoteIndex>>,
    vault: Vault,
    history: FileHistory,
    /// The vault folder as the title shows it (`~/notes`).
    vault_name: String,
    /// The folder shown, vault-relative, `""` for the root.
    folder: String,
    items: Vec<FolderItem>,
    names: Vec<String>,
    search: NoteSearch,
    hits: Vec<StringHit>,
    editing: Option<Editing>,
    /// Cut with `dd`, waiting for `p`.
    cut: Option<VaultPath>,
    /// The first `d` of `dd`.
    pending_d: bool,
    effects: Vec<PickerEffect>,
    job: Option<Job>,
}

impl Explorer {
    /// The explorer on the folder of `current` (the root without one), with
    /// `current` selected.
    pub fn new(
        index: Rc<RefCell<NoteIndex>>,
        vault: Vault,
        history: FileHistory,
        vault_name: String,
        current: Option<&NotePath>,
    ) -> (Self, usize) {
        let folder = current
            .map(ToString::to_string)
            .and_then(|p| p.rsplit_once('/').map(|(dir, _)| dir.to_owned()))
            .unwrap_or_default();
        let mut explorer = Explorer {
            index,
            vault,
            history,
            vault_name,
            folder: String::new(),
            items: Vec::new(),
            names: Vec::new(),
            search: NoteSearch::new(),
            hits: Vec::new(),
            editing: None,
            cut: None,
            pending_d: false,
            effects: Vec::new(),
            job: None,
        };
        explorer.enter(folder);
        let select = current
            .and_then(|p| explorer.position_of(p.as_path()))
            .unwrap_or(0);
        (explorer, select)
    }

    /// The explorer on `folder` (vault-relative), or the root if the index
    /// has nothing in it.
    pub fn at_folder(
        index: Rc<RefCell<NoteIndex>>,
        vault: Vault,
        history: FileHistory,
        vault_name: String,
        folder: &str,
    ) -> Self {
        let folder = folder.trim_matches('/');
        let (mut explorer, _) = Self::new(index, vault, history, vault_name, None);
        if !explorer.index.borrow().folder_items(folder).is_empty() {
            explorer.enter(folder.to_owned());
        }
        explorer
    }

    /// Shows `folder` (the caller resets the query).
    fn enter(&mut self, folder: String) {
        let index = self.index.borrow();
        self.items = index.folder_items(&folder);
        self.names = self
            .items
            .iter()
            .map(|item| match item {
                FolderItem::Folder { name, .. } => format!("{name}/"),
                FolderItem::Note(i) => index.notes()[*i].path.file_name().to_owned(),
            })
            .collect();
        drop(index);
        self.folder = folder;
        self.set_query("");
    }

    /// Lists the folder again after a change, returning where `path` is now.
    fn refresh(&mut self, path: Option<&Path>) -> Confirm {
        self.enter(self.folder.clone());
        Confirm::Navigate {
            select: path.and_then(|p| self.position_of(p)).unwrap_or(0),
        }
    }

    /// Position of the note or folder at `path` in this folder's list.
    fn position_of(&self, path: &Path) -> Option<usize> {
        (0..self.items.len()).find(|&i| self.path_of(&self.items[i]).as_path() == path)
    }

    fn position_of_folder(&self, folder: &str) -> Option<usize> {
        self.items
            .iter()
            .position(|item| matches!(item, FolderItem::Folder { name, .. } if name == folder))
    }

    fn path_of(&self, item: &FolderItem) -> PathBuf {
        match item {
            FolderItem::Folder { name, .. } => PathBuf::from(self.child(name)),
            FolderItem::Note(i) => self.index.borrow().notes()[*i].path.as_path().to_path_buf(),
        }
    }

    /// Row `ix` is the new-note field while one is being named.
    fn new_row(&self) -> bool {
        matches!(
            self.editing,
            Some(Editing {
                naming: Naming::New,
                ..
            })
        )
    }

    fn item(&self, ix: usize) -> Option<&FolderItem> {
        let ix = ix.checked_sub(usize::from(self.new_row()))?;
        self.items.get(self.hits.get(ix)?.item)
    }

    fn child(&self, name: &str) -> String {
        if self.folder.is_empty() {
            name.to_owned()
        } else {
            format!("{}/{name}", self.folder)
        }
    }

    /// `h`: the parent folder, with the one we left selected.
    fn up(&mut self) -> Confirm {
        if self.folder.is_empty() {
            return Confirm::Stay;
        }
        let (parent, left) = match self.folder.rsplit_once('/') {
            Some((parent, left)) => (parent.to_owned(), left.to_owned()),
            None => (String::new(), self.folder.clone()),
        };
        self.enter(parent);
        Confirm::Navigate {
            select: self.position_of_folder(&left).unwrap_or(0),
        }
    }

    fn toast(&mut self, text: impl Into<String>, undo: bool) {
        self.effects.push(PickerEffect::Toast {
            text: text.into(),
            undo,
        });
    }

    fn remember(&mut self, op: FileOp, label: &str) {
        self.history.borrow_mut().push(FileUndo {
            op,
            label: label.to_owned(),
        });
    }

    // ---- operations ------------------------------------------------------

    /// `r`: an inline field on row `ix` with the name, extension outside.
    fn start_rename(&mut self, ix: usize) -> Confirm {
        let Some(item) = self.item(ix).cloned() else {
            return Confirm::Stay;
        };
        let path = self.path_of(&item);
        let Ok(from) = VaultPath::new(&path) else {
            return Confirm::Stay;
        };
        let (name, ext) = match &item {
            FolderItem::Folder { name, .. } => (name.clone(), "/"),
            FolderItem::Note(_) => {
                let file = path
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default();
                match file.strip_suffix(".md") {
                    Some(stem) => (stem.to_owned(), ".md"),
                    None => (file, ""),
                }
            }
        };
        self.editing = Some(Editing {
            input: VimInput::new(&name),
            naming: Naming::Rename { from, ext },
        });
        Confirm::Stay
    }

    /// `a`: an inline field at the top for a new note's name.
    fn start_new(&mut self) -> Confirm {
        self.editing = Some(Editing {
            input: VimInput::new(""),
            naming: Naming::New,
        });
        Confirm::Navigate { select: 0 }
    }

    /// `⏎` in the field.
    fn commit(&mut self) -> Confirm {
        let Some(editing) = self.editing.take() else {
            return Confirm::Stay;
        };
        let name = editing.input.text().trim().trim_matches('/').to_owned();
        if name.is_empty() {
            return self.refresh(None);
        }
        match editing.naming {
            Naming::New => {
                let file = if name.ends_with(".md") || name.ends_with(".markdown") {
                    name
                } else {
                    format!("{name}.md")
                };
                let path = self.child(&file);
                let note = match NotePath::new(&path) {
                    Ok(note) => note,
                    Err(err) => {
                        self.toast(err.to_string(), false);
                        return self.refresh(None);
                    }
                };
                match self.vault.create_note(&note) {
                    Ok(op) => {
                        self.index.borrow_mut().note_saved(note.clone(), "");
                        let label = format!("Created {}", note.file_name());
                        self.remember(op, &label);
                        self.toast(label, true);
                    }
                    Err(err) => self.toast(err.to_string(), false),
                }
                // Inside a new subfolder (`ideas/gpu`), land on the folder.
                let first = file.split('/').next().unwrap_or(&file).to_owned();
                self.refresh(Some(Path::new(&self.child(&first))))
            }
            Naming::Rename { from, ext } => {
                let parent = from.as_path().parent().unwrap_or(Path::new(""));
                let file = if ext == "/" {
                    name
                } else {
                    format!("{name}{ext}")
                };
                let to = parent.join(&file);
                if to == from.as_path() {
                    return self.refresh(Some(&to));
                }
                let label = format!(
                    "Renamed {} to {file}",
                    from.as_path()
                        .file_name()
                        .map(|f| f.to_string_lossy())
                        .unwrap_or_default()
                );
                self.move_item(&from, &to, &label);
                self.refresh(Some(&to))
            }
        }
    }

    /// Renames or moves `from` to `to`, then rewrites links in the background.
    fn move_item(&mut self, from: &VaultPath, to: &Path, label: &str) {
        let to = match VaultPath::new(to) {
            Ok(to) => to,
            Err(err) => return self.toast(err.to_string(), false),
        };
        let op = match self.vault.rename(from, &to) {
            Ok(op) => op,
            Err(err) => return self.toast(err.to_string(), false),
        };
        let moves = self.index.borrow_mut().moved(from.as_path(), to.as_path());
        self.remember(op, label);
        self.effects.push(PickerEffect::Moved {
            from: from.as_path().to_path_buf(),
            to: to.as_path().to_path_buf(),
        });
        self.relink(moves, label.to_owned(), true);
    }

    /// Starts rewriting links for `moves`; the toast comes when it's done.
    fn relink(&mut self, moves: Vec<LinkMove>, message: String, undo: bool) {
        if moves.is_empty() {
            return self.toast(message, undo);
        }
        let vault = self.vault.clone();
        let notes: Vec<NotePath> = self
            .index
            .borrow()
            .notes()
            .iter()
            .map(|n| n.path.clone())
            .collect();
        self.job = Some(Box::new(move || {
            let done = relink_vault(&vault, &notes, &moves);
            Some(JobResult::Relinked {
                done,
                message,
                undo,
            })
        }));
    }

    /// `D`: row `ix` to the Trash.
    fn trash(&mut self, ix: usize) -> Confirm {
        let Some(item) = self.item(ix).cloned() else {
            return Confirm::Stay;
        };
        let path = self.path_of(&item);
        let Ok(target) = VaultPath::new(&path) else {
            return Confirm::Stay;
        };
        match self.vault.trash(&target) {
            Ok(op) => {
                self.index.borrow_mut().remove_under(&path);
                let label = format!(
                    "{} moved to Trash",
                    path.file_name()
                        .map(|f| f.to_string_lossy())
                        .unwrap_or_default()
                );
                self.remember(op, &label);
                self.toast(label, true);
            }
            Err(err) => self.toast(err.to_string(), false),
        }
        self.enter(self.folder.clone());
        Confirm::Navigate {
            select: ix.min(self.hits.len().saturating_sub(1)),
        }
    }

    /// `p`: the cut item into the folder shown.
    fn paste(&mut self) -> Confirm {
        let Some(from) = self.cut.take() else {
            return Confirm::Stay;
        };
        let name = from
            .as_path()
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let to = PathBuf::from(self.child(&name));
        if to == from.as_path() {
            return Confirm::Stay;
        }
        let into = if self.folder.is_empty() {
            "the top folder".to_owned()
        } else {
            format!("{}/", self.folder)
        };
        self.move_item(&from, &to, &format!("Moved {name} to {into}"));
        self.refresh(Some(&to))
    }

    /// `u`: undoes the newest file operation.
    fn undo(&mut self) -> Confirm {
        let Some(last) = self.history.borrow_mut().pop() else {
            return Confirm::Stay;
        };
        if let Err(err) = self.vault.undo(&last.op) {
            self.toast(err.to_string(), false);
            self.history.borrow_mut().push(last);
            return Confirm::Stay;
        }
        let message = format!("Undone: {}", last.label);
        match &last.op {
            FileOp::Created(note) => {
                self.index.borrow_mut().remove(note);
                self.toast(message, false);
            }
            FileOp::Moved { from, to } => {
                let moves = self.index.borrow_mut().moved(to.as_path(), from.as_path());
                self.effects.push(PickerEffect::Moved {
                    from: to.as_path().to_path_buf(),
                    to: from.as_path().to_path_buf(),
                });
                self.relink(moves, message, false);
            }
            FileOp::Trashed { from, .. } => {
                self.index
                    .borrow_mut()
                    .reload_under(&self.vault, from.as_path());
                self.toast(message, false);
            }
        }
        let back = match &last.op {
            FileOp::Created(_) => None,
            FileOp::Moved { from, .. } | FileOp::Trashed { from, .. } => {
                Some(from.as_path().to_path_buf())
            }
        };
        self.refresh(back.as_deref())
    }
}

impl PickerDelegate for Explorer {
    fn title(&self) -> Option<(SharedString, Option<SharedString>)> {
        let name = self.folder.rsplit('/').next().filter(|n| !n.is_empty());
        let title = name.map_or_else(|| self.vault_name.clone(), str::to_owned);
        let mut path = self.vault_name.clone();
        for part in self.folder.split('/').filter(|p| !p.is_empty()) {
            path.push_str(" / ");
            path.push_str(part);
        }
        Some((title.into(), Some(path.into())))
    }

    fn scope(&self, mode: Mode) -> Option<SharedString> {
        Some(SharedString::new_static(match mode {
            Mode::Normal => "j k move · i edit filter",
            _ => "⌃n ⌃p move · esc normal",
        }))
    }

    fn set_query(&mut self, query: &str) {
        let names = self.names.iter().map(String::as_str);
        self.hits = self.search.strings(query, names, self.names.len());
    }

    fn len(&self) -> usize {
        self.hits.len() + usize::from(self.new_row())
    }

    fn row(&self, ix: usize) -> Row {
        if let Some(editing) = &self.editing {
            let here = match &editing.naming {
                Naming::New => ix == 0,
                Naming::Rename { from, .. } => self
                    .item(ix)
                    .is_some_and(|item| self.path_of(item).as_path() == from.as_path()),
            };
            if here {
                let text = editing.input.text();
                let cursor = text
                    .char_indices()
                    .nth(editing.input.cursor())
                    .map_or(text.len(), |(b, _)| b);
                let suffix = match &editing.naming {
                    Naming::New => ".md",
                    Naming::Rename { ext, .. } => ext,
                };
                return Row {
                    label: text.into(),
                    kind: RowKind::Field {
                        cursor,
                        block: editing.input.mode() != Mode::Insert,
                        suffix,
                    },
                    ..Row::default()
                };
            }
        }
        let Some(item) = self.item(ix) else {
            return Row::default();
        };
        let hit = &self.hits[ix - usize::from(self.new_row())];
        let index = self.index.borrow();
        let detail = match item {
            FolderItem::Folder { items, .. } => items.to_string(),
            FolderItem::Note(i) => age(index.notes()[*i].modified),
        };
        let cut = self
            .cut
            .as_ref()
            .is_some_and(|c| c.as_path() == self.path_of(item).as_path());
        Row {
            label: self.names[hit.item].clone().into(),
            matches: hit.indices.iter().map(|&i| i as usize).collect(),
            detail: Some(detail.into()),
            cut,
            ..Row::default()
        }
    }

    fn preview(&self, ix: usize) -> Option<PreviewContent> {
        let index = self.index.borrow();
        match self.item(ix)? {
            FolderItem::Note(i) => Some(note_preview(&index.notes()[*i])),
            FolderItem::Folder { name, .. } => {
                let inside = index.folder_items(&self.child(name));
                let folders = inside
                    .iter()
                    .filter(|i| matches!(i, FolderItem::Folder { .. }))
                    .count();
                let notes = inside.len() - folders;
                let mut lines: Vec<SharedString> = inside
                    .iter()
                    .take(PREVIEW_ITEMS)
                    .map(|item| match item {
                        FolderItem::Folder { name, items } => format!("{name}/   {items}").into(),
                        FolderItem::Note(i) => {
                            let note = &index.notes()[*i];
                            format!("{}   {}", note.path.file_name(), age(note.modified)).into()
                        }
                    })
                    .collect();
                if inside.len() > PREVIEW_ITEMS {
                    lines.push(format!("… {} more", inside.len() - PREVIEW_ITEMS).into());
                }
                lines.push("".into());
                lines.push("l or ⏎ to open this folder".into());
                Some(PreviewContent {
                    title: format!("{name}/").into(),
                    meta: Some(
                        format!(
                            "{} · {} · {}",
                            plural(inside.len(), "item"),
                            plural(folders, "folder"),
                            plural(notes, "note")
                        )
                        .into(),
                    ),
                    lines,
                    code: None,
                })
            }
        }
    }

    fn confirm(&mut self, ix: usize) -> Confirm {
        match self.item(ix).cloned() {
            Some(FolderItem::Folder { name, .. }) => {
                self.enter(self.child(&name));
                Confirm::Navigate { select: 0 }
            }
            Some(FolderItem::Note(i)) => {
                let path = self.index.borrow().notes()[i].path.clone();
                Confirm::Act(PickerAction::OpenNote(path))
            }
            None => Confirm::Stay,
        }
    }

    fn claims(&self, c: char) -> bool {
        matches!(c, 'h' | 'a' | 'r' | 'd' | 'p' | 'D')
            || (c == 'u' && !self.history.borrow().is_empty())
    }

    fn claimed(&mut self, c: char, ix: usize) -> Confirm {
        let pending_d = std::mem::take(&mut self.pending_d);
        match c {
            'h' => self.up(),
            'a' => self.start_new(),
            'r' => self.start_rename(ix),
            'd' if pending_d => {
                if let Some(item) = self.item(ix).cloned() {
                    self.cut = VaultPath::new(self.path_of(&item)).ok();
                }
                Confirm::Stay
            }
            'd' => {
                self.pending_d = true;
                Confirm::Stay
            }
            'p' => self.paste(),
            'D' => self.trash(ix),
            'u' => self.undo(),
            _ => Confirm::Stay,
        }
    }

    fn capturing(&self) -> bool {
        self.editing.is_some()
    }

    fn key(&mut self, key: Key) -> Confirm {
        let Some(editing) = self.editing.as_mut() else {
            return Confirm::Stay;
        };
        match editing.input.handle_key(key, |_| false) {
            InputAction::Submit => self.commit(),
            InputAction::Close => {
                self.editing = None;
                self.refresh(None)
            }
            _ => Confirm::Stay,
        }
    }

    fn clear_on_escape(&self) -> bool {
        true
    }

    fn width(&self) -> Option<f32> {
        Some(WIDTH)
    }

    fn take_job(&mut self) -> Option<Job> {
        self.job.take()
    }

    fn job_done(&mut self, result: JobResult) {
        let JobResult::Relinked {
            done,
            message,
            undo,
        } = result
        else {
            return;
        };
        let links: usize = done.iter().map(|r| r.links).sum();
        {
            let mut index = self.index.borrow_mut();
            for relinked in &done {
                index.note_saved(relinked.note.clone(), &relinked.text);
            }
        }
        let text = if links == 0 {
            message
        } else {
            format!("{message} · {} updated", plural(links, "wikilink"))
        };
        if !done.is_empty() {
            self.effects.push(PickerEffect::Relinked(done));
        }
        self.toast(text, undo);
    }

    fn take_effects(&mut self) -> Vec<PickerEffect> {
        std::mem::take(&mut self.effects)
    }

    fn hints(&self, mode: Mode) -> Vec<Hint> {
        if self.editing.is_some() {
            return vec![
                Hint::new("⏎", "save name"),
                Hint::new("esc", "cancel"),
                Hint::new("⌃w", "delete word"),
            ];
        }
        match mode {
            Mode::Normal => {
                let mut hints = vec![Hint::new("l", "open"), Hint::new("h", "up")];
                if self.cut.is_some() {
                    hints.push(Hint::new("p", "move here"));
                } else {
                    hints.push(Hint::new("i", "filter"));
                }
                hints.extend([
                    Hint::new("a", "new"),
                    Hint::new("r", "rename"),
                    Hint::new("dd p", "move"),
                    Hint::new("D", "trash"),
                ]);
                if !self.history.borrow().is_empty() {
                    hints.push(Hint::new("u", "undo"));
                }
                hints
            }
            _ => vec![Hint::new("⏎", "open"), Hint::new("⌃w", "delete word")],
        }
    }

    fn count(&self) -> Option<SharedString> {
        if let Some(editing) = &self.editing {
            return Some(SharedString::new_static(match editing.naming {
                Naming::New => "new note",
                Naming::Rename { .. } => "renaming",
            }));
        }
        if let Some(cut) = &self.cut {
            let name = cut
                .as_path()
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default();
            return Some(format!("{name} cut · p moves it here").into());
        }
        Some(if self.hits.len() == self.items.len() {
            plural(self.items.len(), "item").into()
        } else {
            format!("{} of {}", self.hits.len(), self.items.len()).into()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::picker::{Picker, PickerOutcome};

    /// A vault on disk with its index, and a Trash folder of its own.
    struct Fixture {
        root: PathBuf,
        trash: PathBuf,
        vault: Vault,
        index: Rc<RefCell<NoteIndex>>,
        history: FileHistory,
    }

    impl Fixture {
        fn new(files: &[(&str, &str)]) -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let base =
                std::env::temp_dir().join(format!("mnote-explorer-{}-{n}", std::process::id()));
            let (root, trash) = (base.join("vault"), base.join("trash"));
            for (path, text) in files {
                let file = root.join(path);
                std::fs::create_dir_all(file.parent().expect("parent")).expect("dirs");
                std::fs::write(file, text).expect("write");
            }
            std::fs::create_dir_all(&root).expect("root");
            let vault = Vault::open(&root).expect("vault").with_trash(&trash);
            let index = Rc::new(RefCell::new(NoteIndex::build(&vault).expect("index")));
            Fixture {
                root,
                trash,
                vault,
                index,
                history: FileHistory::default(),
            }
        }

        fn picker(&self, current: Option<&str>) -> Picker {
            let current = current.map(|p| NotePath::new(p).expect("path"));
            let (explorer, select) = Explorer::new(
                self.index.clone(),
                self.vault.clone(),
                self.history.clone(),
                "~/notes".into(),
                current.as_ref(),
            );
            let mut picker = Picker::new(Box::new(explorer));
            picker.select(select);
            picker
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.root.parent().expect("base"));
        }
    }

    fn keys(picker: &mut Picker, keys: &str) -> PickerOutcome {
        let mut last = PickerOutcome::Open;
        for key in mnote_vim::parse_keys(keys) {
            last = picker.handle_key(key);
            // Run background jobs at once, as the editor would.
            if let Some(job) = picker.take_job()
                && let Some(result) = job()
            {
                picker.job_done(result);
            }
        }
        last
    }

    fn toasts(picker: &mut Picker) -> Vec<String> {
        picker
            .take_effects()
            .into_iter()
            .filter_map(|e| match e {
                PickerEffect::Toast { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn browsing_folders() {
        let fx = Fixture::new(&[
            ("inbox.md", ""),
            ("projects/mnote/ui-design.md", "# UI design\n"),
            ("projects/gpui.md", ""),
        ]);
        let mut picker = fx.picker(Some("projects/mnote/ui-design.md"));
        assert_eq!(keys(&mut picker, "<esc>h"), PickerOutcome::Open);
        keys(&mut picker, "igpu");
        assert_eq!(
            keys(&mut picker, "<cr>"),
            PickerOutcome::Act(PickerAction::OpenNote(
                NotePath::new("projects/gpui.md").expect("path")
            ))
        );
        let mut picker = fx.picker(None);
        keys(&mut picker, "proj<esc>");
        assert_eq!(
            keys(&mut picker, "<esc>"),
            PickerOutcome::Open,
            "clears the filter"
        );
        assert_eq!(keys(&mut picker, "<esc>"), PickerOutcome::Close);
    }

    #[test]
    fn rename_rewrites_links_and_undoes() {
        let fx = Fixture::new(&[("a.md", "# A\n"), ("b.md", "see [[a]]")]);
        let mut picker = fx.picker(Some("a.md"));
        keys(&mut picker, "<esc>r");
        keys(&mut picker, "<c-u>alpha<cr>");
        assert!(fx.root.join("alpha.md").is_file());
        assert_eq!(
            std::fs::read_to_string(fx.root.join("b.md")).expect("read"),
            "see [[alpha]]"
        );
        assert_eq!(
            toasts(&mut picker),
            ["Renamed a.md to alpha.md · 1 wikilink updated"]
        );
        keys(&mut picker, "u");
        assert!(fx.root.join("a.md").is_file());
        assert_eq!(
            std::fs::read_to_string(fx.root.join("b.md")).expect("read"),
            "see [[a]]"
        );
    }

    #[test]
    fn new_cut_paste_trash_and_undo() {
        let fx = Fixture::new(&[("inbox.md", ""), ("archive/old.md", "")]);
        let mut picker = fx.picker(Some("inbox.md"));
        keys(&mut picker, "<esc>aideas<cr>");
        assert!(fx.root.join("ideas.md").is_file());
        assert_eq!(toasts(&mut picker), ["Created ideas.md"]);

        // Cut inbox.md, go into archive/, paste.
        keys(&mut picker, "gg");
        let rows: Vec<String> = (0..3).map(|i| picker_label(&picker, i)).collect();
        let inbox = rows.iter().position(|r| r == "inbox.md").expect("listed");
        picker.select(inbox);
        keys(&mut picker, "dd");
        let archive = rows.iter().position(|r| r == "archive/").expect("listed");
        picker.select(archive);
        keys(&mut picker, "lp");
        assert!(fx.root.join("archive/inbox.md").is_file());
        assert_eq!(toasts(&mut picker), ["Moved inbox.md to archive/"]);

        keys(&mut picker, "D");
        assert!(fx.trash.read_dir().expect("trash").count() == 1);
        keys(&mut picker, "u");
        assert_eq!(
            fx.trash.read_dir().expect("trash").count(),
            0,
            "back from the Trash"
        );
        assert_eq!(fx.history.borrow().len(), 2, "create and move remain");
    }

    fn picker_label(picker: &Picker, ix: usize) -> String {
        picker.row_label(ix)
    }
}
