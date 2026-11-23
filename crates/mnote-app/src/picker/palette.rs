//! The command palette (`⌘K`): every command by name, with its keys and ex
//! form, fuzzy-matched. A list without a preview; the selected command's
//! description sits under it. The `:` command line stays separate (D-002).

use gpui::SharedString;
use mnote_core::NoteSearch;
use mnote_core::index::StringHit;
use mnote_vim::Mode;

use super::{Confirm, PickerAction, PickerDelegate, PreviewContent, Row};
use std::rc::Rc;

use crate::commands::{self, Command};
use crate::ui::Hint;

pub struct CommandPalette {
    /// The registry, and the indexes of its commands the palette lists.
    all: Rc<Vec<Command>>,
    listed: Vec<usize>,
    haystacks: Vec<String>,
    search: NoteSearch,
    hits: Vec<StringHit>,
}

impl Default for CommandPalette {
    fn default() -> Self {
        let all = commands::all();
        let listed: Vec<usize> = (0..all.len()).filter(|&i| all[i].label.is_some()).collect();
        CommandPalette {
            haystacks: listed.iter().map(|&i| all[i].haystack()).collect(),
            all,
            listed,
            search: NoteSearch::new(),
            hits: Vec::new(),
        }
    }
}

impl CommandPalette {
    fn command(&self, hit: usize) -> &Command {
        &self.all[self.listed[self.hits[hit].item]]
    }
}

impl PickerDelegate for CommandPalette {
    fn prefix(&self, _query: &str) -> (&'static str, usize) {
        (">", 0)
    }

    fn scope(&self, mode: Mode) -> Option<SharedString> {
        Some(SharedString::new_static(match mode {
            Mode::Normal => "j k move · i edit query",
            _ => "commands",
        }))
    }

    fn set_query(&mut self, query: &str) {
        let haystacks = self.haystacks.iter().map(String::as_str);
        self.hits = self.search.strings(query, haystacks, self.listed.len());
    }

    fn len(&self) -> usize {
        self.hits.len()
    }

    fn row(&self, ix: usize) -> Row {
        let hit = &self.hits[ix];
        let command = self.command(ix);
        let label = command.label.clone().unwrap_or_default();
        let len = label.chars().count();
        Row {
            label: label.into(),
            // Letters matched in the keys or ex form aren't in the label.
            matches: hit
                .indices
                .iter()
                .map(|&i| i as usize)
                .filter(|&i| i < len)
                .collect(),
            detail: Some(command.detail().into()),
            ..Row::default()
        }
    }

    fn preview(&self, _ix: usize) -> Option<PreviewContent> {
        None
    }

    fn has_preview(&self) -> bool {
        false
    }

    fn footer(&self, ix: usize) -> Option<SharedString> {
        self.hits.get(ix)?;
        Some(self.command(ix).about.clone().into())
    }

    fn compact(&self) -> bool {
        true
    }

    fn confirm(&mut self, ix: usize) -> Confirm {
        Confirm::Act(PickerAction::Command(self.listed[self.hits[ix].item]))
    }

    fn hints(&self, _mode: Mode) -> Vec<Hint> {
        vec![Hint::new("⏎", "run")]
    }

    fn count(&self) -> Option<SharedString> {
        Some("⌘K here · : opens the Vim command line".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_labels_keys_and_ex_forms() {
        commands::install(crate::editor::commands::builtin());
        let mut palette = CommandPalette::default();
        palette.set_query("");
        let listed = commands::all().iter().filter(|c| c.label.is_some()).count();
        assert_eq!(palette.len(), listed, "everything in the palette, in order");
        assert_eq!(palette.row(0).label.as_ref(), "Find a note");

        palette.set_query("tog rel");
        assert_eq!(
            palette.row(0).label.as_ref(),
            "Toggle relative line numbers"
        );
        assert_eq!(
            palette.row(0).detail.as_ref().map(|d| d.as_ref()),
            Some(":set rnu!")
        );
        assert!(palette.footer(0).is_some());

        palette.set_query("rnu");
        assert_eq!(
            palette.row(0).label.as_ref(),
            "Toggle relative line numbers",
            "by ex form"
        );
        palette.set_query("q:");
        assert_eq!(
            palette.row(0).label.as_ref(),
            "Command-line history",
            "by keys"
        );
        let Confirm::Act(PickerAction::Command(i)) = palette.confirm(0) else {
            panic!("a command");
        };
        assert_eq!(
            commands::all()[i].label.as_deref(),
            Some("Command-line history")
        );
    }
}
