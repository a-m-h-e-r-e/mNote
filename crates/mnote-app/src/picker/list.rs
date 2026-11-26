//! The results list (`:copen`): places kept with `⌃q` from content search,
//! filterable, with `⏎` going to one. `:cnext` / `:cprevious` step through the
//! same list without opening it.

use gpui::SharedString;
use mnote_core::NoteSearch;
use mnote_core::index::StringHit;
use mnote_vim::Mode;

use super::{Confirm, ListEntry, PickerAction, PickerDelegate, PreviewContent, Row, RowKind};
use crate::ui::Hint;

pub struct ListPicker {
    entries: Vec<ListEntry>,
    /// `path text` per entry, what the query matches.
    haystacks: Vec<String>,
    search: NoteSearch,
    hits: Vec<StringHit>,
}

impl ListPicker {
    pub fn new(entries: Vec<ListEntry>) -> Self {
        let haystacks = entries
            .iter()
            .map(|e| format!("{} {}", e.path, e.text))
            .collect();
        ListPicker {
            entries,
            haystacks,
            search: NoteSearch::new(),
            hits: Vec::new(),
        }
    }
}

impl PickerDelegate for ListPicker {
    fn opens_preview(&self) -> bool {
        true
    }

    fn title(&self) -> Option<(SharedString, Option<SharedString>)> {
        Some((
            "Results list".into(),
            Some(format!("{} places · :cnext :cprevious", self.entries.len()).into()),
        ))
    }

    fn scope(&self, mode: Mode) -> Option<SharedString> {
        Some(SharedString::new_static(match mode {
            Mode::Normal => "j k move · i edit query",
            _ => "filter by note or text",
        }))
    }

    fn set_query(&mut self, query: &str) {
        let haystacks = self.haystacks.iter().map(String::as_str);
        self.hits = self.search.strings(query, haystacks, self.entries.len());
    }

    fn len(&self) -> usize {
        self.hits.len()
    }

    fn row(&self, ix: usize) -> Row {
        let entry = &self.entries[self.hits[ix].item];
        Row {
            label: entry.text.clone().into(),
            detail: Some(entry.path.file_name().to_owned().into()),
            kind: RowKind::Hit {
                line: entry.line + 1,
            },
            ..Row::default()
        }
    }

    fn preview(&self, ix: usize) -> Option<PreviewContent> {
        let entry = &self.entries[self.hits.get(ix)?.item];
        Some(PreviewContent {
            title: entry.path.name().to_owned().into(),
            meta: Some(format!("{} · line {}", entry.path, entry.line + 1).into()),
            lines: vec![entry.text.clone().into()],
            code: None,
        })
    }

    fn confirm(&mut self, ix: usize) -> Confirm {
        Confirm::Act(PickerAction::ListGo(self.hits[ix].item))
    }

    fn hints(&self, _mode: Mode) -> Vec<Hint> {
        vec![Hint::new("⏎", "go")]
    }

    fn count(&self) -> Option<SharedString> {
        Some(format!("{} / {}", self.hits.len(), self.entries.len()).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnote_core::NotePath;

    #[test]
    fn filters_and_goes_to_an_entry() {
        let entry = |p: &str, line, text: &str| ListEntry {
            path: NotePath::new(p).expect("path"),
            line,
            text: text.into(),
        };
        let mut list = ListPicker::new(vec![
            entry("hotkeys.md", 11, "needs a CGEventTap"),
            entry("macos.md", 3, "event tap vs hotkey"),
        ]);
        list.set_query("");
        assert_eq!(list.len(), 2);
        list.set_query("macos");
        assert_eq!(list.len(), 1);
        assert_eq!(list.row(0).kind, RowKind::Hit { line: 4 });
        assert_eq!(list.confirm(0), Confirm::Act(PickerAction::ListGo(1)));
    }
}
