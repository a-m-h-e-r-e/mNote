//! A picker over every note: fuzzy-matched paths, titles and aliases from the
//! [`NoteIndex`], with the note's outline as preview. The finder (AMI-165)
//! builds on it with scopes, the create row and splits.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::SystemTime;

use gpui::SharedString;
use mnote_core::index::{HitField, NoteHit};
use mnote_core::{NoteIndex, NoteSearch};
use mnote_vim::Mode;

use super::{PickerAction, PickerDelegate, PreviewContent, Row};
use crate::ui::Hint;

/// Results kept per query; more than anyone scrolls through.
const LIMIT: usize = 200;
/// Headings the preview lists.
const PREVIEW_HEADINGS: usize = 12;

pub struct NotesPicker {
    index: Rc<RefCell<NoteIndex>>,
    search: NoteSearch,
    hits: Vec<NoteHit>,
}

impl NotesPicker {
    pub fn new(index: Rc<RefCell<NoteIndex>>) -> Self {
        NotesPicker {
            index,
            search: NoteSearch::new(),
            hits: Vec::new(),
        }
    }
}

impl PickerDelegate for NotesPicker {
    fn scope(&self, mode: Mode) -> Option<SharedString> {
        Some(SharedString::new_static(match mode {
            Mode::Normal => "j k move · i edit query",
            _ => "notes by name, title or alias",
        }))
    }

    fn set_query(&mut self, query: &str) {
        let index = self.index.borrow();
        self.hits = self.search.notes(&index, query, LIMIT);
    }

    fn len(&self) -> usize {
        self.hits.len()
    }

    fn row(&self, ix: usize) -> Row {
        let index = self.index.borrow();
        let hit = &self.hits[ix];
        let note = &index.notes()[hit.note];
        let name_start = note.file_name_start();
        let matches = match hit.field {
            HitField::Path => hit
                .indices
                .iter()
                .filter_map(|&i| (i as usize).checked_sub(name_start))
                .collect(),
            _ => Vec::new(),
        };
        let folder: String = note.path_text.chars().take(name_start).collect();
        Row {
            label: note.path.file_name().to_owned().into(),
            matches,
            detail: Some(if folder.is_empty() {
                age(note.modified).into()
            } else {
                folder.into()
            }),
        }
    }

    fn preview(&self, ix: usize) -> Option<PreviewContent> {
        let index = self.index.borrow();
        let note = &index.notes()[self.hits.get(ix)?.note];
        let mut meta = vec![note.path_text.clone()];
        if note.modified.is_some() {
            meta.push(format!("edited {} ago", age(note.modified)));
        }
        if !note.tags.is_empty() {
            let tags: Vec<String> = note.tags.iter().take(4).map(|t| format!("#{t}")).collect();
            meta.push(tags.join(" "));
        }
        let lines = note
            .headings
            .iter()
            .filter(|h| h.level > 1 || h.text != note.title)
            .take(PREVIEW_HEADINGS)
            .map(|h| {
                let indent = "    ".repeat(usize::from(h.level.saturating_sub(2)));
                SharedString::from(format!("{indent}{}", h.text))
            })
            .collect();
        Some(PreviewContent {
            title: note.title.clone().into(),
            meta: Some(meta.join(" · ").into()),
            lines,
        })
    }

    fn confirm(&mut self, ix: usize) -> Option<PickerAction> {
        let index = self.index.borrow();
        let note = &index.notes()[self.hits.get(ix)?.note];
        Some(PickerAction::OpenNote(note.path.clone()))
    }

    fn hints(&self, _mode: Mode) -> Vec<Hint> {
        vec![Hint::new("⏎", "open")]
    }

    fn count(&self) -> Option<SharedString> {
        Some(format!("{} / {}", self.hits.len(), self.index.borrow().len()).into())
    }
}

/// `4m`, `3h`, `2d`, `5w`, `1y`: how long ago `when` was.
fn age(when: Option<SystemTime>) -> String {
    let Some(ago) = when.and_then(|w| SystemTime::now().duration_since(w).ok()) else {
        return String::new();
    };
    let s = ago.as_secs();
    let (n, unit) = match s {
        0..60 => return "now".to_owned(),
        60..3_600 => (s / 60, "m"),
        3_600..86_400 => (s / 3_600, "h"),
        86_400..604_800 => (s / 86_400, "d"),
        604_800..31_536_000 => (s / 604_800, "w"),
        _ => (s / 31_536_000, "y"),
    };
    format!("{n}{unit}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnote_core::{NoteEntry, NotePath};
    use std::time::Duration;

    fn picker() -> NotesPicker {
        let entry = |p: &str, text: &str| {
            NoteEntry::new(
                NotePath::new(p).expect("path"),
                text,
                Some(SystemTime::now() - Duration::from_secs(7_200)),
            )
        };
        let index = NoteIndex::from_entries(vec![
            entry(
                "projects/gpui-notes.md",
                "# GPUI notes\n## Render\n### Elements\n",
            ),
            entry("inbox.md", ""),
        ]);
        NotesPicker::new(Rc::new(RefCell::new(index)))
    }

    #[test]
    fn rows_bold_the_file_name_and_show_the_folder() {
        let mut p = picker();
        p.set_query("gpu");
        let row = p.row(0);
        assert_eq!(row.label.as_ref(), "gpui-notes.md");
        assert_eq!(row.matches, [0, 1, 2]);
        assert_eq!(row.detail.as_ref().map(|d| d.as_ref()), Some("projects/"));
        p.set_query("inbox");
        assert_eq!(
            p.row(0).detail.as_ref().map(|d| d.as_ref()),
            Some("2h"),
            "root notes show their age"
        );
    }

    #[test]
    fn preview_lists_the_outline() {
        let mut p = picker();
        p.set_query("gpu");
        let preview = p.preview(0).expect("preview");
        assert_eq!(preview.title.as_ref(), "GPUI notes");
        assert_eq!(
            preview.lines,
            [
                SharedString::from("Render"),
                SharedString::from("    Elements")
            ]
        );
        assert!(
            preview
                .meta
                .expect("meta")
                .starts_with("projects/gpui-notes.md · edited 2h ago")
        );
        assert_eq!(p.count().as_ref().map(|c| c.as_ref()), Some("1 / 2"));
    }
}
