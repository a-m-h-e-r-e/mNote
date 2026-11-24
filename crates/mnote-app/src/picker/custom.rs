//! A picker over items someone else supplies (a plugin's
//! `mnote.picker.open`): fuzzy-matched by their text, each with an optional
//! detail, and code to run on the chosen one.

use std::rc::Rc;

use gpui::SharedString;
use mnote_core::NoteSearch;
use mnote_core::index::StringHit;
use mnote_vim::Mode;

use super::{Confirm, PickerAction, PickerCallback, PickerDelegate, PreviewContent, Row};
use crate::ui::Hint;

/// One item: what it shows and matches, and a quiet detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub text: String,
    pub detail: Option<String>,
}

/// Runs for the chosen item: its index in the items.
pub type OnSelect = Rc<dyn Fn(usize) -> PickerCallback>;

pub struct CustomPicker {
    title: String,
    items: Vec<Item>,
    haystacks: Vec<String>,
    search: NoteSearch,
    hits: Vec<StringHit>,
    on_select: OnSelect,
}

impl CustomPicker {
    pub fn new(title: String, items: Vec<Item>, on_select: OnSelect) -> Self {
        let haystacks = items
            .iter()
            .map(|i| format!("{} {}", i.text, i.detail.as_deref().unwrap_or("")))
            .collect();
        CustomPicker {
            title,
            items,
            haystacks,
            search: NoteSearch::new(),
            hits: Vec::new(),
            on_select,
        }
    }
}

impl PickerDelegate for CustomPicker {
    fn title(&self) -> Option<(SharedString, Option<SharedString>)> {
        Some((self.title.clone().into(), None))
    }

    fn scope(&self, mode: Mode) -> Option<SharedString> {
        Some(SharedString::new_static(match mode {
            Mode::Normal => "j k move · i edit query",
            _ => "filter",
        }))
    }

    fn set_query(&mut self, query: &str) {
        let haystacks = self.haystacks.iter().map(String::as_str);
        self.hits = self.search.strings(query, haystacks, self.items.len());
    }

    fn len(&self) -> usize {
        self.hits.len()
    }

    fn row(&self, ix: usize) -> Row {
        let hit = &self.hits[ix];
        let item = &self.items[hit.item];
        let len = item.text.chars().count();
        Row {
            label: item.text.clone().into(),
            matches: hit
                .indices
                .iter()
                .map(|&i| i as usize)
                .filter(|&i| i < len)
                .collect(),
            detail: item.detail.clone().map(Into::into),
            ..Row::default()
        }
    }

    fn preview(&self, _ix: usize) -> Option<PreviewContent> {
        None
    }

    fn has_preview(&self) -> bool {
        false
    }

    fn compact(&self) -> bool {
        true
    }

    fn confirm(&mut self, ix: usize) -> Confirm {
        let item = self.hits[ix].item;
        Confirm::Act(PickerAction::Callback((self.on_select)(item)))
    }

    fn hints(&self, _mode: Mode) -> Vec<Hint> {
        vec![Hint::new("⏎", "choose")]
    }

    fn count(&self) -> Option<SharedString> {
        Some(format!("{} / {}", self.hits.len(), self.items.len()).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_and_chooses_by_index() {
        let chosen = Rc::new(std::cell::Cell::new(usize::MAX));
        let seen = chosen.clone();
        let mut picker = CustomPicker::new(
            "Tasks".into(),
            vec![
                Item {
                    text: "write report".into(),
                    detail: Some("work.md".into()),
                },
                Item {
                    text: "buy milk".into(),
                    detail: None,
                },
            ],
            Rc::new(move |ix| {
                seen.set(ix);
                PickerCallback(Rc::new(|_, _| {}))
            }),
        );
        picker.set_query("");
        assert_eq!(picker.len(), 2);
        picker.set_query("milk");
        assert_eq!(picker.len(), 1);
        assert_eq!(picker.row(0).label.as_ref(), "buy milk");
        assert!(matches!(
            picker.confirm(0),
            Confirm::Act(PickerAction::Callback(_))
        ));
        assert_eq!(chosen.get(), 1, "the item's own index");
    }
}
