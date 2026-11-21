//! The editor: one note in the window, with the top bar and status line.
//!
//! Keys go to the Vim engine, which edits the [`Buffer`]. Only visible lines
//! are laid out (gpui's `list`), and after an edit only the changed lines are
//! measured again. Saving is automatic: about a second after the last edit,
//! and when the window loses focus. Changes made to the note by other apps are
//! picked up live.

mod keys;
mod line;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, ClipboardItem, Context, FocusHandle, Font, IntoElement, KeyDownEvent,
    ListAlignment, ListState, ParentElement, Render, SharedString, Styled, Subscription, Task,
    Window, div, font, list, prelude::*, px,
};
use mnote_core::{Autosave, Buffer, NotePath, Reload, Vault, VaultEvent, VaultWatcher};
use mnote_vim::text::{clamp_normal, line_count, line_len, line_of, line_start};
use mnote_vim::{Clipboard, Effect, Mode, Vim};

use crate::theme::{ActiveTheme, StyledExt, Theme, family, size, space, text};
use crate::ui::{StatusLine, TopBar};
use line::{Caret, LineText};

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
            _tasks: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    // ---- input ------------------------------------------------------------

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = keys::vim_key(&event.keystroke) else {
            return;
        };
        if matches!(self.vim.pending_register(), Some('+' | '*')) {
            self.clipboard.borrow_mut().read =
                cx.read_from_clipboard().and_then(|item| item.text());
        }
        let mode_before = self.vim.mode();
        let revision_before = self.buffer.revision();

        let effect = self.vim.handle_key(key, &mut self.buffer);

        if let Some(text) = self.clipboard.borrow_mut().write.take() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        if let Effect::Unhandled(_) = effect {
            // Leave it to the app (the space leader, ⌘ shortcuts, `:` text).
            return;
        }
        cx.stop_propagation();

        self.sync_lines();
        let cursor_line = line_of(self.buffer.text(), self.vim.cursor());
        self.lines.scroll_to_reveal_item(cursor_line);
        if self.buffer.revision() != revision_before {
            self.notice = None;
            self.schedule_autosave(cx);
        }
        if self.vim.mode() != mode_before || self.vim.mode() == Mode::Insert {
            self.restart_blink(cx);
        }
        cx.notify();
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

    /// The bar caret blinks at about 1 Hz in insert mode and stays solid
    /// while you type. Other modes use a steady block, with no timer running.
    fn restart_blink(&mut self, cx: &mut Context<Self>) {
        self.caret_on = true;
        if self.vim.mode() != Mode::Insert {
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
        let mut line = LineText::new(
            content.clone(),
            prose_font(body),
            body.size(),
            body.line_height(),
            Theme::color(p.ink),
        );

        if let Some(chars) = self.selection_on_line(ix, start, len) {
            let to_byte = |c: usize| char_to_byte(&content, c);
            let end = if chars.end > len {
                content.len() + 1
            } else {
                to_byte(chars.end)
            };
            line = line.selection(to_byte(chars.start)..end, Theme::color(p.select));
        }
        if current && (self.caret_on || self.vim.mode() != Mode::Insert) {
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

fn char_to_byte(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map_or(s.len(), |(b, _)| b)
}

impl Render for Editor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.theme().palette;
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
            .size_full()
            .bg(Theme::color(p.paper))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .child(
                TopBar::new(self.path_segments())
                    .dirty(self.buffer.is_dirty())
                    .leading_inset(px(TRAFFIC_LIGHT_INSET)),
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
            .child(status)
    }
}
