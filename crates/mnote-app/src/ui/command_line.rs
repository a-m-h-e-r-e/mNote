//! [`CommandLine`]: Vim's `:`, `/` and `?` line. It takes the status line's
//! place with the same height and `chrome` background: only the bar's
//! contents change. No modal and no scrim: the note stays in view so the live
//! preview can be seen in it.

use std::ops::Range;

use gpui::{
    App, FontWeight, HighlightStyle, IntoElement, ParentElement, RenderOnce, SharedString, Styled,
    StyledText, Window, div, prelude::FluentBuilder, px,
};
use mnote_vim::ex::cmdline::{CommandKind, Token, tokens};

use crate::theme::{ActiveTheme, Palette, StyledExt, Theme, size, space, text};

/// The colour (`0xRRGGBBAA`) of each part of a command line.
pub fn token_color(token: Token, p: &Palette) -> u32 {
    match token {
        Token::Range => p.syn_url,
        Token::Command => p.syn_keyword,
        Token::Separator => p.ink_3,
        Token::Pattern => p.syn_strong,
        Token::Replacement => p.syn_code,
        Token::Flags => p.syn_link,
        Token::Text => p.ink,
    }
}

#[derive(IntoElement)]
pub struct CommandLine {
    /// `None` for a message on its own (`search hit BOTTOM…`).
    kind: Option<CommandKind>,
    text: SharedString,
    caret: bool,
    info: Option<SharedString>,
    error: bool,
}

impl CommandLine {
    /// The line being typed (with a caret) or the one that just ran.
    pub fn new(kind: CommandKind, text: impl Into<SharedString>) -> Self {
        Self {
            kind: Some(kind),
            text: text.into(),
            caret: false,
            info: None,
            error: false,
        }
    }

    /// A message with no command beside it.
    pub fn message(text: impl Into<SharedString>, error: bool) -> Self {
        Self {
            kind: None,
            text: text.into(),
            caret: false,
            info: None,
            error,
        }
    }

    /// Shows the bar caret after the text.
    pub fn caret(mut self, on: bool) -> Self {
        self.caret = on;
        self
    }

    /// The live count, or the result of the command, at the right end.
    pub fn info(mut self, info: impl Into<SharedString>) -> Self {
        self.info = Some(info.into());
        self
    }

    /// Shows the info as an error.
    pub fn error(mut self, error: bool) -> Self {
        self.error = error;
        self
    }
}

impl RenderOnce for CommandLine {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = cx.theme().palette;
        let highlights: Vec<(Range<usize>, HighlightStyle)> = self
            .kind
            .map(|kind| tokens(kind, &self.text))
            .unwrap_or_default()
            .into_iter()
            .map(|(range, token)| {
                let style = HighlightStyle {
                    color: Some(Theme::color(token_color(token, p))),
                    ..HighlightStyle::default()
                };
                (range, style)
            })
            .collect();
        let input = text::INPUT;
        let plain = match (self.kind, self.error) {
            (Some(_), _) => p.ink,
            (None, true) => p.danger,
            (None, false) => p.ink_2,
        };

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .h(px(size::BAR_H))
            .px(px(space::S3))
            .overflow_hidden()
            .whitespace_nowrap()
            .bg(Theme::color(p.chrome))
            .border_t_1()
            .border_color(Theme::color(p.line))
            .text_style(input)
            .text_color(Theme::color(plain))
            .children(self.kind.map(|kind| {
                div()
                    .flex_none()
                    .mr(px(2.))
                    .text_color(Theme::color(p.cursor))
                    .font_weight(FontWeight::MEDIUM)
                    .child(SharedString::from(kind.prefix().to_string()))
            }))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .child(StyledText::new(self.text.clone()).with_highlights(highlights)),
            )
            .when(self.caret, |line| {
                line.child(
                    div()
                        .flex_none()
                        .w(px(2.))
                        .h(input.line_height())
                        .bg(Theme::color(p.cursor)),
                )
            })
            .child(div().flex_1())
            .children(self.info.map(|info| {
                div()
                    .flex_none()
                    .pl(px(space::S4))
                    .text_style(text::STATUS)
                    .text_color(Theme::color(if self.error { p.danger } else { p.ink_2 }))
                    .child(info)
            }))
    }
}

/// A message too long for the command line (`:registers`, `:set`), shown
/// above it until the next key.
#[derive(IntoElement)]
pub struct MessagePanel {
    lines: Vec<SharedString>,
}

impl MessagePanel {
    pub fn new(message: &str) -> Self {
        Self {
            lines: message.lines().map(|l| l.to_owned().into()).collect(),
        }
    }
}

impl RenderOnce for MessagePanel {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = cx.theme().palette;
        div()
            .flex()
            .flex_col()
            .flex_none()
            .max_h(px(320.))
            .overflow_hidden()
            .px(px(space::S3))
            .py(px(space::S2))
            .bg(Theme::color(p.paper))
            .border_t_1()
            .border_color(Theme::color(p.line))
            .text_style(text::CODE)
            .text_color(Theme::color(p.ink_2))
            .whitespace_nowrap()
            .children(self.lines.into_iter().map(|line| div().child(line)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Appearance, Theme};

    #[test]
    fn parts_that_sit_side_by_side_look_different() {
        for appearance in [Appearance::Dark, Appearance::Light] {
            let p = Theme::new(appearance).palette;
            let order = [
                Token::Range,
                Token::Command,
                Token::Separator,
                Token::Pattern,
                Token::Separator,
                Token::Replacement,
                Token::Separator,
                Token::Flags,
            ];
            for pair in order.windows(2) {
                assert_ne!(token_color(pair[0], p), token_color(pair[1], p));
            }
        }
    }
}
