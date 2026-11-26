//! Untitled notes (tab pages design): `:tabnew` with no name opens an
//! empty note with no path, as Neovim's empty buffer. Its text is kept as a
//! draft in mNote's config folder until it's named; the first `:w` opens
//! the save prompt (the command line with `:saveas` and a suggested path),
//! and `:w {path}` or `:saveas {path}` names it straight away.

use std::path::{Path, PathBuf};

use mnote_core::config::Config;
use mnote_core::{Buffer, NotePath};

use super::panes::{DocKey, Source};
use super::*;

/// Longest name suggested from a note's first line.
const SUGGESTED_LEN: usize = 60;

impl Editor {
    /// An untitled note in the focused pane, in the folder of the note it
    /// came from.
    pub(super) fn open_untitled(&mut self, cx: &mut Context<Self>) {
        self.next_untitled += 1;
        let n = self.next_untitled;
        let folder = match &self.source {
            Source::Note { note, .. } => note
                .as_path()
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            _ => String::new(),
        };
        self.untitled_folders.insert(n, folder);
        let doc = self.new_doc(Buffer::new(""), Source::Untitled(n));
        self.show_doc(doc);
        self.code_file_opened(cx);
        cx.notify();
    }

    /// Where untitled note `n`'s draft is kept.
    pub(super) fn draft_path(n: u32) -> Option<PathBuf> {
        let dir = Config::path()?.parent()?.join("drafts");
        Some(dir.join(format!("untitled-{}-{n}.md", std::process::id())))
    }

    /// Keeps the focused untitled note's text as a draft a moment after
    /// it changes (autosave's timer, written to the drafts folder).
    pub(super) fn schedule_draft(&mut self, cx: &mut Context<Self>) {
        self.autosave_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(mnote_core::Autosave::DEFAULT_DELAY)
                .await;
            let _ = this.update(cx, |this, _| this.write_draft());
        }));
    }

    fn write_draft(&mut self) {
        let Source::Untitled(n) = self.source else {
            return;
        };
        let Some(path) = Self::draft_path(n) else {
            return;
        };
        if self.buffer.len_chars() == 0 {
            let _ = std::fs::remove_file(&path);
        } else if let Err(err) = mnote_core::config::write_file(&path, &self.buffer.to_string()) {
            self.notice = Some(format!("Couldn't keep a draft. {err}").into());
        }
    }

    /// The draft of untitled note `n` isn't needed any more.
    pub(super) fn remove_draft(n: u32) {
        if let Some(path) = Self::draft_path(n) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// The save prompt: the command line, holding `:saveas` and a path
    /// suggested from the note's first heading or line. `⇥` completes it.
    pub(super) fn save_prompt(&mut self, cx: &mut Context<Self>) {
        let suggested = self.suggested_path();
        self.press(mnote_vim::Key::Char(':'), cx);
        for c in format!("saveas {suggested}").chars() {
            self.press(mnote_vim::Key::Char(c), cx);
        }
    }

    /// A path for the focused note: its folder, and a name from its first
    /// heading (or line).
    fn suggested_path(&self) -> String {
        let folder = match self.source {
            Source::Untitled(n) => self.untitled_folders.get(&n).cloned().unwrap_or_default(),
            _ => String::new(),
        };
        let name = suggested_name(&self.buffer.to_string());
        if folder.is_empty() {
            name
        } else {
            format!("{folder}/{name}")
        }
    }

    /// `:saveas[!] {path}`: writes the note to `path` in the vault, and the
    /// buffer is that note from now on (an untitled or scratch note gets
    /// its name). No extension means `.md`; another one sets the language.
    pub(super) fn save_as(&mut self, arg: Option<&str>, force: bool, cx: &mut Context<Self>) {
        let Some(arg) = arg.map(str::trim).filter(|a| !a.is_empty()) else {
            self.reply = Some(("E471: Argument required".into(), true));
            return;
        };
        let Some(vault) = self.vault.clone() else {
            self.reply = Some(("A scratch note has no vault to save into".into(), true));
            return;
        };
        if matches!(self.source, Source::Config(_)) {
            self.reply = Some(("config.toml stays where it is".into(), true));
            return;
        }
        let note = match vault_path(arg) {
            Some(note) => note,
            None => {
                self.reply = Some((format!("E499: Can't save as {arg}").into(), true));
                return;
            }
        };
        let key = DocKey::Note(note.clone());
        if self.source.doc_key() == key {
            return self.write_command(&crate::commands::Invocation::default(), cx);
        }
        if self.parked_index(&key).is_some() {
            self.reply = Some((
                format!("E139: {} is open in another pane", note.file_name()).into(),
                true,
            ));
            return;
        }
        if vault.path_of(&note).exists() && !force {
            self.reply = Some(("E13: File exists (add ! to override)".into(), true));
            return;
        }
        let old = self.source.doc_key();
        let was = std::mem::replace(
            &mut self.source,
            Source::Note {
                vault: vault.clone(),
                note: note.clone(),
            },
        );
        if let Err(err) = self.buffer.save(&vault, &note) {
            self.source = was;
            self.reply = Some((format!("Couldn't save {note}: {err}").into(), true));
            return;
        }
        if let Source::Untitled(n) = was {
            Self::remove_draft(n);
            self.untitled_folders.remove(&n);
        }
        // A code file's language comes from its name, as opening one does.
        if !note.is_note()
            && let Some(language) =
                mnote_core::filetype::for_path(note.as_path()).and_then(|f| f.language)
        {
            self.buffer.set_language(&language);
            let tx = self.syntax_ready.clone();
            self.buffer.on_syntax_ready(move || {
                let _ = tx.try_send(());
            });
            self.code_cache.clear();
        }
        self.rename_doc_everywhere(&old, &key);
        self.notice = None;
        if note.is_note() || self.config.show_code_files {
            self.index
                .borrow_mut()
                .note_saved(note.clone(), &self.buffer.to_string());
        }
        self.reply = Some((format!("\"{}\" written", note.file_name()).into(), false));
        self.emit(events::EditorEvent::NoteSaved { note }, cx);
        cx.notify();
    }

    /// `:w {path}` on a note with a name: writes a copy there, and the
    /// buffer stays this note (Vim's).
    pub(super) fn write_copy(&mut self, arg: &str, force: bool) {
        let Some(vault) = self.vault.clone() else {
            return;
        };
        let Some(note) = vault_path(arg) else {
            self.reply = Some((format!("E499: Can't write {arg}").into(), true));
            return;
        };
        if vault.path_of(&note).exists() && !force {
            self.reply = Some(("E13: File exists (add ! to override)".into(), true));
            return;
        }
        let text = self.buffer.to_string();
        match vault.write(&note, &text) {
            Ok(()) => {
                if note.is_note() || self.config.show_code_files {
                    self.index.borrow_mut().note_saved(note.clone(), &text);
                }
                self.reply = Some((format!("\"{}\" written", note.file_name()).into(), false));
            }
            Err(err) => self.reply = Some((format!("Couldn't write {note}: {err}").into(), true)),
        }
    }
}

/// A path typed at the save prompt, as one in the vault: no extension is
/// a note (`.md` added), another one a file.
fn vault_path(arg: &str) -> Option<NotePath> {
    let has_ext = Path::new(arg).extension().is_some();
    if !has_ext {
        return NotePath::new(format!("{arg}.md")).ok();
    }
    NotePath::new(arg).ok().or_else(|| NotePath::file(arg).ok())
}

/// A file name from a note's first heading, else its first line: cut
/// short, with what can't be in a file name left out. `untitled.md` for a
/// note with no text.
pub(super) fn suggested_name(text: &str) -> String {
    let first = text.lines().map(str::trim).find(|l| !l.is_empty());
    let Some(first) = first else {
        return "untitled.md".to_owned();
    };
    let heading = text
        .lines()
        .map(str::trim)
        .find_map(|l| {
            l.strip_prefix('#')
                .map(|h| h.trim_start_matches('#').trim())
        })
        .filter(|h| !h.is_empty());
    let words = heading.unwrap_or(first);
    let cleaned: String = words
        .chars()
        .filter(|c| {
            !matches!(
                c,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' | '[' | ']'
            )
        })
        .collect();
    let mut name: String = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if name.chars().count() > SUGGESTED_LEN {
        name = name
            .chars()
            .take(SUGGESTED_LEN)
            .collect::<String>()
            .trim_end()
            .to_owned();
    }
    let name = name.trim_matches('.').to_owned();
    if name.is_empty() {
        "untitled.md".to_owned()
    } else {
        format!("{name}.md")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_the_first_heading_or_line() {
        assert_eq!(
            suggested_name("# Standup notes\n\n- a\n"),
            "standup notes.md"
        );
        assert_eq!(
            suggested_name("\n  call Sam: 3pm?\nmore"),
            "call sam 3pm.md"
        );
        assert_eq!(suggested_name("intro\n## Plan / next\n"), "plan next.md");
        assert_eq!(suggested_name(""), "untitled.md");
        assert_eq!(suggested_name("###\n"), "untitled.md");
        let long = format!("# {}", "word ".repeat(30));
        assert!(suggested_name(&long).chars().count() <= SUGGESTED_LEN + 3);
    }

    #[test]
    fn paths_without_an_extension_are_notes() {
        assert_eq!(
            vault_path("meetings/standup").map(|n| n.to_string()),
            Some("meetings/standup.md".to_owned())
        );
        let code = vault_path("src/main.rs").expect("a file");
        assert!(!code.is_note());
        assert!(vault_path("../outside").is_none());
    }
}
