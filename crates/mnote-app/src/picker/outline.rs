//! The outline picker (AMI-240): the current file's symbols, or a note's
//! headings, nested, filtered fuzzily, the code around the selected one in
//! the preview, and ⏎ jumps there (`⌃o` comes back). The outline is built
//! on a background thread from the syntax, so the picker opens at once.

use gpui::SharedString;
use mnote_core::NoteSearch;
use mnote_core::Rope;
use mnote_core::index::StringHit;
use mnote_core::lang::outline::Symbol;
use mnote_core::lang::view::SyntaxSnapshot;
use mnote_vim::Mode;

use super::{Confirm, Job, JobResult, PickerAction, PickerDelegate, PreviewContent, Row};
use crate::ui::Hint;

/// How many lines of the file the preview shows from a symbol.
const PREVIEW_LINES: usize = 40;

pub struct OutlinePicker {
    title: String,
    text: Rope,
    /// The syntax to build the outline from, until the job takes it.
    snapshot: Option<SyntaxSnapshot>,
    symbols: Option<Vec<Symbol>>,
    haystacks: Vec<String>,
    query: String,
    search: NoteSearch,
    hits: Vec<StringHit>,
}

impl OutlinePicker {
    /// The outline of `text` (named `title`), from `snapshot`; without one
    /// (plain text) it's empty.
    pub fn new(title: String, text: Rope, snapshot: Option<SyntaxSnapshot>) -> Self {
        let symbols = snapshot.is_none().then(Vec::new);
        OutlinePicker {
            title,
            text,
            snapshot,
            symbols,
            haystacks: Vec::new(),
            query: String::new(),
            search: NoteSearch::new(),
            hits: Vec::new(),
        }
    }

    fn filter(&mut self) {
        let Some(symbols) = &self.symbols else {
            return;
        };
        let haystacks = self.haystacks.iter().map(String::as_str);
        self.hits = self.search.strings(&self.query, haystacks, symbols.len());
    }

    fn line_of(&self, symbol: &Symbol) -> usize {
        self.text
            .char_to_line(symbol.name_range.start.min(self.text.len_chars()))
    }
}

impl PickerDelegate for OutlinePicker {
    fn title(&self) -> Option<(SharedString, Option<SharedString>)> {
        Some(("Outline".into(), Some(self.title.clone().into())))
    }

    fn scope(&self, mode: Mode) -> Option<SharedString> {
        Some(SharedString::new_static(match mode {
            Mode::Normal => "j k move · i edit query",
            _ => "symbols",
        }))
    }

    fn set_query(&mut self, query: &str) {
        query.clone_into(&mut self.query);
        self.filter();
    }

    fn len(&self) -> usize {
        self.hits.len()
    }

    fn row(&self, ix: usize) -> Row {
        let Some(symbols) = &self.symbols else {
            return Row::default();
        };
        let symbol = &symbols[self.hits[ix].item];
        // Nested symbols are indented while nothing's typed.
        let indent = if self.query.is_empty() {
            "  ".repeat(symbol.depth)
        } else {
            String::new()
        };
        let shift = indent.chars().count();
        let len = symbol.name.chars().count();
        Row {
            label: format!("{indent}{}", symbol.name).into(),
            matches: self.hits[ix]
                .indices
                .iter()
                .map(|&i| i as usize)
                .filter(|&i| i < len)
                .map(|i| i + shift)
                .collect(),
            detail: Some(format!("{}  {}", symbol.kind, self.line_of(symbol) + 1).into()),
            ..Row::default()
        }
    }

    fn preview(&self, ix: usize) -> Option<PreviewContent> {
        let symbols = self.symbols.as_ref()?;
        let symbol = &symbols[self.hits.get(ix)?.item];
        let first = self.line_of(symbol);
        let last = (first + PREVIEW_LINES).min(self.text.len_lines());
        let lines = (first..last)
            .map(|l| {
                let line = self.text.line(l).to_string();
                SharedString::from(line.trim_end_matches(['\n', '\r']).to_owned())
            })
            .collect();
        Some(PreviewContent {
            title: symbol.name.clone().into(),
            meta: Some(format!("{} · line {}", symbol.kind, first + 1).into()),
            lines,
            code: None,
        })
    }

    fn confirm(&mut self, ix: usize) -> Confirm {
        let Some(symbol) = self
            .symbols
            .as_ref()
            .and_then(|s| s.get(self.hits.get(ix)?.item))
        else {
            return Confirm::Stay;
        };
        Confirm::Act(PickerAction::GoToLine(self.line_of(symbol)))
    }

    fn take_job(&mut self) -> Option<Job> {
        let snapshot = self.snapshot.take()?;
        Some(Box::new(move || {
            Some(JobResult::Outline(snapshot.outline()))
        }))
    }

    fn job_done(&mut self, result: JobResult) {
        if let JobResult::Outline(symbols) = result {
            self.haystacks = symbols.iter().map(|s| s.name.clone()).collect();
            self.symbols = Some(symbols);
            self.filter();
        }
    }

    fn hints(&self, _mode: Mode) -> Vec<Hint> {
        vec![Hint::new("⏎", "go to")]
    }

    fn count(&self) -> Option<SharedString> {
        Some(match &self.symbols {
            Some(all) => format!("{} / {}", self.hits.len(), all.len()).into(),
            None => "…".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnote_core::Buffer;

    #[test]
    fn opens_empty_fills_from_its_job_and_jumps_to_a_line() {
        let text = "fn a() {}\n\nstruct B;\n\nfn c() {\n}\n";
        let mut buf = Buffer::with_language(text, "rust");
        let _ = buf.syntax_tree_now();
        let mut picker =
            OutlinePicker::new("a.rs".into(), buf.text().clone(), buf.syntax_snapshot());
        picker.set_query("");
        assert_eq!(picker.len(), 0, "nothing until the outline is built");
        assert_eq!(picker.count().map(|c| c.to_string()).as_deref(), Some("…"));
        let job = picker.take_job().expect("a job");
        picker.job_done(job().expect("a result"));
        assert_eq!(picker.len(), 3);
        assert_eq!(picker.row(1).label.as_ref(), "B");
        picker.set_query("c");
        assert_eq!(picker.len(), 1);
        assert_eq!(picker.confirm(0), Confirm::Act(PickerAction::GoToLine(4)));
        let preview = picker.preview(0).expect("a preview");
        assert_eq!(preview.lines.first().map(|l| l.as_ref()), Some("fn c() {"));
    }
}
