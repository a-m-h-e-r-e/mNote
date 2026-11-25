//! Window chrome: the [`TopBar`] along the top edge, the [`StatusLine`] along
//! the bottom, and the [`ModePill`] that leads the status line.
//!
//! The note owns the window: these two bars are the only chrome. No sidebar,
//! tabs or toolbar.

use gpui::{
    App, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, RenderOnce,
    SharedString, Styled, Window, div, prelude::FluentBuilder, px,
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

/// The window's top edge: where the note lives, and whether it has unsaved
/// changes. There is no save button; `:w` and autosave handle saving.
#[derive(IntoElement)]
pub struct TopBar {
    path: Vec<SharedString>,
    dirty: bool,
    hint: Option<SharedString>,
    leading_inset: Pixels,
}

impl TopBar {
    /// `path` is the vault-relative folders plus the note name, without `.md`.
    pub fn new(path: impl IntoIterator<Item = SharedString>) -> Self {
        Self {
            path: path.into_iter().collect(),
            dirty: false,
            hint: None,
            leading_inset: px(space::S3),
        }
    }

    /// Space before the path, so it clears the macOS traffic-light buttons when
    /// the title bar is transparent.
    pub fn leading_inset(mut self, inset: Pixels) -> Self {
        self.leading_inset = inset;
        self
    }

    pub fn dirty(mut self, dirty: bool) -> Self {
        self.dirty = dirty;
        self
    }

    /// One quiet hint at the right end, such as `⌘K commands`.
    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl RenderOnce for TopBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = cx.theme().palette;
        let last = self.path.len().saturating_sub(1);
        let crumbs = self.path.into_iter().enumerate().flat_map(move |(i, seg)| {
            let sep = (i > 0).then(|| {
                div()
                    .px(px(6.))
                    .text_color(Theme::color(p.ink_3))
                    .child(SharedString::new_static("/"))
            });
            let color = if i == last { p.ink } else { p.ink_2 };
            let seg = div().text_color(Theme::color(color)).child(seg);
            sep.into_iter().chain(std::iter::once(seg))
        });

        bar(p)
            .border_b_1()
            .pl(self.leading_inset)
            // The top bar is the window's title bar: drag to move, double-click to zoom.
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count >= 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .min_w_0()
                    .truncate()
                    .children(crumbs),
            )
            .when(self.dirty, |bar| {
                bar.child(
                    div()
                        .flex_none()
                        .size(px(7.))
                        .rounded_full()
                        .bg(Theme::color(p.cursor)),
                )
            })
            .child(div().flex_1())
            .children(
                self.hint
                    .map(|hint| div().text_color(Theme::color(p.ink_3)).child(hint)),
            )
    }
}

/// The Vim status line along the bottom of the window: mode, file, a few facts
/// about the note, then right-aligned counts such as words and `line:col`.
#[derive(IntoElement)]
pub struct StatusLine {
    mode: Mode,
    file: Option<SharedString>,
    items: Vec<SharedString>,
    right: Vec<SharedString>,
}

impl StatusLine {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            file: None,
            items: Vec::new(),
            right: Vec::new(),
        }
    }

    /// The file name, with `.md`, as it is on disk.
    pub fn file(mut self, file: impl Into<SharedString>) -> Self {
        self.file = Some(file.into());
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
            .children(
                self.file
                    .map(|file| div().text_color(Theme::color(p.ink)).child(file)),
            )
            .children(self.items.into_iter().map(|item| div().child(item)))
            .child(div().flex_1())
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
