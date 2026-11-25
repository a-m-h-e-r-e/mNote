//! Input: every key and mouse event, and where it goes. `on_key` takes a
//! keystroke; `press` routes a key through the layers in order:
//!
//! 1. an overlay (the folder editor, a picker) takes every key;
//! 2. the keymap: a key binding being typed (`space f`, `⌃w v`);
//! 3. Vim, with the system clipboard read just before a paste that may
//!    use it, and Esc in NORMAL hiding the window.
//!
//! ⌘ shortcuts go through the keymap in `on_key`, before any of that, and
//! text typed in INSERT goes through the system's input (see `ime`).

use super::*;

impl Editor {
    pub(super) fn on_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.latency.key_pressed();
        let stroke = &event.keystroke;
        if self.first_run_key(stroke, cx) {
            cx.stop_propagation();
            return;
        }
        // While inserting, typed characters go through the system's input
        // (see `ime`), so accents and composed input work: let them pass.
        if self.takes_text()
            && self.keyseq.is_empty()
            && !stroke.modifiers.control
            && stroke.key_char.is_some()
            && matches!(
                keys::vim_key(stroke),
                Some(mnote_vim::Key::Char(_) | mnote_vim::Key::Alt(_))
            )
        {
            return;
        }
        // ⌘ bindings (⌘K, ⌘S, ⌘C …), from the keymap. Over a picker or
        // another overlay only those made for it run.
        if stroke.modifiers.platform
            && !stroke.modifiers.control
            && !stroke.modifiers.alt
            && !stroke.modifiers.shift
            && let Some(c) = single_key(&stroke.key)
            && let Some(id) = crate::keymap::current().cmd(c).map(str::to_owned)
            && let Some(ix) = crate::commands::by_id(&id)
        {
            let overlay =
                self.picker.is_some() || self.folder_edit.is_some() || self.first_run.is_some();
            if !overlay || crate::commands::all()[ix].over_overlays {
                cx.stop_propagation();
                self.run_command(ix, cx);
                return cx.notify();
            }
        }
        let Some(key) = keys::vim_key(stroke) else {
            return;
        };
        if self.press(key, cx) {
            cx.stop_propagation();
        }
    }

    /// One key, as typed: to the open picker, the leader, or Vim. Returns
    /// whether it was used (unused keys go on to the app).
    pub(super) fn press(&mut self, key: mnote_vim::Key, cx: &mut Context<Self>) -> bool {
        self.overlay_key(key, cx) || self.binding_key(key, cx) || self.vim_key(key, cx)
    }

    /// Layer 1: an open overlay takes every key.
    fn overlay_key(&mut self, key: mnote_vim::Key, cx: &mut Context<Self>) -> bool {
        if self.folder_edit.is_some() {
            self.folder_key(key, cx);
            return true;
        }
        if self.picker.is_some() {
            self.picker_key(key, cx);
            return true;
        }
        false
    }

    /// Layer 3: Vim, and what follows a key it handled (scrolling to the
    /// cursor, autosave, app commands, the caret). Returns `false` for a
    /// key it doesn't use, which goes on to the app.
    fn vim_key(&mut self, key: mnote_vim::Key, cx: &mut Context<Self>) -> bool {
        // The system clipboard is read just before a paste that may use it:
        // `"+p` and `⌃r +`, and plain `p` / `P` (and `⌃r "`) when y, d and p
        // share it.
        let plain_paste = matches!(key, mnote_vim::Key::Char('p' | 'P'))
            && matches!(
                self.vim.mode(),
                Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::VisualBlock
            )
            && matches!(self.vim.pending_register(), None | Some('"'))
            && !self.vim.awaits_register()
            && self.config.system_clipboard;
        let clipboard_paste = plain_paste
            || matches!(self.vim.pending_register(), Some('+' | '*'))
            || (self.vim.awaits_register() && matches!(key, mnote_vim::Key::Char('+' | '*')))
            // `⌃r "` in INSERT, when the unnamed register is the clipboard.
            || (self.vim.awaits_register()
                && self.vim.mode() == Mode::Insert
                && key == mnote_vim::Key::Char('"')
                && self.config.system_clipboard);
        if clipboard_paste {
            self.clipboard.borrow_mut().read =
                cx.read_from_clipboard().and_then(|item| item.text());
        }
        if key == mnote_vim::Key::Esc && self.esc_hides() {
            self.save_now(cx);
            cx.hide();
            return true;
        }
        let mode_before = self.shown_mode();
        let revision_before = self.buffer.revision();
        self.reply = None;

        let (top, bottom) = self.lines_on_screen();
        self.vim.set_view(top, bottom);
        let effect = self.vim.handle_key(key, &mut self.buffer);

        if let Some(text) = self.clipboard.borrow_mut().write.take() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        if let Effect::Unhandled(_) = effect {
            // The app's (⌘ shortcuts).
            return false;
        }

        self.sync_lines();
        // While a pattern is typed, the view follows its next match.
        let reveal = self
            .vim
            .preview()
            .and_then(|p| p.current.as_ref())
            .map_or(self.vim.cursor(), |m| m.start);
        match self.vim.take_scroll() {
            Some(scroll) => self.scroll_lines_to(scroll),
            None => self
                .lines
                .scroll_to_reveal_item(line_of(self.buffer.text(), reveal)),
        }
        if self.buffer.revision() != revision_before {
            self.notice = None;
            self.schedule_autosave(cx);
        }
        if let Some(command) = self.vim.take_app_command() {
            self.run_app_command(command, cx);
        }
        if self.vim.mode() != Mode::Insert {
            self.marked = None;
        }
        if self.shown_mode() != mode_before || has_bar_caret(self.shown_mode()) {
            self.restart_blink(cx);
        }
        self.wait_for_mapping(cx);
        cx.notify();
        true
    }

    /// Keys that may yet become a mapping wait up to a second (Vim's
    /// `timeoutlen`), then run as typed.
    pub(super) fn wait_for_mapping(&mut self, cx: &mut Context<Self>) {
        if !self.vim.map_pending() {
            self.map_task = None;
            return;
        }
        self.map_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MAPPING_TIMEOUT).await;
            let _ = this.update(cx, |this, cx| {
                let revision = this.buffer.revision();
                this.vim.flush_mapping(&mut this.buffer);
                this.sync_lines();
                if this.buffer.revision() != revision {
                    this.schedule_autosave(cx);
                }
                this.map_task = None;
                cx.notify();
            });
        }));
    }

    /// Whether `esc` hides mNote: in NORMAL with nothing pending, no
    /// command-line window and no multi-line message to close first.
    pub(super) fn esc_hides(&self) -> bool {
        self.vim.mode() == Mode::Normal
            && !self.vim.is_pending()
            && self.vim.cmd_window().is_none()
            && !self.vim.message().is_some_and(|m| m.text.contains('\n'))
    }

    /// The mode whose caret is showing: the picker field's, or the
    /// command-line window's, while either is open.
    pub(super) fn shown_mode(&self) -> Mode {
        if let Some(folder) = &self.folder_edit {
            return folder.mode();
        }
        if let Some(picker) = &self.picker {
            return picker.mode();
        }
        self.vim
            .cmd_window()
            .map_or(self.vim.mode(), |w| w.vim.mode())
    }

    /// The key after `space` in NORMAL mode: `f` finds notes, `/` searches
    /// text, `@` lists this note's headings, `#` lists tags, `b` lists
    /// backlinks, `e` explores, `x` toggles a task, `m` cycles the view.
    /// Layer 2, the keymap: a key that continues a binding being typed, or
    /// starts one in NORMAL with nothing half typed (so the leader comes
    /// before Vim's own `,` or space, and `⌃w` before Vim). Returns whether
    /// the keymap took it.
    pub(super) fn binding_key(&mut self, key: mnote_vim::Key, cx: &mut Context<Self>) -> bool {
        use crate::keymap::Lookup;
        let starting = self.keyseq.is_empty();
        if starting
            && !(self.vim.mode() == Mode::Normal
                && !self.vim.is_pending()
                && self.vim.cmd_window().is_none())
        {
            return false;
        }
        self.keyseq_task = None;
        self.keyseq.push(key);
        let keymap = crate::keymap::current();
        match keymap.lookup(&self.keyseq, crate::commands::leader()) {
            Lookup::Prefix => {
                if starting {
                    self.reply = None;
                }
            }
            Lookup::Exact(id) => {
                self.keyseq.clear();
                self.run_command_id(&id, cx);
            }
            Lookup::ExactOrLonger(id) => {
                self.keyseq_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(MAPPING_TIMEOUT).await;
                    let _ = this.update(cx, |this, cx| {
                        this.keyseq.clear();
                        this.keyseq_task = None;
                        this.run_command_id(&id, cx);
                        cx.notify();
                    });
                }));
            }
            Lookup::None => {
                // A key that starts nothing goes on to Vim; one that ends a
                // started binding wrongly is dropped, as Vim drops `⌃w z`.
                let typed = std::mem::take(&mut self.keyseq);
                if typed.len() == 1 {
                    return false;
                }
            }
        }
        cx.notify();
        true
    }

    /// Runs the registry's command with this id (from a key binding).
    pub(super) fn run_command_id(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(ix) = crate::commands::by_id(id) {
            self.run_command(ix, cx);
        }
    }

    pub(super) fn folder_key(&mut self, key: mnote_vim::Key, cx: &mut Context<Self>) {
        let Some(editor) = self.folder_edit.as_mut() else {
            return;
        };
        match editor.handle_key(key) {
            FolderOutcome::Open => {}
            FolderOutcome::Close => self.folder_edit = None,
            FolderOutcome::CountLinks(changes) => self.count_folder_links(&changes, cx),
            FolderOutcome::Apply(changes) => {
                let folder = editor.folder.clone();
                self.folder_edit = None;
                self.apply_folder_changes(folder, &changes, cx);
            }
        }
        self.restart_blink(cx);
        cx.notify();
    }

    pub(super) fn picker_key(&mut self, key: mnote_vim::Key, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let mode_before = picker.mode();
        let outcome = picker.handle_key(key);
        // Effects and background work outlive the picker if it closes.
        let effects = picker.take_effects();
        let job = picker.take_job();
        match outcome {
            PickerOutcome::Open => {
                if picker.mode() != mode_before || picker.mode() == Mode::Insert {
                    self.restart_blink(cx);
                }
            }
            PickerOutcome::Close => {
                self.picker = None;
                self.restart_blink(cx);
            }
            PickerOutcome::Act(action) => {
                self.picker = None;
                self.picker_action(action, cx);
                self.restart_blink(cx);
            }
            PickerOutcome::ActInSplit { action, vertical } => {
                self.picker = None;
                self.split_pane(if vertical { Axis::Row } else { Axis::Column }, cx);
                self.picker_action(action, cx);
                self.restart_blink(cx);
            }
        }
        self.apply_effects(effects, cx);
        if let Some(job) = job {
            self.spawn_job(job, cx);
        }
        cx.notify();
    }

    /// A picker row pressed: selected, and opened on a double click.
    pub(super) fn picker_click(&mut self, ix: usize, clicks: usize, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        picker.select(ix);
        if clicks >= 2 && picker.selected() == ix {
            self.picker_key(mnote_vim::Key::Enter, cx);
        }
        cx.notify();
    }

    /// The mouse over line `ix` of `pane`, at character `ch` of the line:
    /// a click places the cursor (⌘ follows a link there), shift extends
    /// the selection, a double click selects a word, a triple click the
    /// line, and a drag selects from where it started.
    pub(super) fn mouse_on_line(
        &mut self,
        pane: PaneId,
        ix: usize,
        ch: usize,
        hit: line::MouseHit,
        cx: &mut Context<Self>,
    ) {
        use line::MouseHit;
        if self.picker.is_some() || self.folder_edit.is_some() || self.first_run.is_some() {
            return;
        }
        if pane != self.focused {
            if matches!(hit, MouseHit::Drag) {
                return;
            }
            self.focus_pane(pane, cx);
        }
        let rope = self.buffer.text();
        if ix >= line_count(rope) {
            return;
        }
        let start = line_start(rope, ix);
        let len = line_len(rope, ix);
        let pos = start + ch.min(len);
        let visual = self.vim.mode().is_visual();
        match hit {
            MouseHit::Down { command: true, .. } => {
                self.vim.click(pos, &mut self.buffer);
                self.follow_link(cx);
            }
            MouseHit::Down {
                clicks: 1,
                shift: true,
                ..
            } => {
                let anchor = if visual {
                    self.vim.anchor()
                } else {
                    self.vim.cursor()
                };
                self.vim.select(anchor, pos, false, &mut self.buffer);
            }
            MouseHit::Down { clicks: 1, .. } => {
                self.vim.click(pos, &mut self.buffer);
                self.drag_anchor = Some(pos);
            }
            MouseHit::Down { clicks: 2, .. } => {
                // The word under the pointer (or just that character).
                let chars: Vec<char> = rope.slice(start..start + len).chars().collect();
                let word = |c: char| c.is_alphanumeric() || c == '_';
                let at = ch.min(chars.len().saturating_sub(1));
                let (mut a, mut b) = (at, at);
                if chars.get(at).is_some_and(|&c| word(c)) {
                    while a > 0 && word(chars[a - 1]) {
                        a -= 1;
                    }
                    while b + 1 < chars.len() && word(chars[b + 1]) {
                        b += 1;
                    }
                }
                self.vim
                    .select(start + a, start + b, false, &mut self.buffer);
                self.drag_anchor = None;
            }
            MouseHit::Down { .. } => {
                self.vim.select(start, start, true, &mut self.buffer);
                self.drag_anchor = None;
            }
            MouseHit::Drag => {
                let Some(anchor) = self.drag_anchor else {
                    return;
                };
                if pos == anchor && !visual {
                    return;
                }
                self.vim.select(anchor, pos, false, &mut self.buffer);
            }
        }
        self.restart_blink(cx);
        cx.notify();
    }

    /// `⌘C` `⌘X` `⌘V` `⌘A`: copy, cut and paste with the system clipboard,
    /// and select all. Returns whether the key was one of them.
    pub(super) fn clipboard_shortcut(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        if !matches!(key, "c" | "x" | "v" | "a") {
            return false;
        }
        if self.folder_edit.is_some() || self.first_run.is_some() {
            return true;
        }
        if let Some(picker) = &mut self.picker {
            if key == "v"
                && let Some(text) = cx.read_from_clipboard().and_then(|item| item.text())
            {
                picker.paste(&text);
                cx.notify();
            }
            return true;
        }
        let mode = self.vim.mode();
        let visual = mode.is_visual();
        let keys = match (key, mode) {
            ("c", _) if visual => "\"+y",
            ("c", Mode::Normal) => "\"+yy",
            ("x", _) if visual => "\"+d",
            ("x", Mode::Normal) => "\"+dd",
            ("v", Mode::Insert | Mode::Command) => {
                let text = cx.read_from_clipboard().and_then(|item| item.text());
                if let Some(text) = text {
                    let revision = self.buffer.revision();
                    self.vim.paste_text(&text, &mut self.buffer);
                    self.sync_lines();
                    if self.buffer.revision() != revision {
                        self.schedule_autosave(cx);
                    }
                    cx.notify();
                }
                return true;
            }
            ("v", _) if visual => "\"+p",
            ("v", Mode::Normal) => "\"+P",
            ("a", Mode::Normal) => "ggVG",
            ("a", _) if visual => "<esc>ggVG",
            _ => return true,
        };
        for key in mnote_vim::parse_keys(keys) {
            self.press(key, cx);
        }
        true
    }
}

impl Editor {
    /// The first line on screen and the last one fully on screen, for `H`
    /// `M` `L` and Vim's scrolling. Lines below the last one drawn count as
    /// a page of lines.
    fn lines_on_screen(&self) -> (usize, usize) {
        let scroll = self.lines.logical_scroll_top();
        let top = scroll.item_ix + usize::from(scroll.offset_in_item > px(0.));
        let view = self.lines.viewport_bounds();
        let count = self.lines.item_count();
        let mut bottom = top;
        // A page is a few dozen lines: this walks just those.
        for ix in top..count {
            match self.lines.bounds_for_item(ix) {
                Some(b) if b.bottom() <= view.bottom() + px(0.5) => bottom = ix,
                _ => break,
            }
        }
        (top.min(count.saturating_sub(1)), bottom)
    }

    /// Scrolls the note as `zt`, `zz`, `zb`, `⌃e` or `⌃y` asked.
    fn scroll_lines_to(&mut self, scroll: mnote_vim::Scroll) {
        use mnote_vim::Scroll;
        let line = match scroll {
            Scroll::Top(l) | Scroll::Center(l) | Scroll::Bottom(l) => l,
        };
        self.lines.scroll_to(gpui::ListOffset {
            item_ix: line,
            offset_in_item: px(0.),
        });
        let room = self.lines.viewport_bounds().size.height;
        let height = self
            .lines
            .bounds_for_item(line)
            .map_or(px(0.), |b| b.size.height);
        let up = match scroll {
            Scroll::Top(_) => px(0.),
            Scroll::Center(_) => (room - height) / 2.,
            Scroll::Bottom(_) => room - height,
        };
        self.lines.scroll_by(-up.max(px(0.)));
    }
}
