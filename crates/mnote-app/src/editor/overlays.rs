//! What opens over the note: pickers (the finder, explorer, palette,
//! backlinks, results list) and their background jobs and effects, the
//! folder editor, and toasts.

use super::*;

impl Editor {
    /// ⌘K: the command palette (not over another picker).
    pub(super) fn open_palette(&mut self, cx: &mut Context<Self>) {
        if self.picker.is_none() {
            self.open_picker(Picker::new(Box::new(CommandPalette::default())), cx);
        }
    }

    /// `:keys`: every binding and its command.
    pub(super) fn list_bindings(&mut self) {
        let keymap = crate::keymap::current();
        let all = crate::commands::all();
        let mut lines: Vec<String> = keymap
            .bindings()
            .iter()
            .map(|b| {
                let label = crate::commands::by_id(&b.command)
                    .and_then(|i| all[i].label.clone())
                    .unwrap_or_default();
                format!(
                    "{:<12} {:<24} {label}",
                    crate::keymap::show(&b.strokes),
                    b.command
                )
            })
            .collect();
        lines.insert(0, "Keys         Command                  ".to_owned());
        self.vim.show_message(lines.join("\n"));
    }

    /// `space b`: the notes that link to this one.
    pub(super) fn open_backlinks(&mut self, cx: &mut Context<Self>) {
        let Source::Note { vault, note, .. } = &self.source else {
            self.reply = Some(("A scratch note has no backlinks".into(), true));
            return;
        };
        let picker = BacklinksPicker::new(&self.index.borrow(), note, vault.clone());
        self.open_picker(Picker::new(Box::new(picker)), cx);
    }

    /// Opens the finder with `query` already typed.
    pub(super) fn open_finder(&mut self, query: &str, cx: &mut Context<Self>) {
        let current = CurrentNote {
            path: self.source.key().cloned(),
            text: self.buffer.to_string(),
        };
        let vault = self.vault.clone();
        let mut picker = Picker::new(Box::new(Finder::new(self.index.clone(), current, vault)));
        if !query.is_empty() {
            picker.set_query(query);
        }
        self.open_picker(picker, cx);
    }

    /// Opens the explorer on the open note's folder, or on `folder`.
    pub(super) fn open_explorer(&mut self, folder: Option<String>, cx: &mut Context<Self>) {
        // File operations read and rewrite notes on disk: save first.
        self.save_now(cx);
        let Source::Note { vault, note, .. } = &self.source else {
            self.reply = Some(("A scratch note isn't in a vault to explore".into(), true));
            return;
        };
        let name = vault_name(vault.root());
        let picker = match folder {
            Some(folder) => Picker::new(Box::new(Explorer::at_folder(
                self.index.clone(),
                vault.clone(),
                self.file_history.clone(),
                name,
                &folder,
            ))),
            None => {
                let (explorer, select) = Explorer::new(
                    self.index.clone(),
                    vault.clone(),
                    self.file_history.clone(),
                    name,
                    Some(note),
                );
                let mut picker = Picker::new(Box::new(explorer));
                picker.select(select);
                picker
            }
        };
        self.open_picker(picker, cx);
    }

    pub(super) fn open_picker(&mut self, picker: Picker, cx: &mut Context<Self>) {
        self.picker = Some(picker);
        self.restart_blink(cx);
        self.run_picker_job(cx);
        cx.notify();
    }

    /// Opens the folder editor on `folder` (vault-relative).
    pub(super) fn open_folder_editor(&mut self, folder: String, cx: &mut Context<Self>) {
        let Source::Note { vault, .. } = &self.source else {
            return;
        };
        let entries = self
            .index
            .borrow()
            .folder_items(&folder)
            .into_iter()
            .map(|item| match item {
                mnote_core::FolderItem::Folder { name, .. } => FolderEntry { name, is_dir: true },
                mnote_core::FolderItem::Note(i) => FolderEntry {
                    name: self.index.borrow().notes()[i].path.file_name().to_owned(),
                    is_dir: false,
                },
            })
            .collect();
        let mut location = vault_name(vault.root());
        location.push('/');
        if !folder.is_empty() {
            location.push_str(&folder);
            location.push('/');
        }
        self.folder_edit = Some(FolderEditor::new(folder, location, entries));
        self.restart_blink(cx);
        cx.notify();
    }

    /// The vault-relative path of `rel` in `folder`.
    pub(super) fn in_folder(folder: &str, rel: &str) -> std::path::PathBuf {
        let rel = rel.trim_end_matches('/');
        if folder.is_empty() {
            rel.into()
        } else {
            std::path::Path::new(folder).join(rel)
        }
    }

    /// Counts, in the background, the wikilinks the reviewed changes would
    /// rewrite.
    pub(super) fn count_folder_links(&mut self, changes: &[Change], cx: &mut Context<Self>) {
        let (Source::Note { vault, .. }, Some(editor)) = (&self.source, &self.folder_edit) else {
            return;
        };
        let index = self.index.borrow();
        let moves: Vec<LinkMove> = changes
            .iter()
            .filter_map(|c| match c {
                Change::Move { from, to } => Some(index.would_move(
                    &Self::in_folder(&editor.folder, from),
                    &Self::in_folder(&editor.folder, to),
                )),
                _ => None,
            })
            .flatten()
            .collect();
        let notes: Vec<NotePath> = index.notes().iter().map(|n| n.path.clone()).collect();
        drop(index);
        let vault = vault.clone();
        let task = cx
            .background_executor()
            .spawn(async move { count_relinks(&vault, &notes, &moves) });
        cx.spawn(async move |this, cx| {
            let count = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(editor) = this.folder_edit.as_mut() {
                    editor.set_links(count);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// `y` in the folder editor: runs the changes, records them for undo,
    /// updates the index, rewrites links, and shows the folder again.
    pub(super) fn apply_folder_changes(
        &mut self,
        folder: String,
        changes: &[Change],
        cx: &mut Context<Self>,
    ) {
        let Source::Note { vault, .. } = &self.source else {
            return;
        };
        let vault = vault.clone();
        let (ops, error) = mnote_core::folder_edit::apply(&vault, &folder, changes);
        let mut moves = Vec::new();
        let mut effects = Vec::new();
        for op in &ops {
            let label = match op {
                FileOp::Created(note) => {
                    self.index.borrow_mut().note_saved(note.clone(), "");
                    format!("Created {}", note.file_name())
                }
                FileOp::Moved { from, to } => {
                    moves.extend(self.index.borrow_mut().moved(from.as_path(), to.as_path()));
                    effects.push(PickerEffect::Moved {
                        from: from.as_path().to_path_buf(),
                        to: to.as_path().to_path_buf(),
                    });
                    format!("Moved {from} to {to}")
                }
                FileOp::Trashed { from, .. } => {
                    self.index.borrow_mut().remove_under(from.as_path());
                    format!("{from} moved to Trash")
                }
            };
            self.file_history.borrow_mut().push(FileUndo {
                op: op.clone(),
                label,
            });
        }
        self.apply_effects(effects, cx);
        let message = match &error {
            Some(err) => format!(
                "Stopped after {} of {} changes: {err}",
                ops.len(),
                changes.len()
            ),
            None => format!(
                "Applied {} {}",
                changes.len(),
                if changes.len() == 1 {
                    "change"
                } else {
                    "changes"
                }
            ),
        };
        if moves.is_empty() {
            self.show_toast(message, false, cx);
        } else {
            let notes: Vec<NotePath> = self
                .index
                .borrow()
                .notes()
                .iter()
                .map(|n| n.path.clone())
                .collect();
            let job: Job = Box::new(move || {
                let done = relink_vault(&vault, &notes, &moves);
                Some(JobResult::Relinked {
                    done,
                    message,
                    undo: false,
                })
            });
            self.spawn_job(job, cx);
        }
        self.open_explorer(Some(folder), cx);
    }

    /// Runs the picker's background work (a content search) off the UI
    /// thread.
    pub(super) fn run_picker_job(&mut self, cx: &mut Context<Self>) {
        if let Some(job) = self.picker.as_mut().and_then(Picker::take_job) {
            self.spawn_job(job, cx);
        }
    }

    /// Runs `job` off the UI thread and hands its result to the picker, or,
    /// if the picker closed meanwhile, applies what still matters (rewritten
    /// links).
    pub(super) fn spawn_job(&mut self, job: Job, cx: &mut Context<Self>) {
        let task = cx.background_executor().spawn(async move { job() });
        cx.spawn(async move |this, cx| {
            let Some(result) = task.await else { return };
            let _ = this.update(cx, |this, cx| {
                if let Some(picker) = this.picker.as_mut() {
                    picker.job_done(result);
                    let effects = picker.take_effects();
                    this.apply_effects(effects, cx);
                    this.run_picker_job(cx);
                } else if let JobResult::Relinked {
                    done,
                    message,
                    undo,
                } = result
                {
                    let links: usize = done.iter().map(|r| r.links).sum();
                    let text = if links == 0 {
                        message
                    } else {
                        format!("{message} · {links} wikilinks updated")
                    };
                    {
                        let mut index = this.index.borrow_mut();
                        for relinked in &done {
                            index.note_saved(relinked.note.clone(), &relinked.text);
                        }
                    }
                    this.apply_effects(
                        vec![
                            PickerEffect::Relinked(done),
                            PickerEffect::Toast { text, undo },
                        ],
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Applies what a picker did: toasts, and the open note following a
    /// rename, move or link rewrite.
    pub(super) fn apply_effects(&mut self, effects: Vec<PickerEffect>, cx: &mut Context<Self>) {
        for effect in effects {
            match effect {
                PickerEffect::Toast { text, undo } => self.show_toast(text, undo, cx),
                PickerEffect::Moved { from, to } => {
                    // Open notes inside it follow, in every pane.
                    let follow = |note: &mut NotePath| {
                        if let Ok(rest) = note.as_path().strip_prefix(&from) {
                            let moved = if rest.as_os_str().is_empty() {
                                to.clone()
                            } else {
                                to.join(rest)
                            };
                            if let Ok(moved) = NotePath::new(moved) {
                                *note = moved;
                            }
                        }
                    };
                    if let Source::Note { note, .. } = &mut self.source {
                        follow(note);
                    }
                    for doc in &mut self.parked {
                        if let Source::Note { note, .. } = &mut doc.source {
                            follow(note);
                        }
                    }
                    for slot in &mut self.panes {
                        if let DocKey::Note(note) = &mut slot.note {
                            follow(note);
                        }
                    }
                }
                PickerEffect::Relinked(done) => {
                    // The watcher skips mNote's own writes: index them here.
                    {
                        let mut index = self.index.borrow_mut();
                        for relinked in &done {
                            index.note_saved(relinked.note.clone(), &relinked.text);
                        }
                    }
                    for relinked in &done {
                        if self.source.key() == Some(&relinked.note) {
                            self.buffer.reload(&relinked.text);
                            let cursor = clamp_normal(self.buffer.text(), self.vim.cursor());
                            self.vim.set_cursor(cursor);
                            self.sync_lines();
                        } else {
                            self.with_parked(&relinked.note, |this| {
                                this.buffer.reload(&relinked.text);
                            });
                        }
                    }
                }
            }
        }
    }

    /// Shows a toast for about four seconds.
    pub(super) fn show_toast(&mut self, text: String, undo: bool, cx: &mut Context<Self>) {
        self.toast = Some((text.into(), undo));
        self.toast_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = this.update(cx, |this, cx| {
                this.toast = None;
                cx.notify();
            });
        }));
    }

    pub(super) fn picker_action(&mut self, action: PickerAction, cx: &mut Context<Self>) {
        match action {
            PickerAction::OpenNote(note) => self.switch_to(note, cx),
            PickerAction::CreateNote(note) => {
                let Source::Note { vault, .. } = &self.source else {
                    self.reply = Some((
                        "A scratch note has no vault to create notes in".into(),
                        true,
                    ));
                    return;
                };
                if let Err(err) = vault.write(&note, "") {
                    self.reply = Some((format!("Couldn't create {note}: {err}").into(), true));
                    return;
                }
                self.index.borrow_mut().note_saved(note.clone(), "");
                self.switch_to(note, cx);
            }
            PickerAction::GoToLine(line) => self.go_to_line(line),
            PickerAction::JumpTo { path, line } => self.go_to(path, line, cx),
            PickerAction::SendToList(entries) => {
                self.list = entries;
                self.list_go(0, cx);
            }
            PickerAction::ListGo(n) => self.list_go(n, cx),
            PickerAction::Command(n) => self.run_command(n, cx),
            PickerAction::EditFolder(folder) => self.open_folder_editor(folder, cx),
            PickerAction::Callback(callback) => (callback.0)(self, cx),
            PickerAction::RunEx(command) => {
                let revision = self.buffer.revision();
                let result = self.vim.execute(&command, &mut self.buffer);
                self.carry_undo(cx);
                match result {
                    Ok(outcome) => {
                        if let Some(message) = outcome.message {
                            self.reply = Some((message.into(), false));
                        }
                        if let Some(app) = outcome.app {
                            self.run_app_command(app, cx);
                        }
                    }
                    Err(err) => self.reply = Some((err.to_string().into(), true)),
                }
                self.sync_lines();
                if self.buffer.revision() != revision {
                    self.schedule_autosave(cx);
                }
            }
        }
    }
}
