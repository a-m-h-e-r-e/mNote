//! [`KeyCap`] and [`HintBar`]: the keys a dialog answers to, along its bottom
//! edge.

use gpui::{App, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div, px};

use crate::theme::{ActiveTheme, StyledExt, Theme, radius, size, space, text};

/// One key in a hint: `esc`, `⌃n`, `j`.
#[derive(IntoElement)]
pub struct KeyCap(SharedString);

impl KeyCap {
    pub fn new(key: impl Into<SharedString>) -> Self {
        Self(key.into())
    }
}

impl RenderOnce for KeyCap {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = cx.theme().palette;
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .min_w(px(18.))
            .h(px(18.))
            .px(px(space::S1))
            .border_1()
            .border_color(Theme::color(p.line_strong))
            .rounded(px(radius::SM))
            .text_style(text::HINT)
            .text_color(Theme::color(p.ink_2))
            .child(self.0)
    }
}

/// A key (or keys, space-separated) and what it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub keys: &'static str,
    pub label: &'static str,
}

impl Hint {
    pub const fn new(keys: &'static str, label: &'static str) -> Self {
        Self { keys, label }
    }
}

/// The hint bar at the bottom of a dialog, with an optional count at the
/// right (`4 / 412`).
#[derive(IntoElement)]
pub struct HintBar {
    hints: Vec<Hint>,
    right: Option<SharedString>,
}

impl HintBar {
    pub fn new(hints: Vec<Hint>) -> Self {
        Self { hints, right: None }
    }

    pub fn right(mut self, right: impl Into<SharedString>) -> Self {
        self.right = Some(right.into());
        self
    }
}

impl RenderOnce for HintBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = cx.theme().palette;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(space::S4))
            .h(px(size::BAR_H))
            .px(px(space::S3))
            .border_t_1()
            .border_color(Theme::color(p.line))
            .overflow_hidden()
            .whitespace_nowrap()
            .text_style(text::HINT)
            .font_weight(gpui::FontWeight::NORMAL)
            .text_color(Theme::color(p.ink_2))
            .children(self.hints.into_iter().map(|hint| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .gap(px(3.))
                            .children(hint.keys.split(' ').map(KeyCap::new)),
                    )
                    .child(SharedString::new_static(hint.label))
            }))
            .child(div().flex_1())
            .children(
                self.right
                    .map(|right| div().text_color(Theme::color(p.ink_3)).child(right)),
            )
    }
}
