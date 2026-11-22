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

pub(crate) mod keys;
pub(crate) mod line;
mod styled;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, ClipboardItem, Context, FocusHandle, Font, IntoElement, KeyDownEvent,
    ListAlignment, ListState, ParentElement, Render, SharedString, Styled, Subscription, Task,
    Window, div, font, list, prelude::*, px,
};
use mnote_core::code::CodeStyle;
use mnote_core::files::FileOp;
use mnote_core::folder_edit::{Change, FolderEntry};
use mnote_core::links::{LinkMove, count_relinks, relink_vault};
use mnote_core::styled::{LineKind, StyledLine, line_kind};
use mnote_core::{Autosave, Buffer, NoteIndex, NotePath, Reload, Vault, VaultEvent, VaultWatcher};
use mnote_vim::ex::AppCommand;
use mnote_vim::ex::cmdline::{Completer, line_matches};
use mnote_vim::ex::cmdwin::CmdWindow;
use mnote_vim::text::{clamp_normal, line_count, line_len, line_of, line_start};
use mnote_vim::{Clipboard, Effect, Mode, Vim};

use crate::commands::{COMMANDS, Run};
use crate::folder_edit::{FolderEditor, FolderOutcome};
use crate::picker::backlinks::BacklinksPicker;
use crate::picker::explorer::{Explorer, FileHistory, FileUndo};
use crate::picker::finder::{CurrentNote, Finder};
use crate::picker::list::ListPicker;
use crate::picker::palette::CommandPalette;
use crate::picker::{Job, JobResult, ListEntry, Picker, PickerAction, PickerEffect, PickerOutcome};
use crate::theme::{ActiveTheme, Palette, StyledExt, Theme, family, radius, size, space, text};
use crate::ui::{CommandLine, CompletionPopup, MessagePanel, StatusLine, Toast, TopBar};
use line::{Caret, Highlight, LineText};

/// A table's column widths and alignments, shared by its rows.
type TableLayout = Rc<(Vec<usize>, Vec<mnote_core::styled::Align>)>;

/// A code block's colours, line by line: byte ranges and their style.
type CodeLines = Vec<Vec<(std::ops::Range<usize>, CodeStyle)>>;

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
        /// Boxed: the watcher is large, and a scratch note has none.
        _watcher: Option<Box<VaultWatcher>>,
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
    /// The index has finished its first build: until then a link that
    /// doesn't resolve may still name a note.
    index_ready: bool,
    /// The open picker, drawn over the note; it gets every key.
    picker: Option<Picker>,
    /// `space` was pressed in NORMAL mode: the next key is a leader command.
    leader: bool,
    /// The results list (`⌃q` in content search) and where `:cnext` is.
    list: Vec<ListEntry>,
    list_pos: usize,
    /// The folder editor (`e` in the explorer), over the note.
    folder_edit: Option<FolderEditor>,
    /// File operations from the explorer, for `u` there.
    file_history: FileHistory,
    /// A message near the bottom after a file operation, and whether it
    /// offers `u undo`. It goes after a few seconds.
    toast: Option<(SharedString, bool)>,
    toast_task: Option<Task<()>>,
    /// Styled, source or plain (`space m`).
    view: styled::View,
    /// The Markdown tree for this frame (cheap to clone), for line kinds.
    tree: Option<mnote_core::SyntaxTree>,
    /// Code block colours by the block's first row, for one text revision.
    code_cache: std::collections::HashMap<usize, (u64, Rc<CodeLines>)>,
    /// Where each image path of the open note points, so drawing doesn't
    /// check the disk every frame.
    image_paths: RefCell<std::collections::HashMap<String, Option<std::path::PathBuf>>>,
    /// Table column layouts by the table's first row, for one revision.
    table_cache: std::collections::HashMap<usize, (u64, TableLayout)>,
    /// Wakes the editor when a background parse lands.
    syntax_ready: async_channel::Sender<()>,
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
                _watcher: watcher.map(Box::new),
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

        // A finished background parse redraws, so blocks and code styling
        // catch up with the text.
        let (syntax_ready, ready) = async_channel::unbounded::<()>();
        let mut buffer = buffer;
        {
            let tx = syntax_ready.clone();
            buffer.on_syntax_ready(move || {
                let _ = tx.try_send(());
            });
        }
        let redraw = cx.spawn(async move |this, cx| {
            while ready.recv().await.is_ok() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });

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
            index_ready: false,
            picker: None,
            leader: false,
            list: Vec::new(),
            list_pos: 0,
            folder_edit: None,
            file_history: FileHistory::default(),
            toast: None,
            toast_task: None,
            view: styled::View::default(),
            tree: None,
            code_cache: std::collections::HashMap::new(),
            table_cache: std::collections::HashMap::new(),
            image_paths: RefCell::default(),
            syntax_ready,
            _tasks: vec![redraw],
            _subscriptions: subscriptions,
        }
    }

    // ---- input ------------------------------------------------------------

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        if stroke.modifiers.platform && stroke.key == "k" && self.picker.is_none() {
            cx.stop_propagation();
            return self.open_picker(Picker::new(Box::new(CommandPalette::default())), cx);
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
    fn press(&mut self, key: mnote_vim::Key, cx: &mut Context<Self>) -> bool {
        if self.folder_edit.is_some() {
            self.folder_key(key, cx);
            return true;
        }
        if self.picker.is_some() {
            self.picker_key(key, cx);
            return true;
        }
        if std::mem::take(&mut self.leader) {
            self.leader_key(key, cx);
            return true;
        }
        let clipboard_paste = matches!(self.vim.pending_register(), Some('+' | '*'))
            || (self.vim.awaits_register() && matches!(key, mnote_vim::Key::Char('+' | '*')));
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

        let effect = self.vim.handle_key(key, &mut self.buffer);

        if let Some(text) = self.clipboard.borrow_mut().write.take() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        if let Effect::Unhandled(key) = effect {
            if key == mnote_vim::Key::Char(' ') && self.vim.mode() == Mode::Normal {
                self.leader = true;
                return true;
            }
            // Anything else is the app's (⌘ shortcuts).
            return false;
        }

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
        true
    }

    /// Whether `esc` hides mNote: in NORMAL with nothing pending, no
    /// command-line window and no multi-line message to close first.
    fn esc_hides(&self) -> bool {
        self.vim.mode() == Mode::Normal
            && !self.vim.is_pending()
            && self.vim.cmd_window().is_none()
            && !self.vim.message().is_some_and(|m| m.text.contains('\n'))
    }

    /// The mode whose caret is showing: the picker field's, or the
    /// command-line window's, while either is open.
    fn shown_mode(&self) -> Mode {
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
    fn leader_key(&mut self, key: mnote_vim::Key, cx: &mut Context<Self>) {
        if key == mnote_vim::Key::Char('e') {
            return self.open_explorer(None, cx);
        }
        if key == mnote_vim::Key::Char('x') {
            return self.toggle_task(cx);
        }
        if key == mnote_vim::Key::Char('m') {
            return self.set_view(self.view.next(), cx);
        }
        if key == mnote_vim::Key::Char('b') {
            return self.open_backlinks(cx);
        }
        let query = match key {
            mnote_vim::Key::Char('f') => "",
            mnote_vim::Key::Char('/') => "/",
            mnote_vim::Key::Char('@') => "@",
            mnote_vim::Key::Char('#') => "#",
            _ => return,
        };
        self.open_finder(query, cx);
    }

    /// `space b`: the notes that link to this one.
    fn open_backlinks(&mut self, cx: &mut Context<Self>) {
        let Source::Note { vault, note, .. } = &self.source else {
            self.reply = Some(("A scratch note has no backlinks".into(), true));
            return;
        };
        let picker = BacklinksPicker::new(&self.index.borrow(), note, vault.clone());
        self.open_picker(Picker::new(Box::new(picker)), cx);
    }

    /// Switches the view and says which it is.
    fn set_view(&mut self, view: styled::View, cx: &mut Context<Self>) {
        self.view = view;
        self.reply = Some((format!("{} view", view.name()).into(), false));
        cx.notify();
    }

    /// `space x`: checks or unchecks the task on the cursor line.
    fn toggle_task(&mut self, cx: &mut Context<Self>) {
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

    /// Opens the finder with `query` already typed.
    fn open_finder(&mut self, query: &str, cx: &mut Context<Self>) {
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
        let mut picker = Picker::new(Box::new(Finder::new(self.index.clone(), current, vault)));
        if !query.is_empty() {
            picker.set_query(query);
        }
        self.open_picker(picker, cx);
    }

    /// Opens the explorer on the open note's folder, or on `folder`.
    fn open_explorer(&mut self, folder: Option<String>, cx: &mut Context<Self>) {
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

    fn open_picker(&mut self, picker: Picker, cx: &mut Context<Self>) {
        self.picker = Some(picker);
        self.restart_blink(cx);
        self.run_picker_job(cx);
        cx.notify();
    }

    /// Opens the folder editor on `folder` (vault-relative).
    fn open_folder_editor(&mut self, folder: String, cx: &mut Context<Self>) {
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

    fn folder_key(&mut self, key: mnote_vim::Key, cx: &mut Context<Self>) {
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

    /// The vault-relative path of `rel` in `folder`.
    fn in_folder(folder: &str, rel: &str) -> std::path::PathBuf {
        let rel = rel.trim_end_matches('/');
        if folder.is_empty() {
            rel.into()
        } else {
            std::path::Path::new(folder).join(rel)
        }
    }

    /// Counts, in the background, the wikilinks the reviewed changes would
    /// rewrite.
    fn count_folder_links(&mut self, changes: &[Change], cx: &mut Context<Self>) {
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
    fn apply_folder_changes(&mut self, folder: String, changes: &[Change], cx: &mut Context<Self>) {
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

    /// Runs a command from the palette or the finder's `>` scope.
    fn run_command(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(command) = COMMANDS.get(ix) else {
            return;
        };
        match command.run {
            Run::Ex(ex) => self.picker_action(PickerAction::RunEx(ex.to_owned()), cx),
            Run::Finder(query) => self.open_finder(query, cx),
            Run::Keys(keys) => {
                for key in mnote_vim::parse_keys(keys) {
                    self.press(key, cx);
                }
            }
        }
    }

    fn picker_key(&mut self, key: mnote_vim::Key, cx: &mut Context<Self>) {
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
        }
        self.apply_effects(effects, cx);
        if let Some(job) = job {
            self.spawn_job(job, cx);
        }
        cx.notify();
    }

    /// Runs the picker's background work (a content search) off the UI
    /// thread.
    fn run_picker_job(&mut self, cx: &mut Context<Self>) {
        if let Some(job) = self.picker.as_mut().and_then(Picker::take_job) {
            self.spawn_job(job, cx);
        }
    }

    /// Runs `job` off the UI thread and hands its result to the picker, or,
    /// if the picker closed meanwhile, applies what still matters (rewritten
    /// links).
    fn spawn_job(&mut self, job: Job, cx: &mut Context<Self>) {
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
    fn apply_effects(&mut self, effects: Vec<PickerEffect>, cx: &mut Context<Self>) {
        for effect in effects {
            match effect {
                PickerEffect::Toast { text, undo } => self.show_toast(text, undo, cx),
                PickerEffect::Moved { from, to } => {
                    if let Source::Note { note, .. } = &mut self.source
                        && let Ok(rest) = note.as_path().strip_prefix(&from)
                    {
                        let moved = if rest.as_os_str().is_empty() {
                            to
                        } else {
                            to.join(rest)
                        };
                        if let Ok(moved) = NotePath::new(moved) {
                            *note = moved;
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
                    let Source::Note { note, .. } = &self.source else {
                        continue;
                    };
                    if let Some(relinked) = done.iter().find(|r| r.note == *note) {
                        self.buffer.reload(&relinked.text);
                        let cursor = clamp_normal(self.buffer.text(), self.vim.cursor());
                        self.vim.set_cursor(cursor);
                        self.sync_lines();
                    }
                }
            }
        }
    }

    /// Shows a toast for about four seconds.
    fn show_toast(&mut self, text: String, undo: bool, cx: &mut Context<Self>) {
        self.toast = Some((text.into(), undo));
        self.toast_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = this.update(cx, |this, cx| {
                this.toast = None;
                cx.notify();
            });
        }));
    }

    /// The vault's index, if a vault is open (quick capture needs one).
    pub fn capture_index(&self) -> Option<Rc<RefCell<NoteIndex>>> {
        matches!(self.source, Source::Note { .. }).then(|| self.index.clone())
    }

    /// Appends a quick-capture line to `target` under `## Inbox`, then opens
    /// the note here if `open`. The open note takes the line into its buffer
    /// (one undo step) so unsaved edits are kept; any other note is written
    /// on disk, created if it's new. An empty `entry` only opens.
    pub fn capture(
        &mut self,
        entry: &str,
        target: &NotePath,
        open: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Source::Note { vault, note, .. } = &self.source else {
            return Err("quick capture needs a vault".into());
        };
        let mut line = 0;
        if !entry.trim().is_empty() {
            let title = target.name().to_owned();
            if note == target {
                let text = self.buffer.to_string();
                let insertion = mnote_core::capture::insertion(&text, &title, entry);
                let at = text[..insertion.at].chars().count();
                self.vim.replace(&mut self.buffer, at..at, &insertion.text);
                line = insertion.line;
                self.sync_lines();
                self.save_now(cx);
            } else {
                let text = match vault.read(target) {
                    Ok(text) => text,
                    Err(mnote_core::VaultError::Io { source, .. })
                        if source.kind() == std::io::ErrorKind::NotFound =>
                    {
                        String::new()
                    }
                    Err(err) => return Err(err.to_string()),
                };
                let insertion = mnote_core::capture::insertion(&text, &title, entry);
                let text = insertion.apply(&text);
                vault.write(target, &text).map_err(|err| err.to_string())?;
                // The watcher skips mNote's own writes: index it here.
                self.index.borrow_mut().note_saved(target.clone(), &text);
                line = insertion.line;
            }
        }
        if open {
            self.go_to(target.clone(), line, cx);
        }
        cx.notify();
        Ok(())
    }

    /// `gd`: opens the note the `[[link]]` under the cursor names (or the
    /// first link after it on the line), at its `#heading` if it has one. A
    /// link to no note creates it.
    fn follow_link(&mut self, cx: &mut Context<Self>) {
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
            PickerAction::Command(n) => self.run_command(n, cx),
            PickerAction::EditFolder(folder) => self.open_folder_editor(folder, cx),
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
        let tx = self.syntax_ready.clone();
        self.buffer.on_syntax_ready(move || {
            let _ = tx.try_send(());
        });
        self.code_cache.clear();
        self.table_cache.clear();
        self.image_paths.borrow_mut().clear();
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
                    this.index_ready = true;
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
            AppCommand::View(name) => match name.as_deref().map(str::trim) {
                None | Some("") => self.set_view(self.view.next(), cx),
                Some(name) => match styled::View::from_name(name) {
                    Some(view) => self.set_view(view, cx),
                    None => {
                        self.reply = Some((
                            format!("E475: Invalid argument: {name} (styled, source or plain)")
                                .into(),
                            true,
                        ))
                    }
                },
            },
            AppCommand::Explore(folder) => self.open_explorer(folder, cx),
            AppCommand::Find(_) => self.not_yet(":find"),
            AppCommand::Daily => self.not_yet(":daily"),
            AppCommand::ListNext => self.list_step(1, cx),
            AppCommand::ListPrevious => self.list_step(-1, cx),
            AppCommand::FollowLink => self.follow_link(cx),
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
        let number = if current || !self.vim.options().relativenumber {
            ix + 1
        } else {
            ix.abs_diff(cursor_line)
        };

        let kind = line_kind(self.tree.as_ref(), rope, ix);
        let in_frontmatter = |line: usize| {
            matches!(
                line_kind(self.tree.as_ref(), rope, line),
                LineKind::Frontmatter { .. }
            )
        };
        let (shown, highlights) = self.decorate(ix, start, len, &content, p);
        let substituted = shown != content;
        let raw = current || substituted;
        let gutter_number = SharedString::from(number.to_string());

        // Source and plain views show every line as typed: no hidden
        // markers, no collapsed frontmatter, tables or images.
        let styled_view = self.view == styled::View::Styled;
        let plain_view = self.view == styled::View::Plain;

        // Frontmatter collapses to its tags unless the cursor is inside it.
        if let LineKind::Frontmatter { first } = kind
            && styled_view
            && !in_frontmatter(cursor_line)
        {
            if !first {
                return div().h(px(0.)).into_any_element();
            }
            let head: String = rope.slice(..rope.len_chars().min(8192)).to_string();
            let fm = mnote_core::Frontmatter::parse(&head);
            return self.frontmatter_row(gutter_number, current, &fm.tags, p);
        }

        if let LineKind::Image { src, alt } = &kind
            && styled_view
            && !raw
        {
            return self.image_row(gutter_number, current, src, alt, p);
        }

        if let LineKind::Table { block, delimiter } = &kind
            && styled_view
            && !raw
        {
            let block = self.table_block(block.clone(), ix);
            return self.table_row(ix, block, *delimiter, &content, gutter_number, current, p);
        }

        let text = if substituted { &shown } else { &content };
        let mut look = if styled_view {
            styled::look(&kind, text, raw, p)
        } else {
            styled::source_look(&kind, text, plain_view, p)
        };
        if substituted {
            look.shown = StyledLine::plain(&shown);
            look.styling.clear();
        }
        if let LineKind::Code { lang, block } = &kind
            && !plain_view
            && let Some(spans) = self.code_spans(lang, block.clone(), ix)
        {
            look.styling = styled::code_styling(&spans, p);
        }

        // Source positions (bytes of `content`) to positions in the shown
        // text, where markers may be hidden.
        let to_shown = |byte: usize| {
            if substituted {
                byte
            } else {
                look.shown
                    .display_byte(content[..byte.min(content.len())].chars().count())
            }
        };
        let highlights = highlights
            .into_iter()
            .map(|mut h| {
                if !substituted {
                    h.bytes = to_shown(h.bytes.start)..to_shown(h.bytes.end);
                }
                h
            })
            .collect();

        let mut line = LineText::new(
            look.shown.text.clone().into(),
            look.font(),
            look.style.size(),
            look.style.line_height(),
            look.color,
        )
        .styling(look.styling.clone())
        .highlights(highlights);

        if let Some(chars) = self.selection_on_line(ix, start, len) {
            let to_byte = |c: usize| to_shown(char_to_byte(&content, c));
            let end = if chars.end > len {
                look.shown.text.len() + 1
            } else {
                to_byte(chars.end)
            };
            line = line.selection(to_byte(chars.start)..end, Theme::color(p.select));
        }
        let mode = self.vim.mode();
        // In command mode the caret is in the command line.
        if current && mode != Mode::Command && (self.caret_on || mode != Mode::Insert) {
            line = line.caret(Some(Caret {
                byte: to_shown(char_to_byte(&content, cursor - start)),
                shape: self.vim.mode().caret(),
                color: Theme::color(p.cursor),
                on_color: Theme::color(p.on_cursor),
            }));
        }

        let line_height = look.style.line_height();
        let prefix: Option<AnyElement> = match &look.prefix {
            styled::Prefix::None => None,
            styled::Prefix::Bullet => Some(
                div()
                    .flex_none()
                    .w(px(2. * styled::CHAR_W))
                    .text_style(text::NOTE_BODY)
                    .text_color(Theme::color(p.ink_3))
                    .child(SharedString::new_static("•"))
                    .into_any_element(),
            ),
            styled::Prefix::Checkbox { checked } => Some(
                div()
                    .flex_none()
                    .w(px(2. * styled::CHAR_W))
                    .h(line_height)
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .size(px(14.))
                            .rounded(px(3.))
                            .border_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(11.))
                            .line_height(px(14.))
                            .when(*checked, |b| {
                                b.bg(Theme::color(p.cursor))
                                    .border_color(Theme::color(p.cursor))
                                    .text_color(Theme::color(p.on_cursor))
                                    .child(SharedString::new_static("✓"))
                            })
                            .when(!checked, |b| b.border_color(Theme::color(p.line_strong))),
                    )
                    .into_any_element(),
            ),
            styled::Prefix::Callout { label, color } => Some(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .pr(px(space::S2))
                    .h(line_height)
                    .text_style(text::HINT)
                    .text_color(*color)
                    .child(div().size(px(6.)).rounded(px(1.)).bg(*color))
                    .child(SharedString::from(label.clone()))
                    .into_any_element(),
            ),
        };

        let body: AnyElement = if look.rule {
            div()
                .h(look.height.unwrap_or(line_height))
                .flex()
                .items_center()
                .child(div().w_full().h(px(1.)).bg(Theme::color(p.line)))
                .into_any_element()
        } else {
            div()
                .flex()
                .items_start()
                .pl(px(look.indent))
                .when(look.background.is_some(), |row| row.px(px(space::S3)))
                .when_some(look.background, |row, bg| row.bg(bg))
                .when(look.round_top, |row| row.rounded_t(px(radius::MD)))
                .when(look.round_bottom, |row| row.rounded_b(px(radius::MD)))
                .when_some(look.height, |row, h| row.h(h).overflow_hidden())
                .children(prefix)
                .child(div().flex_1().min_w_0().child(line))
                .children(look.label.clone().map(|label| {
                    div()
                        .flex_none()
                        .pl(px(space::S2))
                        .h(px(28.))
                        .flex()
                        .items_center()
                        .text_style(text::HINT)
                        .font_weight(gpui::FontWeight::NORMAL)
                        .text_color(Theme::color(p.ink_3))
                        .child(label)
                }))
                .into_any_element()
        };

        // Line numbers in the note's own font and size, as in Zed, centred
        // on the first row of the line.
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
                    .text_style(text::NOTE_BODY)
                    .line_height(look.height.unwrap_or(line_height))
                    .text_color(Theme::color(if current { p.ink } else { p.ink_3 }))
                    .child(gutter_number),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .max_w(px(size::MEASURE))
                    .child(body),
            )
            .into_any_element()
    }

    /// One row of a table as cells side by side. Each cell wraps its own
    /// text; column widths come from the longest cell in each column, so
    /// every row of the table lines up. The `|---|` row isn't drawn.
    #[allow(clippy::too_many_arguments)]
    fn table_row(
        &mut self,
        ix: usize,
        block: std::ops::Range<usize>,
        delimiter: bool,
        content: &str,
        number: SharedString,
        current: bool,
        p: &Palette,
    ) -> AnyElement {
        if delimiter {
            return div().h(px(0.)).into_any_element();
        }
        let layout = self.table_layout(block.clone());
        // Each column's share of the width: its longest cell, within limits
        // so a short column stays readable and a long one doesn't take all.
        let weights: Vec<f32> = layout.0.iter().map(|&w| w.clamp(6, 60) as f32).collect();
        let total = weights.iter().sum::<f32>().max(1.);
        let cells = mnote_core::styled::table_cells(content);
        let columns = weights.len().max(cells.len()).max(1);
        let header = ix == block.start;
        let last = ix + 1 >= block.end;
        let body = text::NOTE_BODY;
        let line = Theme::color(p.line_strong);
        let row = div()
            .flex()
            .w_full()
            .border_l_1()
            .border_r_1()
            .border_b_1()
            .border_color(line)
            .when(header, |row| row.border_t_1().rounded_t(px(radius::MD)))
            .when(last, |row| row.rounded_b(px(radius::MD)))
            .children((0..columns).map(|c| {
                let cell = cells.get(c).cloned().unwrap_or_default();
                let shown = mnote_core::styled::style_inline(&cell, false, 0);
                let mut styling = styled::inline_styling(&shown, p);
                if header {
                    styling.insert(
                        0,
                        line::Styling {
                            bytes: 0..shown.text.len(),
                            weight: Some(gpui::FontWeight::SEMIBOLD),
                            ..line::Styling::default()
                        },
                    );
                }
                let share = weights.get(c).copied().unwrap_or(6.) / total;
                div()
                    .w(gpui::relative(share))
                    .min_w_0()
                    .px(px(space::S3))
                    .py(px(6.))
                    .when(c + 1 < columns, |cell| cell.border_r_1().border_color(line))
                    .child(
                        LineText::new(
                            shown.text.clone().into(),
                            Font {
                                weight: body.font_weight(),
                                ..font(body.font_family())
                            },
                            body.size(),
                            body.line_height(),
                            Theme::color(p.ink),
                        )
                        .styling(styling),
                    )
            }));
        div()
            .flex()
            .w_full()
            .when(current, |r| r.bg(Theme::color(p.well)))
            .child(
                div()
                    .flex_none()
                    .w(px(size::GUTTER_W))
                    .pr(px(space::S8))
                    .pt(px(6.))
                    .text_right()
                    .text_style(body)
                    .text_color(Theme::color(if current { p.ink } else { p.ink_3 }))
                    .child(number),
            )
            .child(div().flex_1().min_w_0().max_w(px(size::MEASURE)).child(row))
            .into_any_element()
    }

    /// The rows of the table around line `ix`: from the tree when it has
    /// them, else the `|` lines next to it.
    fn table_block(&self, block: std::ops::Range<usize>, ix: usize) -> std::ops::Range<usize> {
        if block.contains(&ix) {
            return block;
        }
        let rope = self.buffer.text();
        let is_row =
            |line: usize| rope.line(line).chars().find(|c| !c.is_whitespace()) == Some('|');
        let mut start = ix;
        while start > 0 && is_row(start - 1) {
            start -= 1;
        }
        let mut end = ix + 1;
        while end < line_count(rope) && is_row(end) {
            end += 1;
        }
        start..end
    }

    /// Column widths and alignments of the table on rows `block`, once per
    /// table and text revision.
    fn table_layout(&mut self, block: std::ops::Range<usize>) -> TableLayout {
        let revision = self.buffer.revision();
        if let Some((rev, layout)) = self.table_cache.get(&block.start)
            && *rev == revision
        {
            return layout.clone();
        }
        let rope = self.buffer.text();
        let first = block.start;
        let rows: Vec<String> = block
            .map(|row| {
                let start = line_start(rope, row);
                rope.slice(start..start + line_len(rope, row)).to_string()
            })
            .collect();
        let layout = Rc::new(mnote_core::styled::table_layout(&rows));
        if self.table_cache.len() > 64 {
            self.table_cache.retain(|_, (rev, _)| *rev == revision);
        }
        self.table_cache.insert(first, (revision, layout.clone()));
        layout
    }

    /// An image on its own line: the picture, or a quiet note when it
    /// can't be shown.
    fn image_row(
        &self,
        number: SharedString,
        current: bool,
        src: &str,
        alt: &str,
        p: &Palette,
    ) -> AnyElement {
        let gutter = div()
            .flex_none()
            .w(px(size::GUTTER_W))
            .pr(px(space::S8))
            .text_right()
            .text_style(text::NOTE_BODY)
            .text_color(Theme::color(if current { p.ink } else { p.ink_3 }))
            .child(number);
        let note_text = |message: String| {
            div()
                .text_style(text::STATUS)
                .line_height(text::NOTE_BODY.line_height())
                .text_color(Theme::color(p.ink_3))
                .child(SharedString::from(message))
        };
        let shown: AnyElement = match self.image_path(src) {
            Some(path) => {
                let missing = format!("image not found: {src}");
                gpui::img(path)
                    .max_w(px(size::MEASURE))
                    .max_h(px(480.))
                    .rounded(px(radius::MD))
                    .object_fit(gpui::ObjectFit::Contain)
                    .with_fallback(move || {
                        div()
                            .text_style(text::STATUS)
                            .child(SharedString::from(missing.clone()))
                            .into_any_element()
                    })
                    .into_any_element()
            }
            None if src.starts_with("http://") || src.starts_with("https://") => {
                note_text(format!("web image, not loaded: {src}")).into_any_element()
            }
            None => note_text(format!("image not found: {src}")).into_any_element(),
        };
        div()
            .flex()
            .w_full()
            .py(px(space::S1))
            .when(current, |row| row.bg(Theme::color(p.well)))
            .child(gutter)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .max_w(px(size::MEASURE))
                    .child(shown)
                    .when(!alt.is_empty(), |col| {
                        col.child(
                            div()
                                .text_style(text::STATUS)
                                .text_color(Theme::color(p.ink_3))
                                .child(SharedString::from(alt.to_owned())),
                        )
                    }),
            )
            .into_any_element()
    }

    /// Where an image path points: next to the note first, then from the
    /// vault's top folder. Web images aren't loaded.
    fn image_path(&self, src: &str) -> Option<std::path::PathBuf> {
        if src.contains("://") {
            return None;
        }
        if let Some(found) = self.image_paths.borrow().get(src) {
            return found.clone();
        }
        let found = self.find_image(src);
        self.image_paths
            .borrow_mut()
            .insert(src.to_owned(), found.clone());
        found
    }

    fn find_image(&self, src: &str) -> Option<std::path::PathBuf> {
        let Source::Note { vault, note, .. } = &self.source else {
            return None;
        };
        let src = src.replace("%20", " ");
        let beside = vault.path_of(note).parent().map(|dir| dir.join(&src));
        let from_root = vault.root().join(src.trim_start_matches('/'));
        [beside, Some(from_root)]
            .into_iter()
            .flatten()
            .find(|p| p.starts_with(vault.root()) && p.is_file())
    }

    /// The frontmatter's first line, collapsed to its tags.
    fn frontmatter_row(
        &self,
        number: SharedString,
        current: bool,
        tags: &[String],
        p: &Palette,
    ) -> AnyElement {
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
                    .text_style(text::NOTE_BODY)
                    .text_color(Theme::color(p.ink_3))
                    .child(number),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .py(px(2.))
                    .max_w(px(size::MEASURE))
                    .children(tags.iter().map(|tag| {
                        div()
                            .h(px(22.))
                            .px(px(space::S2))
                            .flex()
                            .items_center()
                            .rounded(px(11.))
                            .bg(Theme::color(p.well))
                            .text_style(text::UI)
                            .text_size(px(13.))
                            .text_color(Theme::color(p.ink_2))
                            .child(SharedString::from(format!("#{tag}")))
                    }))
                    .when(tags.is_empty(), |row| {
                        row.text_style(text::STATUS)
                            .text_color(Theme::color(p.ink_3))
                            .child(SharedString::new_static("frontmatter"))
                    }),
            )
            .into_any_element()
    }

    /// The code colours of line `ix` in the block `block` (its content rows),
    /// computed once per block and text revision.
    fn code_spans(
        &mut self,
        lang: &str,
        block: std::ops::Range<usize>,
        ix: usize,
    ) -> Option<Vec<(std::ops::Range<usize>, CodeStyle)>> {
        if !mnote_core::code::supported(lang) {
            return None;
        }
        let revision = self.buffer.revision();
        let cached = self
            .code_cache
            .get(&block.start)
            .filter(|(rev, lines)| *rev == revision && lines.len() == block.len())
            .map(|(_, lines)| lines.clone());
        let lines = match cached {
            Some(lines) => lines,
            None => {
                let rope = self.buffer.text();
                let rows: Vec<String> = block
                    .clone()
                    .map(|row| {
                        let start = line_start(rope, row);
                        rope.slice(start..start + line_len(rope, row)).to_string()
                    })
                    .collect();
                let code = rows.join("\n");
                let spans = mnote_core::code::highlight(lang, &code).unwrap_or_default();
                // Split the block's spans into lines.
                let mut per_line = Vec::with_capacity(rows.len());
                let mut offset = 0;
                for row in &rows {
                    let end = offset + row.len();
                    per_line.push(
                        spans
                            .iter()
                            .filter(|(r, _)| r.start < end && r.end > offset)
                            .map(|(r, s)| {
                                (r.start.max(offset) - offset..r.end.min(end) - offset, *s)
                            })
                            .collect(),
                    );
                    offset = end + 1;
                }
                let lines = Rc::new(per_line);
                if self.code_cache.len() > 64 {
                    self.code_cache.retain(|_, (rev, _)| *rev == revision);
                }
                self.code_cache
                    .insert(block.start, (revision, lines.clone()));
                lines
            }
        };
        lines.get(ix.checked_sub(block.start)?).cloned()
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

/// The vault folder as titles show it: `~/notes` under the home folder.
fn vault_name(root: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    match home.as_deref().and_then(|h| root.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => root.display().to_string(),
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
        self.tree = self.buffer.syntax_tree().cloned();
        let p = cx.theme().palette;
        let width = window.viewport_size().width / px(1.);
        let caret_on = self.caret_on;
        let picker = self
            .picker
            .as_mut()
            .map(|picker| picker.render(width, caret_on, p));
        let folder_edit = self
            .folder_edit
            .as_ref()
            .map(|editor| editor.render(caret_on, p));
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
        if self.view != styled::View::Styled {
            status = status.item(format!("{} view", self.view.name()));
        }
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
                    })
                    .when(
                        self.vim.command_line().is_none() && self.view != styled::View::Styled,
                        |bar| bar.hint(format!("space m view: {}", self.view.next().name())),
                    ),
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
            .children(folder_edit)
            .children(self.toast.clone().map(|(text, undo)| {
                let toast = Toast::new(text);
                let toast = if undo {
                    toast.action("u", "undo")
                } else {
                    toast
                };
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(px(48.))
                    .flex()
                    .justify_center()
                    .child(toast)
            }))
    }
}
