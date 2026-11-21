//! mNote desktop app entry point.
//!
//! For now this opens one window on the dark `paper` colour from the design
//! system, to prove the gpui toolchain works. The real window, theme and chrome
//! arrive in AMI-153 and AMI-154.

use gpui::{
    App, Application, Bounds, Context, SharedString, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, size,
};
use mnote_vim::Mode;

/// Dark theme colours from the design system's tokens.json (temporary until AMI-153).
mod colors {
    pub const PAPER: u32 = 0x282c34;
    pub const INK: u32 = 0xd7dae0;
    pub const INK_3: u32 = 0x969dab;
}

struct MNoteWindow {
    mode: Mode,
}

impl Render for MNoteWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .justify_center()
            .items_center()
            .gap_2()
            .bg(rgb(colors::PAPER))
            .child(
                div()
                    .text_3xl()
                    .text_color(rgb(colors::INK))
                    .child(SharedString::from("mNote")),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(colors::INK_3))
                    .child(SharedString::from(self.mode.label())),
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1200.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|_| MNoteWindow {
                    mode: Mode::default(),
                })
            },
        )
        .expect("failed to open the mNote window");
        cx.activate(true);
    });
}
