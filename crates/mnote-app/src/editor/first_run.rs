//! First run: with no config.toml and no folder on the command line, the
//! window asks for a notes folder (an existing one, or a new `~/notes` with
//! a short tour of the keys in `welcome.md`) and shows the global keys,
//! which can be changed by pressing new ones. The choices go into
//! config.toml, and the vault opens.

use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, Keystroke, ParentElement,
    PathPromptOptions, SharedString, StatefulInteractiveElement, Styled, div, px,
};
use mnote_core::config::{Config, KeySpec, collapse_home};
use mnote_core::{NotePath, Vault};

use super::Editor;
use crate::theme::{ActiveTheme, Palette, StyledExt, Theme, radius, space, text};
use crate::ui::KeyCap;

/// Which global key is being changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyTarget {
    Summon,
    Capture,
}

/// The first-run screen's state.
pub(super) struct FirstRun {
    config: Config,
    changing: Option<KeyTarget>,
    error: Option<String>,
}

impl FirstRun {
    pub(super) fn new(config: Config) -> Self {
        FirstRun {
            config,
            changing: None,
            error: None,
        }
    }
}

/// The global key a keystroke makes, if it has option, control or command.
fn key_spec(stroke: &Keystroke) -> Option<KeySpec> {
    let m = &stroke.modifiers;
    let mut words = Vec::new();
    for (on, word) in [
        (m.control, "control"),
        (m.alt, "option"),
        (m.shift, "shift"),
        (m.platform, "command"),
    ] {
        if on {
            words.push(word);
        }
    }
    words.push(stroke.key.as_str());
    KeySpec::parse(&words.join("+"))
}

/// `~/notes`, where a new vault goes.
fn default_folder() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join("notes"))
}

/// The tour in a new vault's `welcome.md`.
fn welcome(config: &Config) -> String {
    format!(
        "# Welcome to mNote\n\
         \n\
         Every note is a plain Markdown file in this folder. Vim keys work\n\
         everywhere: `i` to type, `esc` (or `jk`) to stop, `:w` saves (notes\n\
         also save on their own).\n\
         \n\
         ## From any app\n\
         \n\
         - `{summon}` brings mNote forward; `esc` hides it again\n\
         - `{capture}` opens quick capture: one line into today's note\n\
         \n\
         ## In mNote\n\
         \n\
         - `space f` finds a note; a new name makes one\n\
         - `space /` searches every note's text\n\
         - `space e` explores folders\n\
         - `space b` lists the notes linking here; `gd` follows a [[link]]\n\
         - `space m` switches styled, source and plain views\n\
         - `⌃w v` splits the window; `⌃w h` and `⌃w l` move between panes\n\
         - `⌘K` lists every command\n\
         - `space ,` opens the settings\n\
         \n\
         - [ ] Try `space x` on this line to check it off\n",
        summon = config.summon.label(),
        capture = config.capture.label(),
    )
    // The tour names the leader as configured (`, f` with a `,` leader).
    .replace(
        "`space ",
        &format!("`{} ", mnote_core::config::leader_name(config.leader)),
    )
}

impl Editor {
    /// A key on the first-run screen. Returns false when there's no screen.
    pub(super) fn first_run_key(&mut self, stroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        let Some(run) = &mut self.first_run else {
            return false;
        };
        if let Some(target) = run.changing {
            if stroke.key == "escape" {
                run.changing = None;
            } else if let Some(spec) = key_spec(stroke) {
                match target {
                    KeyTarget::Summon => run.config.summon = spec,
                    KeyTarget::Capture => run.config.capture = spec,
                }
                run.changing = None;
                run.error = None;
            } else {
                run.error = Some("Hold option, control or command with the key".into());
            }
            cx.notify();
            return true;
        }
        let plain =
            !(stroke.modifiers.control || stroke.modifiers.alt || stroke.modifiers.platform);
        match (stroke.key.as_str(), plain) {
            ("enter", true) => self.choose_folder(cx),
            ("n", true) => self.create_notes(cx),
            ("s", true) => run.changing = Some(KeyTarget::Summon),
            ("c", true) => run.changing = Some(KeyTarget::Capture),
            _ => {}
        }
        cx.notify();
        true
    }

    /// `⏎`: the system's folder picker; the chosen folder becomes the vault.
    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(SharedString::new_static("Use as notes folder")),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            if let Some(folder) = paths.into_iter().next() {
                let _ = this.update(cx, |this, cx| this.finish_first_run(folder, false, cx));
            }
        })
        .detach();
    }

    /// `n`: makes `~/notes` (if needed) with a welcome note in it.
    fn create_notes(&mut self, cx: &mut Context<Self>) {
        let Some(folder) = default_folder() else {
            return self.first_run_error("No home folder to put notes in".into(), cx);
        };
        if let Err(err) = std::fs::create_dir_all(&folder) {
            return self.first_run_error(format!("Couldn't make {}: {err}", folder.display()), cx);
        }
        self.finish_first_run(folder, true, cx);
    }

    fn first_run_error(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(run) = &mut self.first_run {
            run.error = Some(error);
        }
        cx.notify();
    }

    /// Saves the choices to config.toml, takes them into use and opens the
    /// vault: at `welcome.md` (written if it isn't there) for a new folder,
    /// else at `inbox.md`.
    fn finish_first_run(
        &mut self,
        folder: std::path::PathBuf,
        welcome_note: bool,
        cx: &mut Context<Self>,
    ) {
        let vault = match Vault::open(&folder) {
            Ok(vault) => vault,
            Err(err) => return self.first_run_error(err.to_string(), cx),
        };
        let Some(run) = self.first_run.take() else {
            return;
        };
        let mut config = run.config;
        config.vault = Some(folder);
        let mut problem = None;
        if let Some(path) = &self.config_path
            && let Err(err) = mnote_core::config::write_file(path, &config.template())
        {
            problem = Some(format!("Couldn't save settings: {err}"));
        }
        let note = if welcome_note {
            let note = NotePath::new("welcome.md").expect("a valid note path");
            if !vault.path_of(&note).exists()
                && let Err(err) = vault.write(&note, &welcome(&config))
            {
                problem = Some(format!("Couldn't write welcome.md: {err}"));
            }
            note
        } else {
            NotePath::new("inbox.md").expect("a valid note path")
        };
        #[cfg(target_os = "macos")]
        if let Some(err) = crate::summon::set_hotkeys(&config.summon, &config.capture).first() {
            problem = Some(err.to_string());
        }
        self.apply_config(config, cx);
        self.open_vault(vault, note, cx);
        if let Some(problem) = problem {
            self.show_notice(problem, cx);
        }
    }

    /// The first-run screen (the Onboarding design).
    pub(super) fn render_first_run(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let run = self.first_run.as_ref()?;
        let p = cx.theme().palette;
        let folder = default_folder().map_or_else(|| "~/notes".to_owned(), |f| collapse_home(&f));
        let key_row =
            |label: &'static str, spec: &KeySpec, change: &'static str, target: KeyTarget| {
                let changing = run.changing == Some(target);
                div()
                    .flex()
                    .items_center()
                    .gap(px(space::S3))
                    .px(px(14.))
                    .py(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .text_style(text::UI_STRONG)
                            .text_color(Theme::color(p.ink))
                            .child(SharedString::new_static(label)),
                    )
                    .child(KeyCap::new(if changing {
                        "…".to_owned()
                    } else {
                        spec.label()
                    }))
                    .child(
                        div()
                            .font_family(crate::theme::mono())
                            .text_size(px(11.))
                            .text_color(Theme::color(if changing { p.cursor } else { p.ink_3 }))
                            .child(if changing {
                                SharedString::new_static("press the new keys · esc keeps it")
                            } else {
                                SharedString::new_static(change)
                            }),
                    )
            };
        let element = div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(space::S8))
            .px(px(space::S4))
            .bg(Theme::color(p.paper))
            .child(
                div()
                    .w(px(18.))
                    .h(px(34.))
                    .rounded(px(4.))
                    .bg(Theme::color(p.cursor)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(space::S2))
                    .max_w(px(460.))
                    .child(
                        div()
                            .text_style(text::NOTE_TITLE)
                            .text_color(Theme::color(p.ink))
                            .child(SharedString::new_static("Choose your notes folder")),
                    )
                    .child(
                        div()
                            .text_style(text::UI)
                            .text_color(Theme::color(p.ink_2))
                            .text_center()
                            .child(SharedString::new_static(
                                "mNote keeps every note as a plain Markdown file in one folder. \
                                 An existing Obsidian vault works as it is.",
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap(px(space::S2))
                    .child(
                        button("open-folder", "Open folder", "⏎", true, p)
                            .on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx))),
                    )
                    .child(
                        button("create-notes", format!("Create {folder}"), "n", false, p)
                            .on_click(cx.listener(|this, _, _, cx| this.create_notes(cx))),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .max_w(px(460.))
                    .flex()
                    .flex_col()
                    .border_1()
                    .border_color(Theme::color(p.line))
                    .rounded(px(radius::MD))
                    .bg(Theme::color(p.overlay))
                    .child(key_row(
                        "Summon key",
                        &run.config.summon,
                        "s to change",
                        KeyTarget::Summon,
                    ))
                    .child(div().h(px(1.)).bg(Theme::color(p.line)))
                    .child(key_row(
                        "Quick capture key",
                        &run.config.capture,
                        "c to change",
                        KeyTarget::Capture,
                    )),
            )
            .children(run.error.clone().map(|error| {
                div()
                    .text_style(text::UI)
                    .text_color(Theme::color(p.danger))
                    .child(error)
            }))
            .child(
                div()
                    .font_family(crate::theme::mono())
                    .text_size(px(11.))
                    .text_color(Theme::color(p.ink_3))
                    .child(SharedString::from(
                        match crate::keymap::current().shown_for("settings") {
                            Some(keys) => format!(
                                "Saved in ~/.config/mnote/config.toml · {keys} opens it any time"
                            ),
                            None => "Saved in ~/.config/mnote/config.toml".to_owned(),
                        },
                    )),
            );
        Some(element.into_any_element())
    }
}

/// A first-run button: its label and the key that presses it.
fn button(
    id: &'static str,
    label: impl Into<SharedString>,
    key: &'static str,
    primary: bool,
    p: &Palette,
) -> gpui::Stateful<gpui::Div> {
    let (fill, ink) = if primary {
        (p.cursor, p.on_cursor)
    } else {
        (p.well, p.ink)
    };
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(space::S2))
        .h(px(36.))
        .px(px(space::S3))
        .rounded(px(radius::MD))
        .bg(Theme::color(fill))
        .text_style(text::UI_STRONG)
        .font_weight(FontWeight::MEDIUM)
        .text_color(Theme::color(ink))
        .cursor_pointer()
        .child(label.into())
        .child(
            // The key cap, in the button's own ink so it reads on the fill.
            div()
                .flex()
                .items_center()
                .justify_center()
                .min_w(px(20.))
                .h(px(20.))
                .px(px(space::S1))
                .border_1()
                .border_color(Theme::color(ink))
                .rounded(px(radius::SM))
                .text_style(text::HINT)
                .text_color(Theme::color(ink))
                .child(SharedString::new_static(key)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(text: &str) -> Keystroke {
        Keystroke::parse(text).expect("keystroke")
    }

    #[test]
    fn keystrokes_become_global_keys() {
        assert_eq!(
            key_spec(&stroke("alt-n")).map(|k| k.label()),
            Some("⌥N".into())
        );
        assert_eq!(
            key_spec(&stroke("ctrl-shift-space")).map(|k| k.label()),
            Some("⌃⇧Space".into())
        );
        assert_eq!(
            key_spec(&stroke("n")),
            None,
            "a global key needs a modifier"
        );
    }

    #[test]
    fn the_welcome_note_names_the_keys() {
        let text = welcome(&Config::default());
        assert!(text.starts_with("# Welcome to mNote\n"));
        assert!(text.contains("`⌥N` brings mNote forward"));
        assert!(text.contains("`⌥M` opens quick capture"));
    }
}
