//! The editor: one note in the window, with the top bar and status line.
//!
//! Keys go to the Vim engine, which edits the [`Buffer`]. Only visible lines
//! are laid out (gpui's `list`), and after an edit only the changed lines are
//! measured again. Saving is automatic: about a second after the last edit,
//! and when the window loses focus. Changes made to the note by other apps are
//! picked up live.
//!
//! `:`, `/` and `?` turn the status line into the command line. While it's
//! typed, matches and `:s` replacements are painted into the note as a
//! preview; nothing changes until `enter`.

mod keys;
pub(crate) mod line;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, ClipboardItem, Context, FocusHandle, Font, IntoElement, KeyDownEvent,
    ListAlignment, ListState, ParentElement, Render, SharedString, Styled, Subscription, Task,
    Window, div, font, list, prelude::*, px,
};
use mnote_core::{Autosave, Buffer, NoteIndex, NotePath, Reload, Vault, VaultEvent, VaultWatcher};
use mnote_vim::ex::AppCommand;
use mnote_vim::ex::cmdline::{Completer, line_matches};
use mnote_vim::ex::cmdwin::CmdWindow;
use mnote_vim::text::{clamp_normal, line_count, line_len, line_of, line_start};
use mnote_vim::{Clipboard, Effect, Mode, Vim};

use crate::picker::finder::{CurrentNote, Finder};
use crate::picker::list::ListPicker;
use crate::picker::{ListEntry, Picker, PickerAction, PickerOutcome};
use crate::theme::{ActiveTheme, Palette, StyledExt, Theme, family, size, space, text};
use crate::ui::{CommandLine, CompletionPopup, MessagePanel, StatusLine, TopBar};
use line::{Caret, Highlight, LineText};

/// Width left for the macOS traffic-light buttons in the top bar.
const TRAFFIC_LIGHT_INSET: f32 = 76.;
/// Half a blink cycle of the insert-mode bar caret (about 1 Hz).
const BLINK: Duration = Duration::from_millis(530);

/// Where the open note lives. A scratch note isn't saved.
enum Source {
    Scratch,
    Note {
        vault: Vault,
        note: NotePath,
        _watcher: Option<VaultWatcher>,
    },
}

/// Bridges Vim's synchronous clipboard to gpui's: the editor fills `read`
/// just before a `"+p`, and flushes `write` after each key.
#[derive(Default)]
struct ClipboardState {
    read: Option<String>,
    write: Option<String>,
}

struct SharedClipboard(Rc<RefCell<ClipboardState>>);

impl Clipboard for SharedClipboard {
    fn read(&mut self) -> Option<String> {
        self.0.borrow().read.clone()
    }

    fn write(&mut self, text: &str) {
        self.0.borrow_mut().write = Some(text.to_owned());
    }
}

/// Note paths for `⇥` on the command line, read from the vault when asked.
struct VaultPaths(Vault);

impl Completer for VaultPaths {
    fn paths(&mut self) -> Vec<String> {
        self.0
            .notes()
            .map(|notes| notes.iter().map(ToString::to_string).collect())
            .unwrap_or_default()
    }
}

pub struct Editor {
    buffer: Buffer,
    vim: Vim,
    source: Source,
    clipboard: Rc<RefCell<ClipboardState>>,
    lines: ListState,
    focus: FocusHandle,
    autosave: Autosave,
    autosave_task: Option<Task<()>>,
    caret_on: bool,
    blink_task: Option<Task<()>>,
    /// A message for the status line: a save error or a reload notice.
    notice: Option<SharedString>,
    /// The app's answer to a command line (`"inbox.md" written`), shown in
    /// the command line until the next key. The flag marks an error.
    reply: Option<(SharedString, bool)>,
    /// Every note in the vault, for the pickers. Built in the background
    /// when the vault opens, then kept current by watcher events and saves.
    index: Rc<RefCell<NoteIndex>>,
    /// The open picker, drawn over the note; it gets every key.
    picker: Option<Picker>,
    /// `space` was pressed in NORMAL mode: the next key is a leader command.
    leader: bool,
    /// The results list (`⌃q` in content search) and where `:cnext` is.
    list: Vec<ListEntry>,
    list_pos: usize,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Editor {
    /// An editor for a scratch note that isn't saved anywhere.
    pub fn scratch(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::build(Buffer::new(""), Source::Scratch, window, cx)
    }

    /// An editor for `note` in `vault`. A note that doesn't exist yet starts
    /// empty and is created on the first save.
    pub fn open(vault: Vault, note: NotePath, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (buffer, notice) = match Buffer::open(&vault, &note) {
            Ok(buffer) => (buffer, None),
            Err(_) if !vault.path_of(&note).exists() => (Buffer::new(""), None),
            Err(err) => (
                Buffer::new(""),
                Some(SharedString::from(format!("Couldn't open {note}: {err}"))),
            ),
        };

        // Changes by other apps arrive on the watcher's thread; hop to ours.
        let (tx, rx) = async_channel::unbounded::<VaultEvent>();
        let watcher = vault
            .watch(move |event| {
                let _ = tx.try_send(event);
            })
            .ok();
        let paths = VaultPaths(vault.clone());
        let mut editor = Self::build(
            buffer,
            Source::Note {
                vault,
                note,
                _watcher: watcher,
            },
            window,
            cx,
        );
        editor.notice = notice;
        editor.vim.set_completer(Box::new(paths));
        editor.build_index(cx);
        editor._tasks.push(cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv().await {
                if this
                    .update(cx, |this, cx| this.on_vault_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
        editor
    }

    fn build(buffer: Buffer, source: Source, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let clipboard = Rc::new(RefCell::new(ClipboardState::default()));
        let mut vim = Vim::new();
        vim.set_clipboard(Box::new(SharedClipboard(clipboard.clone())));
        let focus = cx.focus_handle();
        window.focus(&focus);

        let subscriptions = vec![
            cx.observe_window_appearance(window, |_, window, cx| {
                if Theme::sync_with(window, cx) {
                    cx.notify();
                }
            }),
            // Leaving the window saves at once.
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.save_now(cx);
                }
            }),
        ];

        Editor {
            lines: ListState::new(buffer.text().len_lines(), ListAlignment::Top, px(400.)),
            buffer,
            vim,
            source,
            clipboard,
            focus,
            autosave: Autosave::default(),
            autosave_task: None,
            caret_on: true,
            blink_task: None,
            notice: None,
            reply: None,
            index: Rc::default(),
            picker: None,
            leader: false,
            list: Vec::new(),
            list_pos: 0,
            _tasks: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    // ---- input ------------------------------------------------------------

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = keys::vim_key(&event.keystroke) else {
            return;
        };
        if self.picker.is_some() {
            cx.stop_propagation();
            return self.picker_key(key, cx);
        }
        if std::mem::take(&mut self.leader) {
            cx.stop_propagation();
            return self.leader_key(key, cx);
        }
        let clipboard_paste = matches!(self.vim.pending_register(), Some('+' | '*'))
            || (self.vim.awaits_register() && matches!(key, mnote_vim::Key::Char('+' | '*')));
        if clipboard_paste {
            self.clipboard.borrow_mut().read =
                cx.read_from_clipboard().and_then(|item| item.text());
        }
        let mode_before = self.shown_mode();
        let revision_before = self.buffer.revision();
        self.reply = None;

        let effect = self.vim.handle_key(key, &mut self.buffer);

        if let Some(text) = self.clipboard.borrow_mut().write.take() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        if let Effect::Unhandled(key) = effect {
            if key == mnote_vim::Key::Char(' ') && self.vim.mode() == Mode::Normal {
                self.leader = true;
                cx.stop_propagation();
            }
            // Anything else is the app's (⌘ shortcuts).
            return;
        }
        cx.stop_propagation();

        self.sync_lines();
        // While a pattern is typed, the view follows its next match.
        let reveal = self
            .vim
            .preview()
            .and_then(|p| p.current.as_ref())
            .map_or(self.vim.cursor(), |m| m.start);
        self.lines
            .scroll_to_reveal_item(line_of(self.buffer.text(), reveal));
        if self.buffer.revision() != revision_before {
            self.notice = None;
            self.schedule_autosave(cx);
        }
        if let Some(command) = self.vim.take_app_command() {
            self.run_app_command(command, cx);
        }
        if self.shown_mode() != mode_before || has_bar_caret(self.shown_mode()) {
            self.restart_blink(cx);
        }
        cx.notify();
    }

    /// The mode whose caret is showing: the picker field's, or the
    /// command-line window's, while either is open.
    fn shown_mode(&self) -> Mode {
        if let Some(picker) = &self.picker {
            return picker.mode();
        }
        self.vim
            .cmd_window()
            .map_or(self.vim.mode(), |w| w.vim.mode())
    }

    /// The key after `space` in NORMAL mode.
    fn leader_key(&mut self, key: mnote_vim::Key, cx: &mut Context<Self>) {
        if key == mnote_vim::Key::Char('f') {
            let current = CurrentNote {
                path: match &self.source {
                    Source::Note { note, .. } => Some(note.clone()),
                    Source::Scratch => None,
                },
                text: self.buffer.to_string(),
            };
            let vault = match &self.source {
                Source::Note { vault, .. } => Some(vault.clone()),
                Source::Scratch => None,
            };
            let finder = Finder::new(self.index.clone(), current, vault);
            self.picker = Some(Picker::new(Box::new(finder)));
            self.restart_blink(cx);
            cx.notify();
        }
    }

    fn picker_key(&mut self, key: mnote_vim::Key, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let mode_before = picker.mode();
        match picker.handle_key(key) {
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
        }
        self.run_picker_job(cx);
        cx.notify();
    }

    /// Runs the picker's background work (a content search) off the UI
    /// thread and hands the result back, if the picker is still open.
    fn run_picker_job(&mut self, cx: &mut Context<Self>) {
        let Some(job) = self.picker.as_mut().and_then(Picker::take_job) else {
            return;
        };
        let task = cx.background_executor().spawn(async move { job() });
        cx.spawn(async move |this, cx| {
            if let Some(result) = task.await {
                let _ = this.update(cx, |this, cx| {
                    if let Some(picker) = this.picker.as_mut() {
                        picker.job_done(result);
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// Opens `path` (if it isn't open) with the cursor at `line`.
    fn go_to(&mut self, path: NotePath, line: usize, cx: &mut Context<Self>) {
        let here = matches!(&self.source, Source::Note { note, .. } if *note == path);
        if !here {
            self.switch_to(path.clone(), cx);
            if !matches!(&self.source, Source::Note { note, .. } if *note == path) {
                return;
            }
        }
        self.go_to_line(line);
    }

    fn go_to_line(&mut self, line: usize) {
        let rope = self.buffer.text();
        let line = line.min(line_count(rope).saturating_sub(1));
        self.vim.set_cursor(line_start(rope, line));
        self.lines.scroll_to_reveal_item(line);
    }

    /// `:cnext` (`by` 1), `:cprevious` (-1), or a jump to entry `to`.
    fn list_go(&mut self, to: usize, cx: &mut Context<Self>) {
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

    fn list_step(&mut self, by: isize, cx: &mut Context<Self>) {
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

    fn picker_action(&mut self, action: PickerAction, cx: &mut Context<Self>) {
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
            PickerAction::RunEx(command) => {
                let revision = self.buffer.revision();
                match self.vim.execute(&command, &mut self.buffer) {
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

    /// Opens another note of the same vault in this editor, saving the
    /// current one first. Registers, history and options carry over.
    fn switch_to(&mut self, target: NotePath, cx: &mut Context<Self>) {
        self.save_now(cx);
        let Source::Note { vault, note, .. } = &mut self.source else {
            return;
        };
        if *note == target {
            return;
        }
        let buffer = match Buffer::open(vault, &target) {
            Ok(buffer) => buffer,
            Err(err) => {
                self.reply = Some((format!("Couldn't open {target}: {err}").into(), true));
                return;
            }
        };
        *note = target;
        self.buffer = buffer;
        self.vim.set_cursor(0);
        self.autosave_task = None;
        self.notice = None;
        self.lines.reset(self.buffer.text().len_lines());
    }

    /// Reads the whole vault into the index on a background thread.
    fn build_index(&mut self, cx: &mut Context<Self>) {
        let Source::Note { vault, .. } = &self.source else {
            return;
        };
        let vault = vault.clone();
        let task = cx
            .background_executor()
            .spawn(async move { NoteIndex::build(&vault) });
        self._tasks.push(cx.spawn(async move |this, cx| {
            if let Ok(index) = task.await {
                let _ = this.update(cx, |this, cx| {
                    *this.index.borrow_mut() = index;
                    cx.notify();
                });
            }
        }));
    }

    /// Commands from the command line that belong to the app.
    fn run_app_command(&mut self, command: AppCommand, cx: &mut Context<Self>) {
        match command {
            AppCommand::Write { path: None, .. } => {
                if let Err(err) = self.write(cx) {
                    self.reply = Some((err.into(), true));
                } else if let Source::Note { note, .. } = &self.source {
                    self.reply = Some((format!("\"{}\" written", note.file_name()).into(), false));
                }
            }
            AppCommand::Write { path: Some(_), .. } => {
                self.reply = Some(("Writing to another file isn't supported yet".into(), true));
            }
            AppCommand::Quit { force } => self.quit(false, force, cx),
            AppCommand::WriteQuit { force } => self.quit(true, force, cx),
            AppCommand::Edit { .. } => self.not_yet(":edit"),
            AppCommand::View(_) => self.not_yet(":view"),
            AppCommand::Explore(_) => self.not_yet(":explore"),
            AppCommand::Find(_) => self.not_yet(":find"),
            AppCommand::Daily => self.not_yet(":daily"),
            AppCommand::ListNext => self.list_step(1, cx),
            AppCommand::ListPrevious => self.list_step(-1, cx),
            AppCommand::ListOpen => {
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
        }
    }

    fn not_yet(&mut self, command: &str) {
        self.reply = Some((format!("{command} isn't available yet").into(), true));
    }

    /// `:w`: saves now. A scratch note has nowhere to go.
    fn write(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        if matches!(self.source, Source::Scratch) {
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
    fn quit(&mut self, write: bool, force: bool, cx: &mut Context<Self>) {
        let result = match self.source {
            Source::Scratch if write => Err("E32: No file name".to_owned()),
            Source::Scratch if self.buffer.is_dirty() && !force => {
                Err("E37: No write since last change (add ! to override)".to_owned())
            }
            Source::Scratch => Ok(()),
            Source::Note { .. } => self.write(cx),
        };
        match result {
            Ok(()) => cx.quit(),
            Err(_) if force => cx.quit(),
            Err(err) => self.reply = Some((err.into(), true)),
        }
    }

    /// Re-measures only the lines the last edits touched, keeping the scroll
    /// position.
    fn sync_lines(&mut self) {
        let Some(changed) = self.buffer.take_changed_lines() else {
            return;
        };
        let old = self.lines.item_count();
        let new = self.buffer.text().len_lines();
        let top = changed.unchanged_top.min(old).min(new);
        let bottom = changed.unchanged_bottom.min(old - top).min(new - top);
        self.lines.splice(top..old - bottom, new - top - bottom);
    }

    /// The bar caret (insert mode, and the command line) blinks at about
    /// 1 Hz and stays solid while you type. Other modes use a steady block,
    /// with no timer running.
    fn restart_blink(&mut self, cx: &mut Context<Self>) {
        self.caret_on = true;
        if !has_bar_caret(self.shown_mode()) {
            self.blink_task = None;
            return;
        }
        self.blink_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(BLINK).await;
                let alive = this.update(cx, |this, cx| {
                    this.caret_on = !this.caret_on;
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    // ---- saving and reloading ----------------------------------------------

    fn schedule_autosave(&mut self, cx: &mut Context<Self>) {
        if matches!(self.source, Source::Scratch) {
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

    fn save_now(&mut self, cx: &mut Context<Self>) {
        self.autosave_task = None;
        let Source::Note { vault, note, .. } = &self.source else {
            return;
        };
        if !self.buffer.is_dirty() {
            return;
        }
        match self.buffer.save(vault, note) {
            Ok(()) => {
                self.autosave.saved();
                self.notice = None;
                self.index
                    .borrow_mut()
                    .note_saved(note.clone(), &self.buffer.to_string());
            }
            Err(err) => {
                self.notice = Some(format!("Couldn't save. {err}").into());
            }
        }
        cx.notify();
    }

    fn on_vault_event(&mut self, event: VaultEvent, cx: &mut Context<Self>) {
        let Source::Note { vault, note, .. } = &self.source else {
            return;
        };
        self.index.borrow_mut().apply(vault, &event);
        match event {
            VaultEvent::Changed(path) if &path == note => {
                let Ok(disk) = vault.read(note) else { return };
                match self.buffer.reload(&disk) {
                    Reload::Reloaded => {
                        let cursor = clamp_normal(self.buffer.text(), self.vim.cursor());
                        self.vim.set_cursor(cursor);
                        self.sync_lines();
                        self.notice = None;
                    }
                    Reload::KeptLocalEdits => {
                        self.notice =
                            Some("This note changed on disk. Your unsaved edits are kept.".into());
                    }
                    Reload::Unchanged => {}
                }
                cx.notify();
            }
            VaultEvent::Removed(path) if &path == note => {
                self.notice =
                    Some("This note was deleted or moved on disk. Saving recreates it.".into());
                cx.notify();
            }
            _ => {}
        }
    }

    // ---- rendering ----------------------------------------------------------

    fn render_line(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let rope = self.buffer.text();
        // A final newline is followed by an empty rope line Vim doesn't show.
        if ix >= line_count(rope) {
            return div().into_any_element();
        }
        let p = cx.theme().palette;
        let start = line_start(rope, ix);
        let len = line_len(rope, ix);
        let content: SharedString = rope.slice(start..start + len).to_string().into();

        let cursor = self.vim.cursor();
        let cursor_line = line_of(rope, cursor);
        let current = ix == cursor_line;
        let number = if current {
            ix + 1
        } else {
            ix.abs_diff(cursor_line)
        };

        let body = text::NOTE_BODY;
        let (shown, highlights) = self.decorate(ix, start, len, &content, p);
        let mut line = LineText::new(
            shown,
            prose_font(body),
            body.size(),
            body.line_height(),
            Theme::color(p.ink),
        )
        .highlights(highlights);

        if let Some(chars) = self.selection_on_line(ix, start, len) {
            let to_byte = |c: usize| char_to_byte(&content, c);
            let end = if chars.end > len {
                content.len() + 1
            } else {
                to_byte(chars.end)
            };
            line = line.selection(to_byte(chars.start)..end, Theme::color(p.select));
        }
        let mode = self.vim.mode();
        // In command mode the caret is in the command line.
        if current && mode != Mode::Command && (self.caret_on || mode != Mode::Insert) {
            line = line.caret(Some(Caret {
                byte: char_to_byte(&content, cursor - start),
                shape: self.vim.mode().caret(),
                color: Theme::color(p.cursor),
                on_color: Theme::color(p.on_cursor),
            }));
        }

        let gutter = text::STATUS;
        div()
            .flex()
            .w_full()
            .when(current, |row| row.bg(Theme::color(p.well)))
            .when(self.in_visual_range(ix), |row| {
                row.bg(Theme::color(p.select))
            })
            .child(
                div()
                    .flex_none()
                    .w(px(size::GUTTER_W))
                    .pr(px(space::S8))
                    .text_right()
                    .text_style(gutter)
                    .line_height(body.line_height())
                    .text_color(Theme::color(if current { p.ink } else { p.ink_3 }))
                    .child(SharedString::from(number.to_string())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .max_w(px(size::MEASURE))
                    .child(line),
            )
            .into_any_element()
    }

    /// Whether line `ix` is in the `'<,'>` range of the command being typed,
    /// so the selection stays tinted while the command line is open.
    fn in_visual_range(&self, ix: usize) -> bool {
        let Some(line) = self.vim.command_line() else {
            return false;
        };
        if !line.text.trim_start().starts_with("'<,'>") {
            return false;
        }
        let rope = self.buffer.text();
        match (self.vim.mark('<'), self.vim.mark('>')) {
            (Some(a), Some(b)) => {
                let (a, b) = (line_of(rope, a.min(b)), line_of(rope, a.max(b)));
                (a..=b).contains(&ix)
            }
            _ => false,
        }
    }

    /// The text to show for line `ix` and its highlights: `:s` replacements
    /// while one is typed, else matches of the pattern being typed, else
    /// matches of the last search until `:noh`.
    fn decorate(
        &self,
        ix: usize,
        start: usize,
        len: usize,
        content: &SharedString,
        p: &Palette,
    ) -> (SharedString, Vec<Highlight>) {
        let preview = self.vim.preview();
        if let Some(preview) = preview.filter(|pv| !pv.substitutions.is_empty()) {
            let subs = preview.substitutions_in(start..start + len);
            if subs.is_empty() {
                return (content.clone(), Vec::new());
            }
            let mut shown = String::with_capacity(content.len() + 16);
            let mut highlights = Vec::with_capacity(subs.len());
            let mut byte = 0;
            for (range, replacement) in subs {
                let from = char_to_byte(content, range.start - start);
                let to = char_to_byte(content, range.end.min(start + len) - start);
                shown.push_str(&content[byte.min(from)..from]);
                let at = shown.len();
                // A replacement that splits the line shows where it breaks.
                shown.push_str(&replacement.replace('\n', "⏎"));
                highlights.push(Highlight {
                    bytes: at..shown.len(),
                    background: Theme::color(p.insert_wash),
                    color: Theme::color(p.insert),
                    outline: (preview.current.as_ref() == Some(range))
                        .then(|| Theme::color(p.insert)),
                });
                byte = to.max(byte);
            }
            shown.push_str(&content[byte..]);
            return (shown.into(), highlights);
        }

        let (pattern, current) = match preview {
            Some(pv) if pv.covers(ix) => (pv.pattern.as_ref(), pv.current.clone()),
            Some(pv) if pv.pattern.is_some() => (None, None),
            _ => (self.vim.highlight_pattern(), None),
        };
        let Some(pattern) = pattern else {
            return (content.clone(), Vec::new());
        };
        let highlights = line_matches(pattern, content, start)
            .filter(|m| !m.is_empty())
            .map(|m| {
                let next = current.as_ref() == Some(&m);
                Highlight {
                    bytes: char_to_byte(content, m.start - start)
                        ..char_to_byte(content, m.end - start),
                    background: Theme::color(if next { p.cursor } else { p.match_ }),
                    color: Theme::color(if next { p.on_cursor } else { p.ink }),
                    outline: None,
                }
            })
            .collect();
        (content.clone(), highlights)
    }

    /// The selected characters on line `ix` (relative to the line start). An
    /// end past `len` means the line's newline is selected too.
    fn selection_on_line(
        &self,
        ix: usize,
        start: usize,
        len: usize,
    ) -> Option<std::ops::Range<usize>> {
        let rope = self.buffer.text();
        let (anchor, cursor) = (self.vim.anchor(), self.vim.cursor());
        let (a, b) = (anchor.min(cursor), anchor.max(cursor));
        match self.vim.mode() {
            Mode::VisualLine => (line_of(rope, a)..=line_of(rope, b))
                .contains(&ix)
                .then_some(0..len + 1),
            Mode::Visual => {
                let end = start + len;
                if b < start || a > end {
                    return None;
                }
                let from = a.max(start) - start;
                // The selection includes the character under the cursor; past
                // the line's last character that's the newline.
                let to = (b + 1).min(end + 1) - start;
                Some(from..to)
            }
            _ => None,
        }
    }

    fn path_segments(&self) -> Vec<SharedString> {
        match &self.source {
            Source::Scratch => vec![SharedString::new_static("scratch")],
            Source::Note { note, .. } => note.segments().map(|s| s.to_owned().into()).collect(),
        }
    }
}

fn prose_font(style: crate::theme::TextStyle) -> Font {
    Font {
        weight: style.font_weight(),
        ..font(family::PROSE)
    }
}

/// Modes whose caret is the blinking bar.
fn has_bar_caret(mode: Mode) -> bool {
    matches!(mode, Mode::Insert | Mode::Command)
}

fn char_to_byte(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map_or(s.len(), |(b, _)| b)
}

impl Editor {
    /// What replaces the status line: the line being typed with its live
    /// count, or the last command and its result. Empty means the status
    /// line shows.
    fn command_area(&self) -> Vec<AnyElement> {
        if let Some(line) = self.vim.command_line() {
            return vec![self.typed_line(line).into_any_element()];
        }
        let (text, error, line) = match (self.vim.message(), &self.reply) {
            (Some(m), _) => (m.text.clone(), m.error, m.line.clone()),
            (None, Some((text, error))) => (text.to_string(), *error, None),
            (None, None) => return Vec::new(),
        };
        if text.contains('\n') {
            let mut area = vec![MessagePanel::new(&text).into_any_element()];
            if let Some(line) = line {
                area.push(CommandLine::new(line.kind, line.text).into_any_element());
            }
            return area;
        }
        let bar = match line {
            Some(line) => CommandLine::new(line.kind, line.text)
                .info(text)
                .error(error),
            None => CommandLine::message(text, error),
        };
        vec![bar.into_any_element()]
    }
}

impl Editor {
    fn typed_line(&self, line: &mnote_vim::ex::cmdline::CommandLine) -> CommandLine {
        let caret = self.caret_on.then_some(line.cursor);
        let mut bar = CommandLine::new(line.kind, line.text.clone()).caret(caret);
        if let Some(info) = self.vim.preview().and_then(|p| p.info.clone()) {
            bar = bar.info(info);
        }
        bar
    }

    /// The `⇥` popup, if open: placed by the caller just above the line.
    fn completion_popup(&self) -> Option<CompletionPopup> {
        let completion = self.vim.command_line()?.completion.as_ref()?;
        let (first, rows) = completion.visible();
        let rows = rows
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let selected = completion.selected == Some(first + i);
                (c.text.clone().into(), c.detail.clone().into(), selected)
            })
            .collect();
        Some(CompletionPopup::new(rows))
    }

    /// The command-line window (`q:`): history lines in mono, with the
    /// window's own caret and status line, above the bottom bar.
    fn cmd_window_area(&self, win: &CmdWindow, p: &Palette) -> Vec<AnyElement> {
        let rope = win.buffer.text();
        let (first, last) = win.visible();
        let cursor = win.vim.cursor();
        let cursor_line = line_of(rope, cursor);
        let code = text::CODE;
        let mono = Font {
            weight: code.font_weight(),
            ..font(family::MONO)
        };
        let rows = (first..=last).map(|ix| {
            let start = line_start(rope, ix);
            let len = line_len(rope, ix);
            let content: SharedString = rope.slice(start..start + len).to_string().into();
            let current = ix == cursor_line;
            let mode = win.vim.mode();
            let mut line = LineText::new(
                content.clone(),
                mono.clone(),
                code.size(),
                code.line_height(),
                Theme::color(p.ink),
            );
            if current && mode != Mode::Command && (self.caret_on || mode != Mode::Insert) {
                line = line.caret(Some(Caret {
                    byte: char_to_byte(&content, cursor - start),
                    shape: mode.caret(),
                    color: Theme::color(p.cursor),
                    on_color: Theme::color(p.on_cursor),
                }));
            }
            div()
                .flex()
                .px(px(space::S3))
                .when(current, |row| row.bg(Theme::color(p.well)))
                .child(
                    div()
                        .flex_none()
                        .w(px(space::S6))
                        .text_style(text::STATUS)
                        .line_height(code.line_height())
                        .text_color(Theme::color(p.ink_3))
                        .child(SharedString::new_static(match win.kind.prefix() {
                            ':' => ":",
                            '/' => "/",
                            _ => "?",
                        })),
                )
                .child(div().flex_1().min_w_0().child(line))
        });
        let panel = div()
            .flex()
            .flex_col()
            .flex_none()
            .py(px(space::S1))
            .bg(Theme::color(p.paper))
            .border_t_1()
            .border_color(Theme::color(p.line))
            .children(rows)
            .into_any_element();
        // The window's own status line, or its command line while `:` is typed
        // in it.
        let bar = match win.vim.command_line() {
            Some(line) => self.typed_line(line).into_any_element(),
            None => StatusLine::new(win.vim.mode())
                .file("[Command Line]")
                .item("⏎ run · esc close")
                .into_any_element(),
        };
        vec![panel, bar]
    }
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.theme().palette;
        let width = window.viewport_size().width / px(1.);
        let caret_on = self.caret_on;
        let picker = self
            .picker
            .as_mut()
            .map(|picker| picker.render(width, caret_on, p));
        let rope = self.buffer.text();
        let cursor = self.vim.cursor();
        let line = line_of(rope, cursor);
        let column = cursor - line_start(rope, line);
        let file = match &self.source {
            Source::Scratch => SharedString::new_static("scratch (not saved)"),
            Source::Note { note, .. } => note.file_name().to_owned().into(),
        };
        let view = cx.entity();

        let mut status = StatusLine::new(self.vim.mode()).file(file);
        if let Some(notice) = &self.notice {
            status = status.item(notice.clone());
        }
        let status = status
            .right(format!("{} lines", line_count(rope)))
            .right(format!("{}:{}", line + 1, column + 1));

        div()
            .flex()
            .flex_col()
            .relative()
            .size_full()
            .bg(Theme::color(p.paper))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .child(
                TopBar::new(self.path_segments())
                    .dirty(self.buffer.is_dirty())
                    .leading_inset(px(TRAFFIC_LIGHT_INSET))
                    .when(self.vim.command_line().is_some(), |bar| {
                        bar.hint("⇥ complete · ↑ history · ⌃f edit")
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .justify_center()
                    .pt(px(space::S8))
                    .child(
                        list(self.lines.clone(), move |ix, _window, cx: &mut App| {
                            view.update(cx, |this, cx| this.render_line(ix, cx))
                        })
                        .size_full()
                        .max_w(px(size::GUTTER_W + size::MEASURE + space::S8)),
                    ),
            )
            // The command line takes the status line's place while it's
            // typed, and while it shows the result.
            .map(|editor| {
                if let Some(win) = self.vim.cmd_window() {
                    return editor.children(self.cmd_window_area(win, p));
                }
                let command = self.command_area();
                if command.is_empty() {
                    editor.child(status)
                } else {
                    editor.children(command)
                }
            })
            .children(self.completion_popup().map(|popup| {
                div()
                    .absolute()
                    .left(px(space::S3))
                    .bottom(px(size::BAR_H + space::S1))
                    .child(popup)
            }))
            .children(picker)
    }
}
