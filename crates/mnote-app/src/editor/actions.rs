//! What commands do: the actions the command registry (`commands`) runs,
//! and the app commands that come back from the Vim command line.

use super::*;

impl Editor {
    /// Runs a command from the palette or the finder's `>` scope.
    /// Runs the registry's command `ix` (from the palette or a key).
    pub(super) fn run_command(&mut self, ix: usize, cx: &mut Context<Self>) {
        let all = crate::commands::all();
        if let Some(command) = all.get(ix) {
            self.run(&command.run, &Invocation::default(), cx);
        }
    }

    /// Commands from the command line that belong to the app.
    /// An app command from the command line (or `gd`): the registry's
    /// command with that ex name runs, with its argument and `!`.
    pub(super) fn run_app_command(&mut self, command: AppCommand, cx: &mut Context<Self>) {
        let all = crate::commands::all();
        let Some(found) = crate::commands::by_ex(&command.name).and_then(|i| all.get(i)) else {
            self.reply = Some((
                format!("E492: Not an editor command: {}", command.name).into(),
                true,
            ));
            return;
        };
        let call = Invocation {
            args: command.arg().map(str::to_owned),
            bang: command.bang,
        };
        self.run(&found.run, &call, cx);
    }

    /// Runs a command's [`Run`].
    pub(super) fn run(&mut self, run: &Run, call: &Invocation, cx: &mut Context<Self>) {
        match run {
            Run::Ex(ex) => self.picker_action(PickerAction::RunEx(ex.clone()), cx),
            Run::Finder(query) => self.open_finder(query, cx),
            Run::Keys(keys) => {
                // A leading space is the leader binding: replay it with the
                // real leader.
                let keys = match keys.strip_prefix(' ') {
                    Some(rest) => format!("{}{rest}", crate::commands::leader()),
                    None => keys.clone(),
                };
                for key in mnote_vim::parse_keys(&keys) {
                    self.press(key, cx);
                }
            }
            Run::Action(action) => action(self, call, cx),
        }
    }

    /// `:w [path]` (and ⌘S): saves now. An untitled note opens the save
    /// prompt; with a path it's named (and a scratch note too); a note
    /// with a name writes a copy there.
    pub(super) fn write_command(&mut self, call: &Invocation, cx: &mut Context<Self>) {
        let path = call
            .args
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty());
        if let Some(path) = path {
            match self.source {
                Source::Untitled(_) | Source::Scratch => self.save_as(Some(path), call.bang, cx),
                Source::Note { .. } => self.write_copy(path, call.bang),
                Source::Config(_) => {
                    self.reply = Some(("config.toml stays where it is".into(), true));
                }
            }
        } else if matches!(self.source, Source::Untitled(_)) {
            self.save_prompt(cx);
        } else if let Err(err) = self.write(cx) {
            self.reply = Some((err.into(), true));
        } else if let Source::Note { note, .. } = &self.source {
            self.reply = Some((format!("\"{}\" written", note.file_name()).into(), false));
        }
    }

    /// `:view [styled|source|plain]`, and `space m` (no argument cycles).
    pub(super) fn view_command(&mut self, name: Option<&str>, cx: &mut Context<Self>) {
        match name.map(str::trim) {
            None | Some("") => self.set_view(self.view.next(), cx),
            Some(name) => match styled::View::from_name(name) {
                Some(view) => self.set_view(view, cx),
                None => {
                    self.reply = Some((
                        format!("E475: Invalid argument: {name} (styled, source or plain)").into(),
                        true,
                    ))
                }
            },
        }
    }

    /// `:split [note]` / `:vsplit [note]`: a new pane, on that note or this one.
    pub(super) fn split_command(
        &mut self,
        vertical: bool,
        path: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let axis = if vertical { Axis::Row } else { Axis::Column };
        let target = match path.map(str::trim).filter(|p| !p.is_empty()) {
            None => None,
            Some(name) => {
                let found = {
                    let index = self.index.borrow();
                    index.resolve(name).map(|i| index.notes()[i].path.clone())
                };
                match found {
                    Some(note) => Some(note),
                    None => {
                        self.reply = Some((format!("E32: No note named {name}").into(), true));
                        return;
                    }
                }
            }
        };
        self.split_pane(axis, cx);
        if let Some(note) = target {
            self.switch_to(note, cx);
        }
    }

    /// `:close`.
    pub(super) fn close_command(&mut self, cx: &mut Context<Self>) {
        if !self.close_pane(cx) {
            self.reply = Some(("E444: Cannot close last window".into(), true));
        }
    }

    /// `:copen`: the results list in a picker.
    pub(super) fn open_results_list(&mut self, cx: &mut Context<Self>) {
        if self.list.is_empty() {
            self.reply = Some((
                "E42: No results list (⌃q in a / search makes one)".into(),
                true,
            ));
        } else {
            let list = ListPicker::new(self.list.clone());
            self.picker = Some(Picker::new(Box::new(list)));
            self.restart_blink(cx);
        }
    }

    pub(super) fn not_yet(&mut self, command: &str) {
        self.reply = Some((format!("{command} isn't available yet").into(), true));
    }

    /// `:w`: saves now. A scratch note has nowhere to go.
    pub(super) fn write(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        if matches!(self.source, Source::Scratch | Source::Untitled(_)) {
            return Err("E32: No file name".into());
        }
        self.save_now(cx);
        if self.buffer.is_dirty() {
            return Err(self
                .notice
                .as_ref()
                .map_or_else(|| "Couldn't save".into(), ToString::to_string));
        }
        Ok(())
    }

    /// `:q`, `:wq`, `:x`. Notes autosave, so quitting saves them first; only
    /// unsaved scratch text needs `!` to throw away.
    pub(super) fn quit(&mut self, write: bool, force: bool, cx: &mut Context<Self>) {
        // `:wq` on an untitled note names it first.
        if write && matches!(self.source, Source::Untitled(_)) {
            return self.save_prompt(cx);
        }
        let unsaved = matches!(self.source, Source::Scratch | Source::Untitled(_))
            && self.buffer.is_dirty()
            && self.buffer.len_chars() > 0;
        // With tabs or panes left, `:q` closes the active tab, and the pane
        // with its last tab (notes autosave).
        if self.closes_not_quits() {
            if write && let Err(err) = self.write(cx) {
                self.reply = Some((err.into(), true));
                return;
            }
            if unsaved && !force && !self.shown_elsewhere(&self.source.doc_key()) {
                self.reply = Some((
                    "E37: No write since last change (add ! to override)".into(),
                    true,
                ));
                return;
            }
            self.close_active_tab(cx);
            return;
        }
        let result = match self.source {
            Source::Scratch if write => Err("E32: No file name".to_owned()),
            Source::Scratch | Source::Untitled(_) if unsaved && !force => {
                Err("E37: No write since last change (add ! to override)".to_owned())
            }
            Source::Scratch | Source::Untitled(_) => Ok(()),
            Source::Note { .. } | Source::Config(_) => self.write(cx),
        };
        match result {
            Ok(()) => cx.quit(),
            Err(_) if force => cx.quit(),
            Err(err) => self.reply = Some((err.into(), true)),
        }
    }

    /// `:qa`, `:wqa`, `:xa`: closes every pane and quits. Unsaved scratch or
    /// untitled text anywhere needs `!`.
    pub(super) fn quit_all(&mut self, write: bool, force: bool, cx: &mut Context<Self>) {
        self.save_now(cx);
        if !force && let Some(name) = self.unsaved_anywhere() {
            let err = if write {
                format!("E141: No file name for {name}")
            } else {
                format!("E37: No write since last change in {name} (add ! to override)")
            };
            self.reply = Some((err.into(), true));
            return;
        }
        cx.quit();
    }

    /// Switches the view and says which it is.
    pub(super) fn set_view(&mut self, view: styled::View, cx: &mut Context<Self>) {
        self.view = view;
        self.reply = Some((format!("{} view", view.name()).into(), false));
        cx.notify();
    }

    /// `space x`: checks or unchecks the task on the cursor line.
    pub(super) fn toggle_task(&mut self, cx: &mut Context<Self>) {
        let rope = self.buffer.text();
        let line = line_of(rope, self.vim.cursor());
        let start = line_start(rope, line);
        let text: String = rope.slice(start..start + line_len(rope, line)).to_string();
        let LineKind::Task { checked, .. } = line_kind(None, rope, line) else {
            self.reply = Some(("Not a task line. Tasks look like - [ ] item".into(), true));
            return;
        };
        let Some(open) = text.find(['[']) else { return };
        let at = start + text[..open].chars().count() + 1;
        self.vim.replace(
            &mut self.buffer,
            at..at + 1,
            if checked { " " } else { "x" },
        );
        self.sync_lines();
        self.schedule_autosave(cx);
        cx.notify();
    }

    /// `gd`: opens the note the `[[link]]` under the cursor names (or the
    /// first link after it on the line), at its `#heading` if it has one. A
    /// link to no note creates it.
    pub(super) fn follow_link(&mut self, cx: &mut Context<Self>) {
        let rope = self.buffer.text();
        let cursor = self.vim.cursor();
        let line = line_of(rope, cursor);
        let text = rope.line(line).to_string();
        let at = char_to_byte(&text, cursor - line_start(rope, line));
        let links = mnote_core::links::wikilinks(&text);
        let link = links
            .iter()
            .find(|l| l.range.contains(&at))
            .or_else(|| links.iter().find(|l| l.range.start >= at));
        let Some(link) = link.cloned() else {
            self.reply = Some(("E447: No [[link]] under the cursor".into(), true));
            return;
        };
        let found = {
            let index = self.index.borrow();
            index.resolve(&link.target).map(|i| {
                let entry = &index.notes()[i];
                let heading = link.heading.as_ref().and_then(|h| {
                    entry
                        .headings
                        .iter()
                        .find(|x| x.text.eq_ignore_ascii_case(h))
                        .map(|x| x.line)
                });
                (entry.path.clone(), heading.unwrap_or(0))
            })
        };
        if let Some((path, line)) = found {
            return self.go_to(path, line, cx);
        }
        if !self.index_ready {
            self.reply = Some((
                "Still reading the vault; try gd again in a moment".into(),
                true,
            ));
            return;
        }
        let Some(note) = link.new_note() else {
            self.reply = Some((
                format!("E447: Can't make a note from [[{}]]", link.target).into(),
                true,
            ));
            return;
        };
        // A file the index missed is opened, never written over.
        if matches!(&self.source, Source::Note { vault, .. } if vault.path_of(&note).exists()) {
            return self.switch_to(note, cx);
        }
        let created = format!("{note} created");
        self.picker_action(PickerAction::CreateNote(note.clone()), cx);
        if matches!(&self.source, Source::Note { note: open, .. } if *open == note) {
            self.reply = Some((created.into(), false));
        }
    }

    /// Opens `path` (if it isn't open) with the cursor at `line`.
    pub(super) fn go_to(&mut self, path: NotePath, line: usize, cx: &mut Context<Self>) {
        let here = matches!(&self.source, Source::Note { note, .. } if *note == path);
        if !here {
            self.switch_to(path.clone(), cx);
            if !matches!(&self.source, Source::Note { note, .. } if *note == path) {
                return;
            }
        }
        self.go_to_line(line);
    }

    pub(super) fn go_to_line(&mut self, line: usize) {
        let rope = self.buffer.text();
        let line = line.min(line_count(rope).saturating_sub(1));
        self.vim.set_cursor(line_start(rope, line));
        self.lines.scroll_to_reveal_item(line);
    }

    /// `:cnext` (`by` 1), `:cprevious` (-1), or a jump to entry `to`.
    pub(super) fn list_go(&mut self, to: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.list.get(to).cloned() else {
            return;
        };
        self.list_pos = to;
        self.go_to(entry.path, entry.line, cx);
        self.reply = Some((
            format!("({} of {}) {}", to + 1, self.list.len(), entry.text).into(),
            false,
        ));
    }

    pub(super) fn list_step(&mut self, by: isize, cx: &mut Context<Self>) {
        if self.list.is_empty() {
            self.reply = Some((
                "E42: No results list (⌃q in a / search makes one)".into(),
                true,
            ));
            return;
        }
        let to = self.list_pos as isize + by;
        if to < 0 || to as usize >= self.list.len() {
            self.reply = Some(("E553: No more items".into(), true));
            return;
        }
        self.list_go(to as usize, cx);
    }

    /// `:daily`: today's note (from `daily_note` in config.toml), made if it
    /// doesn't exist yet.
    pub(super) fn open_daily(&mut self, cx: &mut Context<Self>) {
        let Some(vault) = &self.vault else {
            self.reply = Some(("A scratch note has no vault for a daily note".into(), true));
            return;
        };
        let note = self.daily_note();
        if vault.path_of(&note).exists() {
            self.switch_to(note, cx);
        } else {
            self.picker_action(PickerAction::CreateNote(note), cx);
        }
    }

    /// Today's note, as config.toml names it.
    pub fn daily_note(&self) -> NotePath {
        let (year, month, day) = crate::today();
        self.config.daily_note(year, month, day)
    }
}
