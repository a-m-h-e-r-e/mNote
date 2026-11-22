//! [`Toast`]: a short message near the bottom of the window after a file
//! operation, with the key that undoes it. No confirm dialogs: acting, then
//! offering undo, is mNote's way (AMI-169).

use gpui::{App, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div, px};

use crate::theme::{ActiveTheme, StyledExt, Theme, radius, space, text};
use crate::ui::KeyCap;

#[derive(IntoElement)]
pub struct Toast {
    text: SharedString,
    /// The key and label of an action, such as `u` `undo`.
    action: Option<(&'static str, &'static str)>,
}

impl Toast {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            action: None,
        }
    }

    pub fn action(mut self, key: &'static str, label: &'static str) -> Self {
        self.action = Some((key, label));
        self
    }
}

impl RenderOnce for Toast {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let p = theme.palette;
        div()
            .flex()
            .items_center()
            .gap(px(space::S3))
            .h(px(40.))
            .px(px(space::S3))
            .bg(Theme::color(p.overlay))
            .border_1()
            .border_color(Theme::color(p.line_strong))
            .rounded(px(radius::MD))
            .shadow(theme.modal_shadow())
            .text_style(text::UI)
            .text_color(Theme::color(p.ink))
            .whitespace_nowrap()
            .child(self.text)
            .children(self.action.map(|(key, label)| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_style(text::HINT)
                    .font_weight(gpui::FontWeight::NORMAL)
                    .text_color(Theme::color(p.ink_2))
                    .child(KeyCap::new(key))
                    .child(SharedString::new_static(label))
            }))
    }
}
