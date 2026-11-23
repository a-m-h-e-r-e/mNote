//! Saving and reloading: autosave, saving notes and config.toml, theme
//! files saved elsewhere, and notes changed on disk by other apps.

use super::*;

impl Editor {
    pub(super) fn schedule_autosave(&mut self, cx: &mut Context<Self>) {
        // With `autosave = false` notes save on `:w` / `⌘S` and when left.
        if matches!(self.source, Source::Scratch) || !self.config.autosave {
            return;
        }
        self.autosave.edited(Instant::now());
        // Replacing the task cancels the previous timer, so typing keeps
        // pushing the save back.
        self.autosave_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Autosave::DEFAULT_DELAY)
                .await;
            let _ = this.update(cx, |this, cx| this.save_now(cx));
        }));
    }

    pub(super) fn save_now(&mut self, cx: &mut Context<Self>) {
        self.autosave_task = None;
        if let Source::Config(path) = &self.source {
            let path = path.clone();
            return self.save_config(&path, cx);
        }
        let Source::Note { note, .. } = &self.source else {
            return;
        };
        if !self.buffer.is_dirty() {
            return;
        }
        // Subscribers may still change the note before it's written.
        let note = note.clone();
        self.dispatch_now(events::EditorEvent::NoteSavePre { note }, cx);
        let Source::Note { vault, note, .. } = &self.source else {
            return;
        };
        match self.buffer.save(vault, note) {
            Ok(()) => {
                self.autosave.saved();
                self.notice = None;
                self.index
                    .borrow_mut()
                    .note_saved(note.clone(), &self.buffer.to_string());
                let note = note.clone();
                self.emit(events::EditorEvent::NoteSaved { note }, cx);
            }
            Err(err) => {
                self.notice = Some(format!("Couldn't save. {err}").into());
            }
        }
        cx.notify();
    }

    /// Saves config.toml and applies it; a mistake is shown, with its line,
    /// and the settings in use stay.
    pub(super) fn save_config(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        if !self.buffer.is_dirty() {
            return;
        }
        let text = self.buffer.to_string();
        if let Err(err) = mnote_core::config::write_file(path, &text) {
            self.notice = Some(format!("Couldn't save. {err}").into());
            return cx.notify();
        }
        self.buffer.mark_saved();
        self.autosave.saved();
        match Config::parse(&text) {
            Ok(config) => {
                #[cfg(target_os = "macos")]
                let failed = crate::summon::set_hotkeys(&config.summon, &config.capture);
                #[cfg(not(target_os = "macos"))]
                let failed: Vec<String> = Vec::new();
                self.notice = failed.first().map(|err| err.to_string().into());
                self.apply_config(config, cx);
                if self.notice.is_none() {
                    self.reply = Some(("Settings applied".into(), false));
                }
            }
            Err(err) => self.notice = Some(err.to_string().into()),
        }
        cx.notify();
    }

    /// Reloads the theme in use when its file (`themes/<name>.toml`) is
    /// saved, here or in another editor.
    pub(super) fn watch_themes(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = Config::path().and_then(|p| p.parent().map(|d| d.to_path_buf())) else {
            return;
        };
        if !dir.is_dir() {
            return;
        }
        let (tx, rx) = async_channel::unbounded::<std::path::PathBuf>();
        let Ok(watcher) = mnote_core::watch_dir(&dir, move |path| {
            let _ = tx.try_send(path);
        }) else {
            return;
        };
        self._config_watcher = Some(watcher);
        self._tasks.push(cx.spawn(async move |this, cx| {
            while let Ok(path) = rx.recv().await {
                let applied = this.update(cx, |this, cx| {
                    match crate::theme::Theme::reload(&path, cx) {
                        Some(Ok(())) => {
                            this.notice = None;
                            cx.refresh_windows();
                        }
                        Some(Err(err)) => {
                            this.notice = Some(err.into());
                            cx.notify();
                        }
                        None => {}
                    }
                });
                if applied.is_err() {
                    break;
                }
            }
        }));
    }

    pub(super) fn on_vault_event(&mut self, event: VaultEvent, cx: &mut Context<Self>) {
        let Some(vault) = &self.vault else {
            return;
        };
        self.index.borrow_mut().apply(vault, &event);
        let path = match &event {
            VaultEvent::Changed(path) | VaultEvent::Removed(path) => path.clone(),
        };
        if self.source.key() == Some(&path) {
            self.reload_from_disk(&event, true);
        } else if self.parked_notes().contains(&path) {
            self.with_parked(&path, |this| this.reload_from_disk(&event, false));
        }
        self.emit(events::EditorEvent::VaultChanged, cx);
    }

    /// The focused note changed or went away on disk. `live`: it's the
    /// focused pane's own (not a parked note swapped in).
    pub(super) fn reload_from_disk(&mut self, event: &VaultEvent, live: bool) {
        let Source::Note { vault, note } = &self.source else {
            return;
        };
        match event {
            VaultEvent::Changed(_) => {
                let Ok(disk) = vault.read(note) else { return };
                match self.buffer.reload(&disk) {
                    Reload::Reloaded => {
                        if live {
                            let cursor = clamp_normal(self.buffer.text(), self.vim.cursor());
                            self.vim.set_cursor(cursor);
                            self.sync_lines();
                        }
                        self.notice = None;
                    }
                    Reload::KeptLocalEdits => {
                        self.notice =
                            Some("This note changed on disk. Your unsaved edits are kept.".into());
                    }
                    Reload::Unchanged => {}
                }
            }
            VaultEvent::Removed(_) => {
                self.notice =
                    Some("This note was deleted or moved on disk. Saving recreates it.".into());
            }
        }
    }
}
