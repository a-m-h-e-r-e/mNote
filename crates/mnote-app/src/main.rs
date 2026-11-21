//! mNote desktop app entry point.
//!
//! Opens one window drawn from the design-system theme. The window chrome (top
//! bar and status line) arrives in AMI-154.

use gpui::{
    App, Application, Bounds, Context, SharedString, Subscription, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use mnote_app::theme::{ActiveTheme, Theme, text};
use mnote_vim::Mode;

struct MNoteWindow {
    mode: Mode,
    _appearance: Subscription,
}

impl MNoteWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            if Theme::sync_with(window, cx) {
                cx.notify();
            }
        });
        Self {
            mode: Mode::default(),
            _appearance: appearance,
        }
    }
}

impl Render for MNoteWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.theme().palette;
        let title = text::NOTE_TITLE;
        let status = text::STATUS;
        div()
            .flex()
            .flex_col()
            .size_full()
            .justify_center()
            .items_center()
            .gap_2()
            .bg(Theme::color(p.paper))
            .child(
                div()
                    .font_family(title.font_family())
                    .font_weight(title.font_weight())
                    .text_size(title.size())
                    .line_height(title.line_height())
                    .text_color(Theme::color(p.ink))
                    .child(SharedString::new_static("mNote")),
            )
            .child(
                div()
                    .font_family(status.font_family())
                    .text_size(status.size())
                    .line_height(status.line_height())
                    .text_color(Theme::color(p.ink_3))
                    .child(SharedString::new_static(self.mode.label())),
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        Theme::init(cx);
        let bounds = Bounds::centered(None, size(px(1200.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| MNoteWindow::new(window, cx)),
        )
        .expect("failed to open the mNote window");
        cx.activate(true);
    });
}
