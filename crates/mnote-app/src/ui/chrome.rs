//! Window chrome: the [`StatusLine`] along the bottom, and the [`ModePill`]
//! that leads it.
//!
//! The note owns the window: there's no top bar (ADR-0015) and no sidebar
//! or toolbar. Panes' tab rows (see `editor::tab_line`) are the top edge; the
//! status line says where the note lives and holds the hints.

use gpui::{
    App, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div,
    prelude::FluentBuilder, px,
};
use mnote_vim::Mode;

use crate::theme::{ActiveTheme, Palette, StyledExt, Theme, radius, space, text};

/// Fill and text colour (`0xRRGGBBAA`) of the mode pill for each Vim mode.
///
/// The pill always spells the mode, so mode never depends on colour alone.
pub fn mode_colors(mode: Mode, p: &Palette) -> (u32, u32) {
    match mode {
        Mode::Normal => (p.select, p.select_ink),
        Mode::Insert => (p.insert_wash, p.insert),
        Mode::Visual | Mode::VisualLine | Mode::VisualBlock => (p.visual_wash, p.visual),
        Mode::Command => (p.well, p.ink),
        // Typing overwrites text: the warning colours, as Vim does.
        Mode::Replace => (p.danger_wash, p.danger),
    }
}

/// The Vim mode, first thing in the status line.
#[derive(IntoElement)]
pub struct ModePill {
    mode: Mode,
}

impl ModePill {
    pub fn new(mode: Mode) -> Self {
        Self { mode }
    }
}

impl RenderOnce for ModePill {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (fill, ink) = mode_colors(self.mode, cx.theme().palette);
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(20.))
            .px(px(space::S2))
            .rounded(px(radius::SM))
            .bg(Theme::color(fill))
            .text_color(Theme::color(ink))
            .text_style(text::HINT)
            .child(SharedString::new_static(self.mode.label()))
    }
}

/// The Vim status line along the bottom of the window: mode, file, a few facts
/// about the note, then right-aligned counts such as words and `line:col`.
#[derive(IntoElement)]
pub struct StatusLine {
    mode: Mode,
    file: Option<SharedString>,
    dirty: bool,
    items: Vec<SharedString>,
    hint: Option<SharedString>,
    right: Vec<SharedString>,
}

impl StatusLine {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            file: None,
            dirty: false,
            items: Vec::new(),
            hint: None,
            right: Vec::new(),
        }
    }

    /// The file name, with `.md`, as it is on disk.
    pub fn file(mut self, file: impl Into<SharedString>) -> Self {
        self.file = Some(file.into());
        self
    }

    /// Whether the note has unsaved changes: a dot after its name.
    pub fn dirty(mut self, dirty: bool) -> Self {
        self.dirty = dirty;
        self
    }

    /// One quiet hint before the counts, such as `gt next tab`.
    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Up to three facts after the file name, such as `2 backlinks`.
    pub fn item(mut self, item: impl Into<SharedString>) -> Self {
        self.items.push(item.into());
        self
    }

    /// Right-aligned values, such as `212 words` and `7:3`.
    pub fn right(mut self, value: impl Into<SharedString>) -> Self {
        self.right.push(value.into());
        self
    }
}

impl RenderOnce for StatusLine {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = cx.theme().palette;
        bar(p)
            .border_t_1()
            .child(ModePill::new(self.mode))
            .children(self.file.map(|file| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w_0()
                    .text_color(Theme::color(p.ink))
                    .child(file)
                    .when(self.dirty, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .size(px(7.))
                                .rounded_full()
                                .bg(Theme::color(p.cursor)),
                        )
                    })
            }))
            .children(self.items.into_iter().map(|item| div().child(item)))
            .child(div().flex_1())
            .children(
                self.hint
                    .map(|hint| div().text_color(Theme::color(p.ink_3)).child(hint)),
            )
            .children(self.right.into_iter().map(|value| div().child(value)))
    }
}

/// The shared shape of both bars: `bar-h` tall, on `chrome`, `status` type in
/// `ink-2`, items `space-4` apart.
fn bar(p: &Palette) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(space::S4))
        .h(px(crate::theme::bar_h()))
        .px(px(space::S3))
        .overflow_hidden()
        .whitespace_nowrap()
        .bg(Theme::color(p.chrome))
        .border_color(Theme::color(p.line))
        .text_style(text::STATUS)
        .text_color(Theme::color(p.ink_2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Appearance, Theme};

    #[test]
    fn each_mode_has_its_own_pill_colours() {
        for appearance in [Appearance::Dark, Appearance::Light] {
            let p = Theme::new(appearance).palette;
            assert_eq!(mode_colors(Mode::Normal, p), (p.select, p.select_ink));
            assert_eq!(mode_colors(Mode::Insert, p), (p.insert_wash, p.insert));
            assert_eq!(
                mode_colors(Mode::Visual, p),
                mode_colors(Mode::VisualLine, p)
            );
            assert_ne!(mode_colors(Mode::Normal, p), mode_colors(Mode::Insert, p));
            assert_ne!(mode_colors(Mode::Insert, p), mode_colors(Mode::Visual, p));
        }
    }
}
