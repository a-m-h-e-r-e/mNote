//! mNote desktop app entry point.
//!
//! Opens the main window: the top bar, the note area on `paper`, and the status
//! line. The note itself arrives with the editor (milestone 2) and the vault
//! (AMI-155); until then the window shows an empty scratch note.

use gpui::{
    App, Application, Bounds, Context, SharedString, Subscription, TitlebarOptions, Window,
    WindowBounds, WindowOptions, div, point, prelude::*, px, size,
};
use mnote_app::theme::{ActiveTheme, Theme};
use mnote_app::ui::{StatusLine, TopBar};
use mnote_vim::Mode;

/// Width to leave for the macOS traffic-light buttons in the top bar.
const TRAFFIC_LIGHT_INSET: f32 = 76.;

/// What the chrome shows about the open note.
struct OpenNote {
    /// Vault-relative folders plus the note name without `.md`.
    path: Vec<SharedString>,
    file: SharedString,
    dirty: bool,
}

impl OpenNote {
    fn scratch() -> Self {
        Self {
            path: vec![SharedString::new_static("scratch")],
            file: SharedString::new_static("scratch.md"),
            dirty: false,
        }
    }
}

struct MNoteWindow {
    note: OpenNote,
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
            note: OpenNote::scratch(),
            mode: Mode::default(),
            _appearance: appearance,
        }
    }
}

impl Render for MNoteWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.theme().palette;
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(Theme::color(p.paper))
            .child(
                TopBar::new(self.note.path.iter().cloned())
                    .dirty(self.note.dirty)
                    .hint("⌘K commands")
                    .leading_inset(px(TRAFFIC_LIGHT_INSET)),
            )
            .child(div().flex_1().min_h_0())
            .child(
                StatusLine::new(self.mode)
                    .file(self.note.file.clone())
                    .right("0 words")
                    .right("1:1"),
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
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::new_static("mNote")),
                    appears_transparent: true,
                    // Centre the 12px buttons in the 28px top bar.
                    traffic_light_position: Some(point(px(10.), px(8.))),
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| MNoteWindow::new(window, cx)),
        )
        .expect("failed to open the mNote window");
        cx.activate(true);
    });
}
