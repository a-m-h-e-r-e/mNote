//! The picker shell every list dialog shares (finder, palette, backlinks,
//! explorer): a `Modal` on the scrim with a Vim query field (D-001), the
//! result list, a preview pane and a hint bar that follows the field's mode.
//!
//! What a picker lists comes from its [`PickerDelegate`]; the shell owns the
//! query, the selection, scrolling and all the drawing. It isn't a view of its
//! own: the window that opens it routes keys here and draws [`Picker::render`]
//! on top of itself.

pub mod notes;

use std::ops::Range;

use gpui::{
    AnyElement, Font, FontWeight, HighlightStyle, IntoElement, ParentElement, SharedString, Styled,
    StyledText, div, font, prelude::FluentBuilder, px,
};
use mnote_core::NotePath;
use mnote_vim::{InputAction, Key, Mode, VimInput};

use crate::editor::line::{Caret, LineText};
use crate::theme::{Palette, StyledExt, Theme, family, radius, size, space, text};
use crate::ui::{Hint, HintBar};

/// Below this window width the preview moves under the list.
const STACK_BELOW: f32 = 700.;
/// Height of the list (and the preview beside it).
const LIST_H: f32 = 300.;
/// Rows of the list when the preview is stacked under it.
const STACKED_LIST_H: f32 = 180.;
const STACKED_PREVIEW_H: f32 = 150.;
/// Space above the modal, between it and the top bar.
const SCRIM_TOP: f32 = 48.;

/// One result row.
#[derive(Debug, Clone, Default)]
pub struct Row {
    pub label: SharedString,
    /// Characters of `label` to set bold (the fuzzy match).
    pub matches: Vec<usize>,
    /// Quiet text at the right end (a folder, an age).
    pub detail: Option<SharedString>,
}

/// What the preview pane shows for the selected row.
#[derive(Debug, Clone, Default)]
pub struct PreviewContent {
    pub title: SharedString,
    pub meta: Option<SharedString>,
    pub lines: Vec<SharedString>,
}

/// What confirming a row asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    OpenNote(NotePath),
}

/// The result of a key in the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerOutcome {
    /// Still open; redraw.
    Open,
    Close,
    /// Close and do this.
    Act(PickerAction),
}

/// What a particular picker lists and does.
pub trait PickerDelegate {
    /// The character before the query (`›`, `/`, `#`).
    fn prefix(&self) -> &'static str {
        "›"
    }

    /// An optional title row: `(title, quieter subtitle)`.
    fn title(&self) -> Option<(SharedString, Option<SharedString>)> {
        None
    }

    /// Quiet text at the right of the query field.
    fn scope(&self, _mode: Mode) -> Option<SharedString> {
        None
    }

    /// The query changed; recompute the results.
    fn set_query(&mut self, query: &str);

    /// Number of results.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn row(&self, ix: usize) -> Row;

    fn preview(&self, ix: usize) -> Option<PreviewContent>;

    /// `enter` (or `l` in NORMAL) on row `ix`.
    fn confirm(&mut self, ix: usize) -> Option<PickerAction>;

    /// Keys the hint bar shows, after the list keys the shell adds.
    fn hints(&self, _mode: Mode) -> Vec<Hint> {
        vec![Hint::new("⏎", "open")]
    }

    /// The count at the right of the hint bar.
    fn count(&self) -> Option<SharedString> {
        None
    }
}

/// An open picker.
pub struct Picker {
    input: VimInput,
    delegate: Box<dyn PickerDelegate>,
    selected: usize,
    /// First visible row.
    scroll: usize,
}

impl Picker {
    pub fn new(mut delegate: Box<dyn PickerDelegate>) -> Self {
        delegate.set_query("");
        Picker {
            input: VimInput::default(),
            delegate,
            selected: 0,
            scroll: 0,
        }
    }

    pub fn mode(&self) -> Mode {
        self.input.mode()
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Handles one key.
    pub fn handle_key(&mut self, key: Key) -> PickerOutcome {
        match self.input.handle_key(key, |c| c == 'l') {
            InputAction::Edited => {
                self.delegate.set_query(&self.input.text());
                self.selected = 0;
                self.scroll = 0;
            }
            InputAction::Moved => {}
            InputAction::Select(by) => self.move_selection(by),
            InputAction::Submit | InputAction::Claimed('l') => {
                if self.delegate.is_empty() {
                    return PickerOutcome::Open;
                }
                return match self.delegate.confirm(self.selected) {
                    Some(action) => PickerOutcome::Act(action),
                    None => PickerOutcome::Close,
                };
            }
            InputAction::Claimed(_) => {}
            InputAction::Close => return PickerOutcome::Close,
        }
        PickerOutcome::Open
    }

    /// Moves the selection, wrapping at both ends as Vim's completion menu
    /// does.
    fn move_selection(&mut self, by: isize) {
        let len = self.delegate.len();
        if len == 0 {
            return;
        }
        self.selected = (self.selected as isize + by).rem_euclid(len as isize) as usize;
    }

    /// Keeps the selection inside the `rows` visible ones.
    fn visible_rows(&mut self, rows: usize) -> Range<usize> {
        let rows = rows.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
        self.scroll..(self.scroll + rows).min(self.delegate.len())
    }

    /// The scrim, the modal and everything in it, for a window
    /// `window_width` wide. `caret_on` is the blink phase of the bar caret.
    pub fn render(&mut self, window_width: f32, caret_on: bool, p: &Palette) -> AnyElement {
        let stacked = window_width < STACK_BELOW;
        let list_h = if stacked { STACKED_LIST_H } else { LIST_H };
        let rows_fit = ((list_h - 12.) / (size::ROW_H + 1.)).floor() as usize;
        let rows = self.visible_rows(rows_fit);
        let mode = self.input.mode();

        let list = div()
            .flex()
            .flex_col()
            .gap(px(1.))
            .p(px(6.))
            .min_w_0()
            .overflow_hidden()
            .children(rows.map(|ix| render_row(&self.delegate.row(ix), ix == self.selected, p)));
        let preview = self
            .delegate
            .preview(self.selected)
            .filter(|_| !self.delegate.is_empty())
            .map(|content| render_preview(content, p));

        let body = if stacked {
            div()
                .flex()
                .flex_col()
                .child(
                    list.h(px(list_h))
                        .border_b_1()
                        .border_color(Theme::color(p.line)),
                )
                .children(preview.map(|pv| div().h(px(STACKED_PREVIEW_H)).child(pv)))
        } else {
            div()
                .flex()
                .h(px(list_h))
                // The list gets 1 part to the preview's 1.25.
                .child(
                    list.flex_none()
                        .w(gpui::relative(1. / 2.25))
                        .border_r_1()
                        .border_color(Theme::color(p.line)),
                )
                .child(div().flex_1().min_w_0().children(preview))
        };

        let mut hints = match mode {
            Mode::Normal => vec![Hint::new("j k", "move")],
            _ => vec![Hint::new("⌃n ⌃p", "move")],
        };
        hints.extend(self.delegate.hints(mode));
        hints.push(match mode {
            Mode::Normal => Hint::new("esc", "close"),
            _ => Hint::new("esc", "normal mode"),
        });
        let mut hint_bar = HintBar::new(hints);
        if let Some(count) = self.delegate.count() {
            hint_bar = hint_bar.right(count);
        }

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
            .children(self.delegate.title().map(|(title, sub)| {
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(space::S2))
                    .p(px(space::S3))
                    .border_b_1()
                    .border_color(Theme::color(p.line))
                    .text_style(text::UI_STRONG)
                    .child(title)
                    .children(sub.map(|sub| {
                        div()
                            .text_style(text::STATUS)
                            .text_color(Theme::color(p.ink_3))
                            .child(sub)
                    }))
            }))
            .child(self.render_input(mode, caret_on, p))
            .child(body)
            .child(hint_bar);

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(SCRIM_TOP))
            .px(px(space::S4))
            .bg(Theme::color(p.scrim))
            .child(modal)
            .into_any_element()
    }

    /// The query field: prefix, the text with a bar (INSERT) or block (NORMAL)
    /// caret, and the scope at the right. No mode pill (D-001).
    fn render_input(&self, mode: Mode, caret_on: bool, p: &Palette) -> impl IntoElement {
        let style = text::INPUT;
        let query: SharedString = self.input.text().into();
        let cursor = self.input.cursor();
        let byte = |chars: usize| {
            query
                .char_indices()
                .nth(chars)
                .map_or(query.len(), |(b, _)| b)
        };
        let mono = Font {
            weight: style.font_weight(),
            ..font(family::MONO)
        };
        let mut line = LineText::new(
            query.clone(),
            mono,
            style.size(),
            style.line_height(),
            Theme::color(p.ink),
        );
        if let Some(sel) = self.input.selection() {
            line = line.selection(byte(sel.start)..byte(sel.end), Theme::color(p.select));
        }
        if caret_on || mode != Mode::Insert {
            line = line.caret(Some(Caret {
                byte: byte(cursor),
                shape: mode.caret(),
                color: Theme::color(p.cursor),
                on_color: Theme::color(p.on_cursor),
            }));
        }
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(space::S2))
            .h(px(40.))
            .px(px(space::S3))
            .border_b_1()
            .border_color(Theme::color(p.line))
            .text_style(style)
            .child(
                div()
                    .flex_none()
                    .text_color(Theme::color(p.cursor))
                    .font_weight(FontWeight::MEDIUM)
                    .child(SharedString::new_static(self.delegate.prefix())),
            )
            .child(div().flex_1().min_w_0().child(line))
            .children(self.delegate.scope(mode).map(|scope| {
                div()
                    .flex_none()
                    .text_style(text::HINT)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(Theme::color(p.ink_3))
                    .child(scope)
            }))
    }
}

fn render_row(row: &Row, selected: bool, p: &Palette) -> AnyElement {
    let (ink, quiet, strong) = if selected {
        (p.select_ink, p.select_ink, p.select_ink)
    } else {
        (p.ink_2, p.ink_3, p.ink)
    };
    let label = &row.label;
    let bold: Vec<(Range<usize>, HighlightStyle)> = label
        .char_indices()
        .enumerate()
        .filter(|(i, _)| row.matches.contains(i))
        .map(|(_, (b, c))| {
            let style = HighlightStyle {
                font_weight: Some(FontWeight::SEMIBOLD),
                color: Some(Theme::color(strong)),
                ..HighlightStyle::default()
            };
            (b..b + c.len_utf8(), style)
        })
        .collect();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(space::S2))
        .h(px(size::ROW_H))
        .px(px(space::S2))
        .rounded(px(radius::SM))
        .whitespace_nowrap()
        .overflow_hidden()
        .text_style(text::UI)
        .text_color(Theme::color(ink))
        .when(selected, |row| row.bg(Theme::color(p.select)))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .child(StyledText::new(label.clone()).with_highlights(bold)),
        )
        .child(div().flex_1())
        .children(row.detail.clone().map(|detail| {
            div()
                .flex_none()
                .text_style(text::HINT)
                .font_weight(FontWeight::NORMAL)
                .text_color(Theme::color(quiet))
                .child(detail)
        }))
        .into_any_element()
}

fn render_preview(content: PreviewContent, p: &Palette) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .size_full()
        .min_w_0()
        .overflow_hidden()
        .px(px(space::S4))
        .py(px(space::S3))
        .text_style(text::UI)
        .text_color(Theme::color(p.ink_2))
        .child(
            div()
                .text_size(px(15.))
                .line_height(px(22.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(Theme::color(p.ink))
                .child(content.title),
        )
        .children(content.meta.map(|meta| {
            div()
                .mt(px(2.))
                .mb(px(space::S2))
                .text_style(text::HINT)
                .font_weight(FontWeight::NORMAL)
                .text_color(Theme::color(p.ink_3))
                .child(meta)
        }))
        .children(
            content
                .lines
                .into_iter()
                .map(|line| div().whitespace_nowrap().overflow_hidden().child(line)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fruits {
        all: Vec<&'static str>,
        shown: Vec<&'static str>,
    }

    impl PickerDelegate for Fruits {
        fn set_query(&mut self, query: &str) {
            self.shown = self
                .all
                .iter()
                .copied()
                .filter(|f| f.contains(query))
                .collect();
        }
        fn len(&self) -> usize {
            self.shown.len()
        }
        fn row(&self, ix: usize) -> Row {
            Row {
                label: self.shown[ix].into(),
                ..Row::default()
            }
        }
        fn preview(&self, _ix: usize) -> Option<PreviewContent> {
            None
        }
        fn confirm(&mut self, ix: usize) -> Option<PickerAction> {
            NotePath::new(format!("{}.md", self.shown[ix]))
                .ok()
                .map(PickerAction::OpenNote)
        }
    }

    fn picker() -> Picker {
        Picker::new(Box::new(Fruits {
            all: vec!["apple", "banana", "cherry", "grape"],
            shown: Vec::new(),
        }))
    }

    fn keys(p: &mut Picker, keys: &str) -> PickerOutcome {
        let mut last = PickerOutcome::Open;
        for key in mnote_vim::parse_keys(keys) {
            last = p.handle_key(key);
        }
        last
    }

    #[test]
    fn typing_filters_and_resets_the_selection() {
        let mut p = picker();
        keys(&mut p, "<c-n><c-n>");
        assert_eq!(p.selected(), 2);
        keys(&mut p, "an");
        assert_eq!((p.delegate.len(), p.selected()), (1, 0));
        assert_eq!(
            keys(&mut p, "<cr>"),
            PickerOutcome::Act(PickerAction::OpenNote(
                NotePath::new("banana.md").expect("path")
            ))
        );
    }

    #[test]
    fn modes_move_the_list_and_close() {
        let mut p = picker();
        keys(&mut p, "<c-p>");
        assert_eq!(p.selected(), 3, "wraps to the end");
        keys(&mut p, "<esc>");
        assert_eq!(p.mode(), Mode::Normal);
        keys(&mut p, "jj");
        assert_eq!(p.selected(), 1);
        assert!(
            matches!(keys(&mut p, "l"), PickerOutcome::Act(_)),
            "l opens in NORMAL"
        );
        let mut p = picker();
        assert_eq!(keys(&mut p, "<esc><esc>"), PickerOutcome::Close);
        let mut p = picker();
        assert_eq!(
            keys(&mut p, "zzz<cr>"),
            PickerOutcome::Open,
            "nothing to open"
        );
    }

    #[test]
    fn scrolling_keeps_the_selection_in_view() {
        let mut p = picker();
        p.selected = 3;
        assert_eq!(p.visible_rows(2), 2..4);
        p.selected = 0;
        assert_eq!(p.visible_rows(2), 0..2);
    }
}
