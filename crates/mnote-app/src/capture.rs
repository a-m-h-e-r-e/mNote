//! [`QuickCapture`]: the one-line capture panel, opened with `⌥M` from any
//! app. It floats over whatever app is in front without
//! taking it out of focus. `⏎` appends the line to the daily note's
//! `## Inbox`, `⇥` picks another note, `⌘⏎` appends and opens the note in
//! the full window, and `esc` (from NORMAL, as in every field, D-001)
//! discards it.
//!
//! The panel only gathers the line and the note; the editor that owns the
//! vault writes it (see `Editor::capture`), so an open note with unsaved
//! edits takes the line into its buffer instead of being overwritten.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AnyElement, Context, EventEmitter, FocusHandle, Focusable, Font, FontWeight,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, SharedString, Size,
    Styled, Window, div, font, prelude::FluentBuilder, px,
};
use mnote_core::index::{HitField, NoteSearch};
use mnote_core::{NoteIndex, NotePath};
use mnote_vim::{InputAction, Key, Mode, VimInput};

use crate::editor::keys;
use crate::editor::line::{Caret, LineText};
use crate::theme::{ActiveTheme, Palette, StyledExt, TextStyle, Theme, radius, size, space, text};
use crate::ui::{Hint, HintBar, ModePill};

/// Notes listed while choosing one.
const CHOICES: usize = 6;
/// The panel's height without and with the note list.
pub const PANEL_H: f32 = 116.;
const CHOOSER_H: f32 = CHOICES as f32 * size::ROW_H + 52.;

/// What the panel asks for when it closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureEvent {
    /// Append `entry` to `target`, and open it in the full window if `open`.
    Append {
        entry: String,
        target: NotePath,
        open: bool,
    },
    /// Close without saving anything.
    Discard,
}

/// A note offered while choosing: its path and the matched characters.
struct Choice {
    path: NotePath,
    label: SharedString,
    matches: Vec<usize>,
}

/// The `⇥` note chooser.
struct Chooser {
    input: VimInput,
    choices: Vec<Choice>,
    selected: usize,
}

/// The capture panel's view.
pub struct QuickCapture {
    input: VimInput,
    target: NotePath,
    index: Rc<RefCell<NoteIndex>>,
    search: NoteSearch,
    chooser: Option<Chooser>,
    /// Why the last append failed; the line stays so nothing is lost.
    error: Option<SharedString>,
    focus: FocusHandle,
}

impl EventEmitter<CaptureEvent> for QuickCapture {}

impl Focusable for QuickCapture {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl QuickCapture {
    /// An empty capture into `target` (the daily note), choosing from `index`.
    pub fn new(target: NotePath, index: Rc<RefCell<NoteIndex>>, cx: &mut Context<Self>) -> Self {
        Self {
            input: VimInput::default(),
            target,
            index,
            search: NoteSearch::new(),
            chooser: None,
            error: None,
            focus: cx.focus_handle(),
        }
    }

    /// Shows why saving failed, keeping the line.
    pub fn set_error(&mut self, error: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.error = Some(error.into());
        cx.notify();
    }

    /// The height the window needs now.
    pub fn height(&self) -> f32 {
        PANEL_H
            + if self.chooser.is_some() {
                CHOOSER_H
            } else {
                0.
            }
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        cx.stop_propagation();
        if stroke.key == "enter" && stroke.modifiers.platform && self.chooser.is_none() {
            return self.finish(true, cx);
        }
        let Some(key) = keys::vim_key(stroke) else {
            return;
        };
        let height = self.height();
        if self.chooser.is_some() {
            self.chooser_key(key);
        } else {
            self.capture_key(key, cx);
        }
        if self.height() != height {
            let width = window.bounds().size.width;
            window.resize(Size {
                width,
                height: px(self.height()),
            });
        }
        cx.notify();
    }

    fn capture_key(&mut self, key: Key, cx: &mut Context<Self>) {
        if key == Key::Tab {
            let mut chooser = Chooser {
                input: VimInput::default(),
                choices: Vec::new(),
                selected: 0,
            };
            self.refilter(&mut chooser);
            self.chooser = Some(chooser);
            return;
        }
        match self.input.handle_key(key, |_| false) {
            InputAction::Submit => self.finish(false, cx),
            InputAction::Close => cx.emit(CaptureEvent::Discard),
            _ => {}
        }
    }

    fn chooser_key(&mut self, key: Key) {
        let Some(mut chooser) = self.chooser.take() else {
            return;
        };
        if key == Key::Tab {
            return;
        }
        match chooser.input.handle_key(key, |_| false) {
            InputAction::Submit => {
                if let Some(choice) = chooser.choices.get(chooser.selected) {
                    self.target = choice.path.clone();
                    self.error = None;
                }
                return;
            }
            InputAction::Close => return,
            InputAction::Select(by) => {
                let last = chooser.choices.len().saturating_sub(1) as isize;
                chooser.selected = (chooser.selected as isize + by).clamp(0, last) as usize;
            }
            InputAction::Edited => self.refilter(&mut chooser),
            _ => {}
        }
        self.chooser = Some(chooser);
    }

    /// Lists the notes matching the chooser's query.
    fn refilter(&mut self, chooser: &mut Chooser) {
        let index = self.index.borrow();
        let hits = self.search.notes(&index, &chooser.input.text(), CHOICES);
        chooser.choices = hits
            .into_iter()
            .filter_map(|hit| {
                let entry = index.notes().get(hit.note)?;
                let matches = match hit.field {
                    HitField::Path => hit.indices.iter().map(|&i| i as usize).collect(),
                    _ => Vec::new(),
                };
                Some(Choice {
                    path: entry.path.clone(),
                    label: entry.path_text.clone().into(),
                    matches,
                })
            })
            .collect();
        chooser.selected = 0;
    }

    /// `⏎` or `⌘⏎`. An empty line appends nothing: `⏎` then just closes and
    /// `⌘⏎` just opens the note.
    fn finish(&mut self, open: bool, cx: &mut Context<Self>) {
        let entry = self.input.text();
        if entry.trim().is_empty() && !open {
            return cx.emit(CaptureEvent::Discard);
        }
        cx.emit(CaptureEvent::Append {
            entry,
            target: self.target.clone(),
            open,
        });
    }

    fn hints(&self) -> Vec<Hint> {
        if self.chooser.is_some() {
            return vec![
                Hint::new("⏎", "use this note"),
                Hint::new("⌃n ⌃p", "move"),
                Hint::new("esc", "back"),
            ];
        }
        match self.input.mode() {
            Mode::Insert => vec![
                Hint::new("⏎", "append"),
                Hint::new("⇥", "choose note"),
                Hint::new("⌘ ⏎", "open in full window"),
                Hint::new("esc", "normal"),
            ],
            _ => vec![
                Hint::new("⏎", "append"),
                Hint::new("⇥", "choose note"),
                Hint::new("⌘ ⏎", "open in full window"),
                Hint::new("esc", "discard"),
            ],
        }
    }
}

/// A one-line Vim field in mono `style`, the caret in the field's mode.
fn field(input: &VimInput, style: TextStyle, p: &Palette) -> LineText {
    let text: SharedString = input.text().into();
    let byte = |chars: usize| {
        text.char_indices()
            .nth(chars)
            .map_or(text.len(), |(b, _)| b)
    };
    let mono = Font {
        weight: style.font_weight(),
        ..font(crate::theme::mono())
    };
    let mut line = LineText::new(
        text.clone(),
        mono,
        style.size(),
        style.line_height(),
        Theme::color(p.ink),
    );
    if let Some(sel) = input.selection() {
        line = line.selection(byte(sel.start)..byte(sel.end), Theme::color(p.select));
    }
    line.caret(Some(Caret {
        byte: byte(input.cursor()),
        shape: input.mode().caret(),
        color: Theme::color(p.cursor),
        on_color: Theme::color(p.on_cursor),
    }))
}

impl Render for QuickCapture {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let p = theme.palette;
        let shadow = theme.modal_shadow();
        let body = text::NOTE_BODY;
        let quiet = |s: SharedString| -> AnyElement { div().child(s).into_any_element() };

        let mut target = vec![
            quiet("append to".into()),
            div()
                .text_color(Theme::color(p.ink_2))
                .child(SharedString::from(
                    self.target.as_path().display().to_string(),
                ))
                .into_any_element(),
            quiet(format!("· under ## {}", mnote_core::capture::INBOX).into()),
        ];
        if let Some(error) = &self.error {
            target.push(
                div()
                    .text_color(Theme::color(p.danger))
                    .child(error.clone())
                    .into_any_element(),
            );
        }

        let chooser = self.chooser.as_ref().map(|chooser| {
            let rows = chooser.choices.iter().enumerate().map(|(i, choice)| {
                let styles: Vec<_> = choice
                    .label
                    .char_indices()
                    .enumerate()
                    .filter(|(ci, _)| choice.matches.contains(ci))
                    .map(|(_, (b, c))| {
                        (
                            b..b + c.len_utf8(),
                            gpui::HighlightStyle {
                                font_weight: Some(FontWeight::SEMIBOLD),
                                color: Some(Theme::color(p.ink)),
                                ..Default::default()
                            },
                        )
                    })
                    .collect();
                div()
                    .flex()
                    .items_center()
                    .h(px(size::ROW_H))
                    .px(px(space::S3))
                    .rounded(px(radius::SM))
                    .text_style(text::UI)
                    .text_color(Theme::color(p.ink_2))
                    .when(i == chooser.selected, |row| {
                        row.bg(Theme::color(p.select))
                            .text_color(Theme::color(p.ink))
                    })
                    .child(gpui::StyledText::new(choice.label.clone()).with_highlights(styles))
            });
            div()
                .flex()
                .flex_col()
                .border_t_1()
                .border_color(Theme::color(p.line))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(space::S2))
                        .h(px(40.))
                        .px(px(space::S3))
                        .text_style(text::INPUT)
                        .child(
                            div()
                                .text_color(Theme::color(p.cursor))
                                .font_weight(FontWeight::MEDIUM)
                                .child(SharedString::new_static("note")),
                        )
                        .child(div().flex_1().min_w_0().child(field(
                            &chooser.input,
                            text::INPUT,
                            p,
                        ))),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .px(px(space::S1))
                        .pb(px(space::S1))
                        .children(rows),
                )
        });

        let mode = self
            .chooser
            .as_ref()
            .map_or(self.input.mode(), |c| c.input.mode());
        let _ = window;
        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .flex()
            .flex_col()
            .bg(Theme::color(p.overlay))
            .border_1()
            .border_color(Theme::color(p.line_strong))
            .rounded(px(radius::MD))
            .shadow(shadow)
            .overflow_hidden()
            .text_color(Theme::color(p.ink))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .pt(px(14.))
                    .px(px(14.))
                    .pb(px(12.))
                    .child(ModePill::new(if self.chooser.is_some() {
                        self.input.mode()
                    } else {
                        mode
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .mt(px(-4.))
                            .text_style(body)
                            .child(field(&self.input, body, p)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(14.))
                    .pb(px(10.))
                    .font_family(crate::theme::mono())
                    .text_size(px(11.))
                    .text_color(Theme::color(p.ink_3))
                    .children(target),
            )
            .children(chooser)
            .child(div().mt_auto().child(HintBar::new(self.hints())))
    }
}
