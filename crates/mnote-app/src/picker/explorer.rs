//! The explorer (`space e`): one folder at a time, its folders then its
//! notes, with a preview of the selected one. A folder previews what's in it;
//! a note previews its outline.
//!
//! The filter is a Vim field (D-001): typing fuzzy-filters this folder's
//! list. In NORMAL mode `j` / `k` move, `l` / `⏎` open, and `h` goes up a
//! folder and lands on the one you came from. `esc` in NORMAL first clears
//! the filter, then closes.
//!
//! Listings come from the note index, so moving around reads no files. A
//! folder with no notes in it isn't listed.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::SharedString;
use mnote_core::index::StringHit;
use mnote_core::{FolderItem, NoteIndex, NotePath, NoteSearch};
use mnote_vim::Mode;

use super::finder::{age, note_preview, plural};
use super::{Confirm, PickerAction, PickerDelegate, PreviewContent, Row};
use crate::ui::Hint;

/// The explorer's width (wider than the finder, for the folder path).
const WIDTH: f32 = 900.;
/// Items a folder preview lists.
const PREVIEW_ITEMS: usize = 12;

pub struct Explorer {
    index: Rc<RefCell<NoteIndex>>,
    /// The vault folder as the title shows it (`~/notes`).
    vault_name: String,
    /// The folder shown, vault-relative, `""` for the root.
    folder: String,
    items: Vec<FolderItem>,
    names: Vec<String>,
    search: NoteSearch,
    hits: Vec<StringHit>,
}

impl Explorer {
    /// The explorer on the folder of `current` (the root without one), with
    /// `current` selected.
    pub fn new(
        index: Rc<RefCell<NoteIndex>>,
        vault_name: String,
        current: Option<&NotePath>,
    ) -> (Self, usize) {
        let folder = current
            .map(ToString::to_string)
            .and_then(|p| p.rsplit_once('/').map(|(dir, _)| dir.to_owned()))
            .unwrap_or_default();
        let mut explorer = Explorer {
            index,
            vault_name,
            folder: String::new(),
            items: Vec::new(),
            names: Vec::new(),
            search: NoteSearch::new(),
            hits: Vec::new(),
        };
        explorer.enter(folder);
        let select = current
            .and_then(|p| explorer.position_of_note(p))
            .unwrap_or(0);
        (explorer, select)
    }

    /// The explorer on `folder` (vault-relative), or the root if the index
    /// has nothing in it.
    pub fn at_folder(index: Rc<RefCell<NoteIndex>>, vault_name: String, folder: &str) -> Self {
        let folder = folder.trim_matches('/');
        let (mut explorer, _) = Self::new(index, vault_name, None);
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

    fn position_of_note(&self, path: &NotePath) -> Option<usize> {
        let index = self.index.borrow();
        self.items
            .iter()
            .position(|item| matches!(item, FolderItem::Note(i) if index.notes()[*i].path == *path))
    }

    fn position_of_folder(&self, folder: &str) -> Option<usize> {
        self.items
            .iter()
            .position(|item| matches!(item, FolderItem::Folder { name, .. } if name == folder))
    }

    fn item(&self, ix: usize) -> Option<&FolderItem> {
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
        self.hits.len()
    }

    fn row(&self, ix: usize) -> Row {
        let hit = &self.hits[ix];
        let index = self.index.borrow();
        let detail = match &self.items[hit.item] {
            FolderItem::Folder { items, .. } => items.to_string(),
            FolderItem::Note(i) => age(index.notes()[*i].modified),
        };
        Row {
            label: self.names[hit.item].clone().into(),
            matches: hit.indices.iter().map(|&i| i as usize).collect(),
            detail: Some(detail.into()),
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
        c == 'h'
    }

    fn claimed(&mut self, c: char, _ix: usize) -> Confirm {
        match c {
            'h' => self.up(),
            _ => Confirm::Stay,
        }
    }

    fn clear_on_escape(&self) -> bool {
        true
    }

    fn width(&self) -> Option<f32> {
        Some(WIDTH)
    }

    fn hints(&self, mode: Mode) -> Vec<Hint> {
        match mode {
            Mode::Normal => vec![
                Hint::new("l", "open"),
                Hint::new("h", "up"),
                Hint::new("i", "filter"),
            ],
            _ => vec![Hint::new("⏎", "open"), Hint::new("⌃w", "delete word")],
        }
    }

    fn count(&self) -> Option<SharedString> {
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
    use mnote_core::NoteEntry;

    fn index() -> Rc<RefCell<NoteIndex>> {
        let entry =
            |p: &str, text: &str| NoteEntry::new(NotePath::new(p).expect("path"), text, None);
        Rc::new(RefCell::new(NoteIndex::from_entries(vec![
            entry("inbox.md", ""),
            entry(
                "projects/mnote/ui-design.md",
                "# UI design\n## Principles\n",
            ),
            entry("projects/mnote/design/colours.md", ""),
            entry("projects/mnote/decisions.md", ""),
            entry("projects/gpui.md", ""),
        ])))
    }

    fn labels(e: &Explorer) -> Vec<String> {
        (0..e.len()).map(|i| e.row(i).label.to_string()).collect()
    }

    #[test]
    fn opens_on_the_current_note() {
        let current = NotePath::new("projects/mnote/ui-design.md").expect("path");
        let (e, select) = Explorer::new(index(), "~/notes".into(), Some(&current));
        assert_eq!(labels(&e), ["design/", "decisions.md", "ui-design.md"]);
        assert_eq!(select, 2);
        assert_eq!(
            e.title(),
            Some(("mnote".into(), Some("~/notes / projects / mnote".into())))
        );
        let preview = e.preview(0).expect("folder preview");
        assert_eq!(preview.title.as_ref(), "design/");
        assert_eq!(
            preview.meta.as_ref().map(|m| m.as_ref()),
            Some("1 item · 0 folders · 1 note")
        );
        assert_eq!(e.preview(2).expect("note").title.as_ref(), "UI design");
    }

    #[test]
    fn keys_move_through_folders() {
        let current = NotePath::new("projects/mnote/ui-design.md").expect("path");
        let (e, select) = Explorer::new(index(), "~/notes".into(), Some(&current));
        let mut picker = Picker::new(Box::new(e));
        picker.select(select);
        let mut keys = |k: &str| {
            let mut last = PickerOutcome::Open;
            for key in mnote_vim::parse_keys(k) {
                last = picker.handle_key(key);
            }
            last
        };
        // h goes up and lands on the folder we came from.
        keys("<esc>h");
        assert_eq!(keys("l"), PickerOutcome::Open, "back into mnote/");
        assert_eq!(keys("h"), PickerOutcome::Open);
        // Filtering, then esc clears it before closing.
        keys("igpu");
        assert_eq!(
            keys("<cr>"),
            PickerOutcome::Act(PickerAction::OpenNote(
                NotePath::new("projects/gpui.md").expect("path")
            ))
        );
    }

    #[test]
    fn filter_and_escape() {
        let (e, _) = Explorer::new(index(), "~/notes".into(), None);
        let mut picker = Picker::new(Box::new(e));
        for key in mnote_vim::parse_keys("proj<esc>") {
            picker.handle_key(key);
        }
        assert_eq!(picker.delegate_len(), 1, "filtered to projects/");
        assert_eq!(
            picker.handle_key(mnote_vim::Key::Esc),
            PickerOutcome::Open,
            "clears the filter"
        );
        assert_eq!(picker.delegate_len(), 2);
        assert_eq!(picker.handle_key(mnote_vim::Key::Esc), PickerOutcome::Close);
    }
}
