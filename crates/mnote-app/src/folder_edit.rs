//! The folder editor (`e` in the explorer): the folder as a Vim buffer, one
//! line per entry, with the changes it would make listed live beside it.
//! `:w` (or `esc` after editing) asks to apply them, `y` does, `n` goes back
//! to editing, `esc` discards everything. Nothing touches the disk before
//! `y`.

use gpui::{
    AnyElement, Font, FontWeight, IntoElement, ParentElement, SharedString, Styled, div, font,
    prelude::FluentBuilder, px,
};
use mnote_core::Buffer;
use mnote_core::folder_edit::{Change, FolderEntry, PlanError, folder_text, plan};
use mnote_vim::ex::AppCommand;
use mnote_vim::text::{line_count, line_len, line_of, line_start};
use mnote_vim::{Key, Mode, Vim};

use crate::editor::line::{Caret, LineText};
use crate::theme::{Palette, StyledExt, Theme, radius, size, space, text};
use crate::ui::{Hint, HintBar};

/// Height of the two panes.
const BODY_H: f32 = 340.;
/// Lines of the buffer shown at once.
const VISIBLE_LINES: usize = 13;

enum State {
    Editing,
    /// Asking to apply; `links` is the wikilink count once it's known.
    Review {
        changes: Vec<Change>,
        links: Option<usize>,
    },
}

/// What a key did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderOutcome {
    Open,
    /// Close without changing anything.
    Close,
    /// Review opened: count the wikilinks these changes would rewrite.
    CountLinks(Vec<Change>),
    /// `y`: apply these.
    Apply(Vec<Change>),
}

pub struct FolderEditor {
    /// The folder, vault-relative (`""` for the root).
    pub folder: String,
    /// How the title names it (`~/notes/projects/mnote/`).
    location: String,
    entries: Vec<FolderEntry>,
    buffer: Buffer,
    vim: Vim,
    state: State,
    plan: Result<Vec<Change>, PlanError>,
    /// A Vim message (`E37: …`).
    message: Option<String>,
}

impl FolderEditor {
    pub fn new(folder: String, location: String, entries: Vec<FolderEntry>) -> Self {
        let buffer = Buffer::new(&folder_text(&entries));
        FolderEditor {
            folder,
            location,
            entries,
            buffer,
            vim: Vim::new(),
            state: State::Editing,
            plan: Ok(Vec::new()),
            message: None,
        }
    }

    pub fn mode(&self) -> Mode {
        match self.state {
            State::Editing => self.vim.mode(),
            State::Review { .. } => Mode::Normal,
        }
    }

    fn changes(&self) -> &[Change] {
        self.plan.as_deref().unwrap_or(&[])
    }

    /// The wikilink count for the review that's open.
    pub fn set_links(&mut self, count: usize) {
        if let State::Review { links, .. } = &mut self.state {
            *links = Some(count);
        }
    }

    pub fn handle_key(&mut self, key: Key) -> FolderOutcome {
        self.message = None;
        if let State::Review { changes, .. } = &self.state {
            return match key {
                Key::Char('y') => FolderOutcome::Apply(changes.clone()),
                Key::Char('n') => {
                    self.state = State::Editing;
                    FolderOutcome::Open
                }
                Key::Esc => FolderOutcome::Close,
                _ => FolderOutcome::Open,
            };
        }

        // `esc` in NORMAL: close if nothing changed, else review.
        if key == Key::Esc && self.vim.mode() == Mode::Normal && !self.vim.is_pending() {
            return self.review();
        }
        let revision = self.buffer.revision();
        self.vim.handle_key(key, &mut self.buffer);
        if self.buffer.revision() != revision {
            self.plan = plan(&self.entries, &self.buffer.to_string());
        }
        if let Some(message) = self.vim.message() {
            self.message = Some(message.text.clone());
        }
        match self.vim.take_app_command() {
            Some(AppCommand::Write { .. } | AppCommand::WriteQuit { .. }) => self.review(),
            Some(AppCommand::Quit { force }) => {
                if self.changes().is_empty() || force {
                    FolderOutcome::Close
                } else {
                    self.message =
                        Some("E37: No write since last change (add ! to discard)".into());
                    FolderOutcome::Open
                }
            }
            Some(_) => {
                self.message = Some("Only :w, :q and :q! here".into());
                FolderOutcome::Open
            }
            None => FolderOutcome::Open,
        }
    }

    /// `:w`: the review, or closing if there's nothing to do.
    fn review(&mut self) -> FolderOutcome {
        match &self.plan {
            Err(err) => {
                self.message = Some(err.to_string());
                FolderOutcome::Open
            }
            Ok(changes) if changes.is_empty() => FolderOutcome::Close,
            Ok(changes) => {
                let changes = changes.clone();
                self.state = State::Review {
                    changes: changes.clone(),
                    links: None,
                };
                FolderOutcome::CountLinks(changes)
            }
        }
    }

    /// The dialog over the window.
    pub fn render(&self, caret_on: bool, p: &Palette) -> AnyElement {
        let (title, changes, links) = match &self.state {
            State::Editing => (
                "Edit folder as text".to_owned(),
                self.changes().to_vec(),
                None,
            ),
            State::Review { changes, links } => (
                format!(
                    "Apply {} {}?",
                    changes.len(),
                    if changes.len() == 1 {
                        "change"
                    } else {
                        "changes"
                    }
                ),
                changes.clone(),
                Some(*links),
            ),
        };
        let reviewing = links.is_some();
        let modified = !self.changes().is_empty();

        let pane_head = |label: &'static str| {
            div()
                .flex()
                .items_center()
                .gap(px(space::S2))
                .px(px(space::S3))
                .py(px(space::S2))
                .border_b_1()
                .border_color(Theme::color(p.line))
                .text_style(text::HINT)
                .font_weight(FontWeight::NORMAL)
                .text_color(Theme::color(p.ink_3))
                .child(SharedString::new_static(label))
        };

        let buffer_pane = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .border_r_1()
            .border_color(Theme::color(p.line))
            .child(
                pane_head("folder buffer")
                    .child(div().flex_1())
                    .when(modified, |head| {
                        head.child(
                            div()
                                .text_color(Theme::color(p.insert))
                                .child(SharedString::new_static("modified")),
                        )
                    }),
            )
            .child(self.render_lines(caret_on && !reviewing, p));

        let mut change_rows: Vec<AnyElement> = changes
            .iter()
            .map(|change| {
                let color = match change.verb() {
                    "move" => p.select_ink,
                    "rename" => p.insert,
                    "trash" => p.danger,
                    _ => p.cursor,
                };
                div()
                    .flex()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_none()
                            .min_w(px(64.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(Theme::color(color))
                            .child(SharedString::new_static(change.verb())),
                    )
                    .child(SharedString::from(change.to_string()))
                    .into_any_element()
            })
            .collect();
        if let Err(err) = &self.plan {
            change_rows.push(
                div()
                    .text_color(Theme::color(p.danger))
                    .child(SharedString::from(err.to_string()))
                    .into_any_element(),
            );
        } else if changes.is_empty() {
            change_rows.push(
                div()
                    .text_color(Theme::color(p.ink_3))
                    .child(SharedString::new_static(
                        "Edit the lines: rename, add a folder in front to move, delete to trash, add to create.",
                    ))
                    .into_any_element(),
            );
        }
        let notes = |text: String| {
            div()
                .mt(px(space::S1))
                .text_style(text::UI)
                .whitespace_normal()
                .text_color(Theme::color(p.ink_2))
                .child(SharedString::from(text))
        };
        let changes_pane = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .child(pane_head("changes"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px(px(space::S3))
                    .py(px(space::S2))
                    .text_style(text::STATUS)
                    .line_height(px(26.))
                    .text_color(Theme::color(p.ink))
                    .children(change_rows)
                    .children(links.map(|links| match links {
                        Some(0) => notes("No wikilinks point at these notes.".into()),
                        Some(1) => notes("1 wikilink will be updated.".into()),
                        Some(n) => notes(format!("{n} wikilinks will be updated.")),
                        None => notes("Counting wikilinks…".into()),
                    }))
                    .when(
                        reviewing && changes.iter().any(|c| matches!(c, Change::Trash(_))),
                        |pane| {
                            pane.child(notes(
                                "Trashed files can be restored from the system Trash.".into(),
                            ))
                        },
                    ),
            );

        let hints = if reviewing {
            vec![
                Hint::new("y", "apply"),
                Hint::new("n", "back to edit"),
                Hint::new("esc", "discard all"),
            ]
        } else {
            vec![
                Hint::new(":w", "review"),
                Hint::new(":q", "close"),
                Hint::new("u", "undo"),
                Hint::new("esc", "review or close"),
            ]
        };
        let right = if reviewing {
            "nothing is written until y".to_owned()
        } else {
            self.message
                .clone()
                .unwrap_or_else(|| self.vim.mode().label().to_lowercase())
        };

        let modal = div()
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(size::MODAL_W))
            .bg(Theme::color(p.overlay))
            .border_1()
            .border_color(Theme::color(p.line_strong))
            .rounded(px(radius::LG))
            .overflow_hidden()
            .text_color(Theme::color(p.ink))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(space::S2))
                    .p(px(space::S3))
                    .border_b_1()
                    .border_color(Theme::color(p.line))
                    .text_style(text::UI_STRONG)
                    .child(SharedString::from(title))
                    .child(
                        div()
                            .text_style(text::STATUS)
                            .text_color(Theme::color(p.ink_3))
                            .child(SharedString::from(self.location.clone())),
                    ),
            )
            .child(
                div()
                    .flex()
                    .h(px(BODY_H))
                    .child(buffer_pane)
                    .child(changes_pane),
            )
            .child(HintBar::new(hints).right(right));

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(48.))
            .px(px(space::S4))
            .bg(Theme::color(p.scrim))
            .child(modal)
            .into_any_element()
    }

    /// The buffer's lines around the cursor, numbered, with the caret.
    fn render_lines(&self, caret_on: bool, p: &Palette) -> impl IntoElement {
        let rope = self.buffer.text();
        let count = line_count(rope).max(1);
        let cursor = self.vim.cursor();
        let cursor_line = line_of(rope, cursor);
        let first = (cursor_line + 1)
            .saturating_sub(VISIBLE_LINES)
            .min(count.saturating_sub(VISIBLE_LINES));
        let last = (first + VISIBLE_LINES).min(count);
        let code = text::CODE;
        let mono = Font {
            weight: code.font_weight(),
            ..font(crate::theme::mono())
        };
        let mode = self.vim.mode();
        div()
            .flex()
            .flex_col()
            .py(px(space::S2))
            .children((first..last).map(|ix| {
                let start = line_start(rope, ix);
                let len = line_len(rope, ix);
                let content: SharedString = rope.slice(start..start + len).to_string().into();
                let current = ix == cursor_line;
                let mut line = LineText::new(
                    content.clone(),
                    mono.clone(),
                    code.size(),
                    px(24.),
                    Theme::color(p.ink),
                );
                if current && mode != Mode::Command && (caret_on || mode != Mode::Insert) {
                    let byte = content
                        .char_indices()
                        .nth(cursor - start)
                        .map_or(content.len(), |(b, _)| b);
                    line = line.caret(Some(Caret {
                        byte,
                        shape: mode.caret(),
                        color: Theme::color(p.cursor),
                        on_color: Theme::color(p.on_cursor),
                    }));
                }
                div()
                    .flex()
                    .when(current, |row| row.bg(Theme::color(p.well)))
                    .child(
                        div()
                            .flex_none()
                            .w(px(40.))
                            .pr(px(space::S3))
                            .text_right()
                            .text_style(text::STATUS)
                            .line_height(px(24.))
                            .text_color(Theme::color(if current { p.ink } else { p.ink_3 }))
                            .child(SharedString::from((ix + 1).to_string())),
                    )
                    .child(div().flex_1().min_w_0().child(line))
            }))
            .child(
                div()
                    .px(px(space::S3))
                    .text_style(text::STATUS)
                    .text_color(Theme::color(p.ink_3))
                    .when(self.vim.command_line().is_some(), |row| {
                        let line = self
                            .vim
                            .command_line()
                            .map(|l| format!(":{}", l.text))
                            .unwrap_or_default();
                        row.text_color(Theme::color(p.ink))
                            .child(SharedString::from(line))
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> FolderEditor {
        let e = |name: &str, is_dir| FolderEntry {
            name: name.into(),
            is_dir,
        };
        FolderEditor::new(
            "projects".into(),
            "~/notes/projects/".into(),
            vec![e("design", true), e("a.md", false), e("b.md", false)],
        )
    }

    fn keys(ed: &mut FolderEditor, keys: &str) -> FolderOutcome {
        let mut last = FolderOutcome::Open;
        for key in mnote_vim::parse_keys(keys) {
            last = ed.handle_key(key);
        }
        last
    }

    #[test]
    fn edits_then_review_then_apply() {
        let mut ed = editor();
        assert_eq!(
            keys(&mut ed, "<esc>"),
            FolderOutcome::Close,
            "nothing changed"
        );
        let mut ed = editor();
        keys(&mut ed, "jcwalpha<esc>jdd");
        assert_eq!(ed.changes().len(), 2, "a renamed, b trashed");
        let FolderOutcome::CountLinks(changes) = keys(&mut ed, ":w<cr>") else {
            panic!("review");
        };
        assert_eq!(changes.len(), 2);
        ed.set_links(3);
        assert_eq!(keys(&mut ed, "n"), FolderOutcome::Open, "back to editing");
        keys(&mut ed, "u");
        assert_eq!(ed.changes().len(), 1, "undo brings b back");
        assert!(
            matches!(keys(&mut ed, "<esc>"), FolderOutcome::CountLinks(_)),
            "esc reviews"
        );
        assert!(matches!(keys(&mut ed, "y"), FolderOutcome::Apply(c) if c.len() == 1));
    }

    #[test]
    fn quitting_and_bad_lines() {
        let mut ed = editor();
        keys(&mut ed, "dd");
        assert_eq!(
            keys(&mut ed, ":q<cr>"),
            FolderOutcome::Open,
            "E37 with changes"
        );
        assert!(ed.message.as_deref().is_some_and(|m| m.starts_with("E37")));
        assert_eq!(keys(&mut ed, ":q!<cr>"), FolderOutcome::Close);

        let mut ed = editor();
        keys(&mut ed, "ccx/../y<esc>");
        assert!(ed.plan.is_err());
        assert_eq!(
            keys(&mut ed, ":w<cr>"),
            FolderOutcome::Open,
            "a bad line stays in editing"
        );
    }
}
