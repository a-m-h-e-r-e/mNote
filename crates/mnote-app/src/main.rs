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
                    traffic_light_position: Some(point(px(10.), px(8.))),
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
        if let Err(err) = opened {
            eprintln!("mnote: couldn't open the window: {err}");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
}
