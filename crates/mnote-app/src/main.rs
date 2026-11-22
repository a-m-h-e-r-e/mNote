//! mNote desktop app entry point.
//!
//! Until first run and settings exist (AMI-177), the note comes from the
//! command line:
//!
//! - `mnote ~/notes/ideas.md` opens that note, with its folder as the vault;
//! - `mnote ~/notes` opens `inbox.md` in that vault (created on first save);
//! - `mnote` alone opens a scratch note that isn't saved.

use std::path::{Path, PathBuf};

use gpui::{
    App, Application, Bounds, SharedString, TitlebarOptions, WindowBounds, WindowOptions, point,
    prelude::*, px, size,
};
#[cfg(target_os = "macos")]
use std::rc::Rc;

#[cfg(target_os = "macos")]
use mnote_app::capture::{CaptureEvent, PANEL_H, QuickCapture};
use mnote_app::editor::Editor;
use mnote_app::theme::Theme;
use mnote_core::{NotePath, Vault};

/// What to open, worked out from the command line before the app starts.
enum Target {
    Scratch,
    Note(Vault, NotePath),
}

fn target_from_args() -> Result<Target, String> {
    let Some(arg) = std::env::args_os().nth(1) else {
        return Ok(Target::Scratch);
    };
    let path = PathBuf::from(arg);
    let (folder, file) = if path.is_dir() {
        (path.clone(), PathBuf::from("inbox.md"))
    } else {
        let folder = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let file = path
            .file_name()
            .map(PathBuf::from)
            .ok_or_else(|| format!("{} isn't a note or a folder.", path.display()))?;
        (folder.to_path_buf(), file)
    };
    let vault = Vault::open(&folder).map_err(|e| e.to_string())?;
    let note = NotePath::new(&file).map_err(|e| e.to_string())?;
    Ok(Target::Note(vault, note))
}

fn main() {
    let target = match target_from_args() {
        Ok(target) => target,
        Err(message) => {
            eprintln!("mnote: {message}");
            std::process::exit(2);
        }
    };

    Application::new().run(move |cx: &mut App| {
        Theme::init(cx);
        let bounds = Bounds::centered(None, size(px(1200.), px(720.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::new_static("mNote")),
                    appears_transparent: true,
                    // Centre the 12px buttons in the 28px top bar.
                    traffic_light_position: Some(point(px(12.), px(10.))),
                }),
                ..Default::default()
            },
            move |window, cx| {
                cx.new(|cx| match target {
                    Target::Scratch => Editor::scratch(window, cx),
                    Target::Note(vault, note) => Editor::open(vault, note, window, cx),
                })
            },
        );
        let window = match opened {
            Ok(window) => window,
            Err(err) => {
                eprintln!("mnote: couldn't open the window: {err}");
                cx.quit();
                return;
            }
        };
        summon_setup(window, cx);
        cx.activate(true);
    });
}

/// `⌥N` from any app shows mNote where you are, on any Space; it only ever
/// brings mNote forward, and `esc` in NORMAL hides it. `⌥M` opens quick
/// capture over whatever app is in front (AMI-174).
#[cfg(target_os = "macos")]
fn summon_setup(window: gpui::WindowHandle<Editor>, cx: &mut App) {
    use mnote_app::summon::{self, Hotkey};

    let _ = window.update(cx, |_, window, _| summon::join_all_spaces(window));
    let presses = match summon::register_hotkeys() {
        Ok((presses, failed)) => {
            for err in failed {
                eprintln!("mnote: {err}");
            }
            presses
        }
        Err(err) => {
            eprintln!("mnote: no global keys: {err}");
            return;
        }
    };
    let capture: CaptureSlot = Rc::default();
    cx.spawn(async move |cx| {
        while let Ok(key) = presses.recv().await {
            let capture = capture.clone();
            let _ = cx.update(|cx| {
                // An open capture panel takes the key back.
                if let Some(open) = *capture.borrow()
                    && open.update(cx, |_, w, _| w.activate_window()).is_ok()
                {
                    return;
                }
                match key {
                    Hotkey::Capture => {
                        let front = window
                            .update(cx, |_, w, _| w.is_window_active())
                            .unwrap_or(false);
                        open_capture(window, front, capture.clone(), cx);
                    }
                    Hotkey::Summon => {
                        let _ = window.update(cx, |_, window, cx| {
                            cx.activate(true);
                            window.activate_window();
                        });
                    }
                }
            });
        }
    })
    .detach();
}

/// The quick capture panel, while it's open.
#[cfg(target_os = "macos")]
type CaptureSlot = Rc<std::cell::RefCell<Option<gpui::WindowHandle<QuickCapture>>>>;

/// Opens the capture panel over the app in front. If mNote wasn't in front,
/// its window stays out of sight so the panel shows alone, and closing the
/// panel goes back to that app.
#[cfg(target_os = "macos")]
fn open_capture(
    main: gpui::WindowHandle<Editor>,
    was_front: bool,
    slot: CaptureSlot,
    cx: &mut App,
) {
    use mnote_app::summon;
    let Some(index) = main
        .update(cx, |editor, _, _| editor.capture_index())
        .ok()
        .flatten()
    else {
        // A scratch note has no vault to capture into: stay in the window.
        return;
    };
    if !was_front {
        let _ = main.update(cx, |_, window, _| summon::order_out(window));
    }
    let (year, month, day) = summon::today();
    let target = mnote_core::capture::daily_note(year, month, day);

    let panel = size(px(mnote_app::theme::size::CAPTURE_W), px(PANEL_H));
    let mut bounds = Bounds::centered(None, panel, cx);
    if let Some(display) = cx.primary_display() {
        let screen = display.bounds();
        bounds.origin.y = screen.origin.y + screen.size.height * 0.22;
    }
    let close_slot = slot.clone();
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            kind: gpui::WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: gpui::WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        move |window, cx| {
            let view = cx.new(|cx| QuickCapture::new(target, index, cx));
            window.focus(&gpui::Focusable::focus_handle(&view, cx));
            cx.subscribe(&view, move |view, event: &CaptureEvent, cx| {
                let close = |cx: &mut App| {
                    if let Some(panel) = close_slot.borrow_mut().take() {
                        let _ = panel.update(cx, |_, w, _| w.remove_window());
                    }
                };
                match event {
                    CaptureEvent::Discard => {
                        close(cx);
                        leave(main, was_front, false, cx);
                    }
                    CaptureEvent::Append {
                        entry,
                        target,
                        open,
                    } => {
                        let saved = main
                            .update(cx, |editor, _, cx| editor.capture(entry, target, *open, cx))
                            .unwrap_or_else(|_| Err("the main window is gone".into()));
                        match saved {
                            Ok(()) => {
                                close(cx);
                                leave(main, was_front, *open, cx);
                            }
                            Err(err) => view.update(cx, |panel, cx| {
                                panel.set_error(format!("not saved: {err}"), cx)
                            }),
                        }
                    }
                }
            })
            .detach();
            view
        },
    );
    match opened {
        Ok(panel) => *slot.borrow_mut() = Some(panel),
        Err(err) => eprintln!("mnote: couldn't open quick capture: {err}"),
    }
}

/// After capture: the full window if asked for or if mNote was in front,
/// else back to the app you came from.
#[cfg(target_os = "macos")]
fn leave(main: gpui::WindowHandle<Editor>, was_front: bool, open: bool, cx: &mut App) {
    if open || was_front {
        let _ = main.update(cx, |_, window, cx| {
            cx.activate(true);
            window.activate_window();
        });
    } else {
        cx.hide();
    }
}

#[cfg(not(target_os = "macos"))]
fn summon_setup(_: gpui::WindowHandle<Editor>, _: &mut App) {}
