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

mod first_run;
mod ime;
pub(crate) mod keys;
pub(crate) mod line;
mod panes;
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
use mnote_core::config::Config;
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
use crate::theme::{ActiveTheme, Palette, StyledExt, Theme, radius, size, space, text};
use crate::ui::{CommandLine, CompletionPopup, MessagePanel, StatusLine, Toast, TopBar};
use line::{Caret, Highlight, LineText};

/// A table's column widths and alignments, shared by its rows.
type TableLayout = Rc<(Vec<usize>, Vec<mnote_core::styled::Align>)>;

/// Each measured key's (total, frame wait) in milliseconds.
type LatencyLog = Rc<RefCell<Vec<(f32, f32)>>>;

/// A code block's colours, line by line: byte ranges and their style.
type CodeLines = Vec<Vec<(std::ops::Range<usize>, CodeStyle)>>;

/// Width left for the macOS traffic-light buttons in the top bar.
const TRAFFIC_LIGHT_INSET: f32 = 76.;
/// How long the cursor rests on a line before git blame runs.
const BLAME_DELAY: Duration = Duration::from_millis(300);
/// How long typing pauses before the word count catches up.
const WORD_COUNT_DELAY: Duration = Duration::from_millis(400);
/// Half a blink cycle of the insert-mode bar caret (about 1 Hz).
const BLINK: Duration = Duration::from_millis(530);

use panes::{Axis, Doc, DocKey, Layout, PaneId, PaneSlot, Source};

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

/// Where the focused pane's lines wrap, as Vim asks for `gj` / `gk`.
struct WrapRows(Rc<RefCell<std::collections::HashMap<usize, Vec<usize>>>>);

impl mnote_vim::ScreenRows for WrapRows {
    fn row_starts(&self, line: usize) -> Vec<usize> {
        self.0.borrow().get(&line).cloned().unwrap_or_default()
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
    /// Every pane, the focused one's state being the fields above.
    panes: Vec<PaneSlot>,
    focused: PaneId,
    layout: Layout,
    next_pane: u32,
    /// Open notes other than the focused pane's.
    parked: Vec<Doc>,
    /// A line of an unfocused pane is being drawn: no caret, selection or
    /// typed-command preview.
    drawing_other: bool,
    /// `⌃w` was pressed: the next key is a pane command.
    pane_key: bool,
    /// The unfocused pane being drawn (while `drawing_other`).
    drawing_pane: PaneId,
    /// Where a mouse drag started (a character), while the button is down.
    drag_anchor: Option<usize>,
    /// Where the focused pane's visible lines wrap (characters each screen
    /// row starts at, by line), for `gj` / `gk`; filled as lines paint.
    wraps: Rc<RefCell<std::collections::HashMap<usize, Vec<usize>>>>,
    /// The notes folder, whatever the focused pane shows.
    vault: Option<Vault>,
    /// The focused note's word count, for the status line: its document and
    /// text revision when counted, and the count.
    words: Option<(DocKey, u64, usize)>,
    /// The next word count, waiting for typing to pause.
    words_task: Option<Task<()>>,
    /// The cursor line's git blame: its document, line and label (`None`
    /// when git has nothing to say).
    blame: Option<(DocKey, usize, Option<SharedString>)>,
    blame_task: Option<Task<()>>,
    /// Keystroke-to-pixels timing (`MNOTE_LATENCY=1`): when the last key
    /// arrived, how long it waited for the display's next frame to start
    /// drawing, and each key's (total, wait) in milliseconds.
    key_at: Option<Instant>,
    frame_wait: Option<f32>,
    latency: Option<LatencyLog>,
    /// The first-run screen, until a notes folder is chosen.
    first_run: Option<first_run::FirstRun>,
    /// Text the input system is still composing (an accent, Chinese or
    /// Japanese input), drawn under the caret until it's committed.
    marked: Option<String>,
    /// The settings in use, and where they're kept.
    config: Config,
    config_path: Option<std::path::PathBuf>,
    /// Changes by other apps. Boxed: the watcher is large, and a scratch
    /// note has none.
    _watcher: Option<Box<VaultWatcher>>,
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
        let (buffer, notice) = open_buffer(&vault, &note);
        let mut editor = Self::build(
            buffer,
            Source::Note {
                vault: vault.clone(),
                note,
            },
            window,
            cx,
        );
        editor.notice = notice;
        editor.attach_vault(vault, cx);
        editor
    }

    /// Starts on a scratch note and turns into `vault`'s editor at `note`
    /// (first run, after a folder is chosen).
    pub fn open_vault(&mut self, vault: Vault, note: NotePath, cx: &mut Context<Self>) {
        let (buffer, notice) = open_buffer(&vault, &note);
        let mut doc = self.new_doc(
            buffer,
            Source::Note {
                vault: vault.clone(),
                note,
            },
        );
        doc.notice = notice;
        self.show_doc(doc);
        self.attach_vault(vault, cx);
        cx.notify();
    }

    /// The vault's watcher, index and `:e` completion.
    fn attach_vault(&mut self, vault: Vault, cx: &mut Context<Self>) {
        // Changes by other apps arrive on the watcher's thread; hop to ours.
        let (tx, rx) = async_channel::unbounded::<VaultEvent>();
        self._watcher = vault
            .watch(move |event| {
                let _ = tx.try_send(event);
            })
            .ok()
            .map(Box::new);
        self.vim.set_completer(Box::new(VaultPaths(vault.clone())));
        self.vault = Some(vault);
        self.build_index(cx);
        self._tasks.push(cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv().await {
                if this
                    .update(cx, |this, cx| this.on_vault_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
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

        let first_note = source.doc_key();
        let wraps: Rc<RefCell<std::collections::HashMap<usize, Vec<usize>>>> = Rc::default();
        vim.set_screen_rows(Box::new(WrapRows(wraps.clone())));
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
            panes: vec![PaneSlot {
                id: PaneId(0),
                note: first_note,
                local: None,
            }],
            focused: PaneId(0),
            layout: Layout::Pane(PaneId(0)),
            next_pane: 1,
            parked: Vec::new(),
            drawing_other: false,
            pane_key: false,
            drawing_pane: PaneId(0),
            drag_anchor: None,
            wraps,
            vault: None,
            first_run: None,
            marked: None,
            words: None,
            words_task: None,
            blame: None,
            blame_task: None,
            key_at: None,
            frame_wait: None,
            latency: std::env::var_os("MNOTE_LATENCY").map(|_| Rc::default()),
            config: Config::default(),
            config_path: Config::path(),
            _watcher: None,
            _tasks: vec![redraw],
            _subscriptions: subscriptions,
        }
    }

    // ---- input ------------------------------------------------------------

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.latency.is_some() {
            self.key_at = Some(Instant::now());
        }
        let stroke = &event.keystroke;
        if self.first_run_key(stroke, cx) {
            cx.stop_propagation();
            return;
        }
        // While inserting, typed characters go through the system's input
        // (see `ime`), so accents and composed input work: let them pass.
        if self.takes_text()
            && !self.leader
            && !self.pane_key
            && !stroke.modifiers.control
            && stroke.key_char.is_some()
            && matches!(keys::vim_key(stroke), Some(mnote_vim::Key::Char(_)))
        {
            return;
        }
        if stroke.modifiers.platform && stroke.key == "k" && self.picker.is_none() {
            cx.stop_propagation();
            return self.open_picker(Picker::new(Box::new(CommandPalette::default())), cx);
        }
        if stroke.modifiers.platform
            && !stroke.modifiers.control
            && !stroke.modifiers.alt
            && self.clipboard_shortcut(&stroke.key, cx)
        {
            cx.stop_propagation();
            return;
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
        if std::mem::take(&mut self.pane_key) {
            self.pane_command(key, cx);
            cx.notify();
            return true;
        }
        if key == mnote_vim::Key::Ctrl('w')
            && self.vim.mode() == Mode::Normal
            && !self.vim.is_pending()
            && self.vim.cmd_window().is_none()
        {
            self.pane_key = true;
            return true;
        }
        // The system clipboard is read just before a paste that may use it:
        // `"+p`, and plain `p` / `P` when y, d and p share it.
        let plain_paste = matches!(key, mnote_vim::Key::Char('p' | 'P'))
            && matches!(
                self.vim.mode(),
                Mode::Normal | Mode::Visual | Mode::VisualLine
            )
            && matches!(self.vim.pending_register(), None | Some('"'))
            && !self.vim.awaits_register()
            && self.config.system_clipboard;
        let clipboard_paste = plain_paste
            || matches!(self.vim.pending_register(), Some('+' | '*'))
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
        // The leader (config.toml's `leader`) comes before Vim, so a leader
        // like `,` isn't taken by Vim's own `,`.
        if key == mnote_vim::Key::Char(crate::commands::leader())
            && self.vim.mode() == Mode::Normal
            && !self.vim.is_pending()
            && self.vim.cmd_window().is_none()
        {
            self.reply = None;
            self.leader = true;
            return true;
        }
        let mode_before = self.shown_mode();
        let revision_before = self.buffer.revision();
        self.reply = None;

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
        self.lines
            .scroll_to_reveal_item(line_of(self.buffer.text(), reveal));
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
        if key == mnote_vim::Key::Char(',') {
            return self.open_config(cx);
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
                // A leading space is the leader binding: replay it with the
                // real leader.
                let keys = match keys.strip_prefix(' ') {
                    Some(rest) => format!("{}{rest}", crate::commands::leader()),
                    None => keys.to_owned(),
                };
                for key in mnote_vim::parse_keys(&keys) {
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
            } else if self.parked_notes().contains(target) {
                // Open in another pane: into its buffer, so it isn't stale.
                line = self
                    .with_parked(target, |this| {
                        let text = this.buffer.to_string();
                        let insertion = mnote_core::capture::insertion(&text, &title, entry);
                        let at = text[..insertion.at].chars().count();
                        this.vim.replace(&mut this.buffer, at..at, &insertion.text);
                        this.save_now(cx);
                        insertion.line
                    })
                    .unwrap_or(0);
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

    /// Opens another note of the same vault in the focused pane, saving the
    /// current one first. Registers, history and options carry over. A note
    /// another pane shows keeps its buffer, shared.
    fn switch_to(&mut self, target: NotePath, cx: &mut Context<Self>) {
        if self.source.key() == Some(&target) {
            return;
        }
        let Some(vault) = self.vault.clone() else {
            return;
        };
        self.save_now(cx);
        let next = match self.unpark(&DocKey::Note(target.clone())) {
            Some(doc) => doc,
            None => match Buffer::open(&vault, &target) {
                Ok(buffer) => self.new_doc(
                    buffer,
                    Source::Note {
                        vault,
                        note: target,
                    },
                ),
                Err(err) => {
                    self.reply = Some((format!("Couldn't open {target}: {err}").into(), true));
                    return;
                }
            },
        };
        self.show_doc(next);
    }

    /// `space ,`: mNote's own config.toml in the focused pane (made from the
    /// current settings if there's none yet). Saving it applies it.
    fn open_config(&mut self, cx: &mut Context<Self>) {
        if self.source.doc_key() == DocKey::Config {
            return;
        }
        let Some(path) = self.config_path.clone() else {
            self.reply = Some(("No home folder for config.toml".into(), true));
            return;
        };
        self.save_now(cx);
        let next = match self.unpark(&DocKey::Config) {
            Some(doc) => doc,
            None => {
                let text = match std::fs::read_to_string(&path) {
                    Ok(text) => text,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        let text = self.config.template();
                        if let Err(err) = mnote_core::config::write_file(&path, &text) {
                            self.reply = Some((
                                format!("Couldn't create {}: {err}", path.display()).into(),
                                true,
                            ));
                            return;
                        }
                        text
                    }
                    Err(err) => {
                        self.reply = Some((
                            format!("Couldn't open {}: {err}", path.display()).into(),
                            true,
                        ));
                        return;
                    }
                };
                self.new_doc(Buffer::new(&text), Source::Config(path))
            }
        };
        self.show_doc(next);
        cx.notify();
    }

    /// Puts `next` in the focused pane; the doc it replaces stays open if
    /// another pane shows it.
    fn show_doc(&mut self, mut next: Doc) {
        std::mem::swap(&mut self.buffer, &mut next.buffer);
        std::mem::swap(&mut self.source, &mut next.source);
        std::mem::swap(&mut self.autosave, &mut next.autosave);
        std::mem::swap(&mut self.autosave_task, &mut next.autosave_task);
        std::mem::swap(&mut self.notice, &mut next.notice);
        std::mem::swap(&mut self.tree, &mut next.tree);
        std::mem::swap(&mut self.code_cache, &mut next.code_cache);
        std::mem::swap(&mut self.table_cache, &mut next.table_cache);
        std::mem::swap(&mut self.image_paths, &mut next.image_paths);
        self.set_pane_note();
        self.park_or_drop(next);
        self.wraps.borrow_mut().clear();
        self.vim.set_cursor(0);
        self.lines.reset(self.buffer.text().len_lines());
    }

    /// `:daily`: today's note (from `daily_note` in config.toml), made if it
    /// doesn't exist yet.
    fn open_daily(&mut self, cx: &mut Context<Self>) {
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

    /// Takes settings into use: the view and line numbers here, the theme,
    /// and (on macOS) the global keys. At startup and when config.toml is
    /// saved.
    pub fn apply_config(&mut self, config: Config, cx: &mut Context<Self>) {
        // Only a changed setting moves the view, so `space m` choices stay.
        if config.view != self.config.view {
            self.view = styled::View::from_config(config.view);
        }
        let set = if config.relative_numbers {
            "set relativenumber"
        } else {
            "set norelativenumber"
        };
        let _ = self.vim.execute(set, &mut self.buffer);
        crate::theme::Theme::set_choice(config.theme, cx);
        crate::commands::set_leader(config.leader);
        self.vim.share_clipboard(config.system_clipboard);
        self.vim.set_wrap_motion(config.wrap_motion);
        // Fonts: an installed one, else the bundled default (and say so).
        let installed = cx.text_system().all_font_names();
        let mut missing = Vec::new();
        let mut pick = |want: &str, default: &'static str| {
            if installed.iter().any(|n| n.eq_ignore_ascii_case(want)) {
                SharedString::from(want.to_owned())
            } else {
                missing.push(want.to_owned());
                SharedString::new_static(default)
            }
        };
        let fonts = crate::theme::Fonts {
            mono: pick(&config.font, crate::theme::family::MONO),
            prose: pick(&config.ui_font, crate::theme::family::PROSE),
            note_size: config.font_size as f32,
            ui_size: config.ui_font_size as f32,
        };
        if crate::theme::set_fonts(fonts) {
            self.remeasure_all();
            cx.refresh_windows();
        }
        if !missing.is_empty() {
            self.notice =
                Some(format!("{} isn't installed; using IBM Plex", missing.join(" and ")).into());
        }
        self.config = config;
        cx.notify();
    }

    /// Counts the focused note's words in the background, at most once per
    /// pause in typing, never on the keystroke path.
    fn count_words_soon(&mut self, cx: &mut Context<Self>) {
        let key = self.source.doc_key();
        let revision = self.buffer.revision();
        let current = self
            .words
            .as_ref()
            .is_some_and(|(k, r, _)| *k == key && *r == revision);
        if current || self.words_task.is_some() {
            return;
        }
        self.words_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(WORD_COUNT_DELAY).await;
            // The text as it is now, after the pause.
            let Ok((key, revision, text)) = this.update(cx, |this, _| {
                (
                    this.source.doc_key(),
                    this.buffer.revision(),
                    this.buffer.text().clone(),
                )
            }) else {
                return;
            };
            let words = cx
                .background_executor()
                .spawn(async move { mnote_core::text::count_words(&text) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.words = Some((key, revision, words));
                this.words_task = None;
                cx.notify();
            });
        }));
    }

    /// Asks git who last changed the cursor line, once the cursor rests on
    /// it (not while typing), off the UI thread.
    fn blame_soon(&mut self, cx: &mut Context<Self>) {
        if !self.config.git_blame || self.vim.mode() == Mode::Insert || self.blame_task.is_some() {
            return;
        }
        let Source::Note { .. } = &self.source else {
            return;
        };
        let key = self.source.doc_key();
        let line = line_of(self.buffer.text(), self.vim.cursor());
        if self
            .blame
            .as_ref()
            .is_some_and(|(k, l, _)| *k == key && *l == line)
        {
            return;
        }
        self.blame_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(BLAME_DELAY).await;
            let Ok(Some((path, text, line, key))) = this.update(cx, |this, _| {
                let Source::Note { vault, note } = &this.source else {
                    return None;
                };
                let line = line_of(this.buffer.text(), this.vim.cursor());
                Some((
                    vault.path_of(note),
                    this.buffer.text().clone(),
                    line,
                    this.source.doc_key(),
                ))
            }) else {
                let _ = this.update(cx, |this, _| this.blame_task = None);
                return;
            };
            let label = cx
                .background_executor()
                .spawn(async move {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs() as i64);
                    mnote_core::blame::blame_line(&path, &text.to_string(), line)
                        .map(|b| SharedString::from(b.label(now)))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.blame = Some((key, line, label));
                this.blame_task = None;
                cx.notify();
            });
        }));
    }

    /// `MNOTE_LATENCY_BENCH=n`: types `n` characters into this window, one
    /// every 25 ms, then prints keystroke-to-pixels times and quits. The
    /// time splits into mNote's work (handling, render, layout, paint),
    /// which the 8 ms budget is for, and the wait for the display's next
    /// frame, which every app has (up to one refresh, 8.3 ms at 120 Hz).
    pub fn run_latency_bench(&mut self, keys: usize, cx: &mut Context<Self>) {
        let log = self.latency.get_or_insert_with(Rc::default).clone();
        self._tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let _ = this.update(cx, |this, cx| {
                this.press(mnote_vim::Key::Char('G'), cx);
                this.press(mnote_vim::Key::Char('o'), cx);
            });
            for i in 0..keys {
                let c = if i % 8 == 7 { ' ' } else { (b'a' + (i % 26) as u8) as char };
                let _ = this.update(cx, |this, cx| {
                    this.key_at = Some(Instant::now());
                    this.press(mnote_vim::Key::Char(c), cx);
                });
                cx.background_executor().timer(Duration::from_millis(25)).await;
            }
            cx.background_executor().timer(Duration::from_millis(200)).await;
            // The first keys warm caches up; they aren't typing.
            let samples: Vec<(f32, f32)> = log.borrow().iter().skip(10).copied().collect();
            let stats = |mut xs: Vec<f32>| {
                xs.sort_by(f32::total_cmp);
                let at = |q: f32| xs.get(((xs.len() as f32 - 1.) * q) as usize).copied().unwrap_or(0.);
                format!("p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms", at(0.5), at(0.95), xs.last().copied().unwrap_or(0.))
            };
            eprintln!(
                "mnote latency over {} keys\n  mNote's work (key to painted frame, less the wait): {}\n  waiting for the display's next frame:               {}\n  keystroke to painted frame:                        {}",
                samples.len(),
                stats(samples.iter().map(|(t, w)| t - w).collect()),
                stats(samples.iter().map(|(_, w)| *w).collect()),
                stats(samples.iter().map(|(t, _)| *t).collect()),
            );
            let _ = this.update(cx, |_, cx| cx.quit());
        }));
    }

    /// A picker row pressed: selected, and opened on a double click.
    fn picker_click(&mut self, ix: usize, clicks: usize, cx: &mut Context<Self>) {
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
    fn mouse_on_line(
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
        let visual = matches!(self.vim.mode(), Mode::Visual | Mode::VisualLine);
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
    fn clipboard_shortcut(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
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
        let visual = matches!(mode, Mode::Visual | Mode::VisualLine);
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

    /// Shows the first-run screen (no config.toml yet, no folder given):
    /// `config` holds the keys it starts with.
    pub fn start_first_run(&mut self, config: Config, cx: &mut Context<Self>) {
        self.first_run = Some(first_run::FirstRun::new(config));
        cx.notify();
    }

    /// The global keys couldn't all be registered, or config.toml has a
    /// mistake: say so in the status line.
    pub fn show_notice(&mut self, notice: String, cx: &mut Context<Self>) {
        self.notice = Some(notice.into());
        cx.notify();
    }

    /// Reads the whole vault into the index on a background thread.
    fn build_index(&mut self, cx: &mut Context<Self>) {
        let Some(vault) = self.vault.clone() else {
            return;
        };
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
            AppCommand::Daily => self.open_daily(cx),
            AppCommand::ListNext => self.list_step(1, cx),
            AppCommand::ListPrevious => self.list_step(-1, cx),
            AppCommand::FollowLink => self.follow_link(cx),
            AppCommand::Split { vertical, path } => {
                let axis = if vertical { Axis::Row } else { Axis::Column };
                let target = match path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
                    None => None,
                    Some(name) => {
                        let found = {
                            let index = self.index.borrow();
                            index.resolve(name).map(|i| index.notes()[i].path.clone())
                        };
                        match found {
                            Some(note) => Some(note),
                            None => {
                                self.reply =
                                    Some((format!("E32: No note named {name}").into(), true));
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
            AppCommand::Close => {
                if !self.close_pane(cx) {
                    self.reply = Some(("E444: Cannot close last window".into(), true));
                }
            }
            AppCommand::Only => self.only_pane(cx),
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
        // With split panes, `:q` closes the focused one (notes autosave).
        if self.panes.len() > 1 {
            if write && let Err(err) = self.write(cx) {
                self.reply = Some((err.into(), true));
                return;
            }
            self.save_now(cx);
            self.close_pane(cx);
            return;
        }
        let result = match self.source {
            Source::Scratch if write => Err("E32: No file name".to_owned()),
            Source::Scratch if self.buffer.is_dirty() && !force => {
                Err("E37: No write since last change (add ! to override)".to_owned())
            }
            Source::Scratch => Ok(()),
            Source::Note { .. } | Source::Config(_) => self.write(cx),
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
        self.splice_other_panes(top..old - bottom, new - top - bottom);
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
        if let Source::Config(path) = &self.source {
            let path = path.clone();
            return self.save_config(&path, cx);
        }
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

    /// Saves config.toml and applies it; a mistake is shown, with its line,
    /// and the settings in use stay.
    fn save_config(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
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

    fn on_vault_event(&mut self, event: VaultEvent, cx: &mut Context<Self>) {
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
        cx.notify();
    }

    /// The focused note changed or went away on disk. `live`: it's the
    /// focused pane's own (not a parked note swapped in).
    fn reload_from_disk(&mut self, event: &VaultEvent, live: bool) {
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
        // An unfocused pane shows no cursor line.
        let current = ix == cursor_line && !self.drawing_other;
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
        // config.toml shows as typed, its `#` comments not headings.
        let config = matches!(self.source, Source::Config(_));
        let styled_view = self.view == styled::View::Styled && !config;
        let plain_view = self.view == styled::View::Plain || config;

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
        let mut highlights: Vec<Highlight> = highlights
            .into_iter()
            .map(|mut h| {
                if !substituted {
                    h.bytes = to_shown(h.bytes.start)..to_shown(h.bytes.end);
                }
                h
            })
            .collect();

        // Text still being composed shows at the caret, underlined. Only the
        // cursor line has a caret (elsewhere the cursor may be before `start`).
        let caret_byte = if current {
            to_shown(char_to_byte(&content, cursor - start))
        } else {
            0
        };
        let marked = self
            .marked
            .clone()
            .filter(|_| current && self.vim.mode() == Mode::Insert);
        let mut text: String = look.shown.text.to_string();
        let mut styling = look.styling.clone();
        let marked_len = marked.as_ref().map_or(0, String::len);
        if let Some(marked) = &marked {
            let at = caret_byte.min(text.len());
            text.insert_str(at, marked);
            let shift = |r: std::ops::Range<usize>| {
                let from = if r.start >= at {
                    r.start + marked_len
                } else {
                    r.start
                };
                let to = if r.end > at {
                    r.end + marked_len
                } else {
                    r.end
                };
                from..to
            };
            for s in &mut styling {
                s.bytes = shift(s.bytes.clone());
            }
            for h in &mut highlights {
                h.bytes = shift(h.bytes.clone());
            }
            styling.push(line::Styling {
                bytes: at..at + marked_len,
                color: Some(Theme::color(p.cursor)),
                underline: true,
                ..line::Styling::default()
            });
        }

        let mut line = LineText::new(
            text.into(),
            look.font(),
            look.style.size(),
            look.style.line_height(),
            look.color,
        )
        .styling(styling)
        .highlights(highlights);

        // The mouse: the byte under the pointer back to the note's
        // character, then a click, drag or selection in this pane.
        {
            let pane = if self.drawing_other {
                self.drawing_pane
            } else {
                self.focused
            };
            let view = cx.entity().downgrade();
            let map = Rc::new((content.clone(), look.shown.clone(), substituted));
            // A byte of the shown text to a character of the note's line.
            let to_char = move |byte: usize| {
                let (content, shown, substituted) = &*map;
                if *substituted {
                    let mut b = byte.min(content.len());
                    while !content.is_char_boundary(b) {
                        b -= 1;
                    }
                    content[..b].chars().count()
                } else {
                    (0..=content.chars().count())
                        .take_while(|&c| shown.display_byte(c) <= byte)
                        .last()
                        .unwrap_or(0)
                }
            };
            // Where this line wraps, for `gj` / `gk` (the focused pane's).
            if !self.drawing_other {
                let wraps = self.wraps.clone();
                let to_char = to_char.clone();
                line = line.on_wraps(Rc::new(move |bytes| {
                    let starts = bytes.into_iter().map(&to_char).collect();
                    wraps.borrow_mut().insert(ix, starts);
                }));
            }
            line = line.on_mouse(Rc::new(move |byte, hit, _window, cx| {
                let ch = to_char(byte);
                let _ = view.update(cx, |this, cx| this.mouse_on_line(pane, ix, ch, hit, cx));
            }));
        }

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
                byte: caret_byte + marked_len,
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
                    .w(px(2. * styled::char_w()))
                    .text_style(text::NOTE_BODY)
                    .text_color(Theme::color(p.ink_3))
                    .child(SharedString::new_static("•"))
                    .into_any_element(),
            ),
            styled::Prefix::Checkbox { checked } => Some(
                div()
                    .flex_none()
                    .w(px(2. * styled::char_w()))
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

        // Who last changed the cursor line (`git_blame`), at its right.
        let blame = current
            .then_some(self.blame.as_ref())
            .flatten()
            .filter(|(key, line, _)| *key == self.source.doc_key() && *line == ix)
            .and_then(|(_, _, label)| label.clone())
            .filter(|_| self.config.git_blame && self.vim.mode() != Mode::Insert);

        // Line numbers in the note's own font and size, as in Zed, centred
        // on the first row of the line.
        div()
            .flex()
            .relative()
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
            .children(blame.map(|label| {
                div()
                    .absolute()
                    .right_0()
                    .top_0()
                    .h(line_height)
                    .flex()
                    .items_center()
                    .pl(px(space::S4))
                    .pr(px(space::S2))
                    .bg(Theme::color(p.well))
                    .text_style(text::HINT)
                    .text_color(Theme::color(p.ink_3))
                    .child(label)
            }))
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
        if self.drawing_other {
            return false;
        }
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
        // A command being typed previews in the focused pane only.
        let preview = self.vim.preview().filter(|_| !self.drawing_other);
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
        if self.drawing_other {
            return None;
        }
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
            Source::Config(_) => vec![
                SharedString::new_static("settings"),
                SharedString::new_static("config.toml"),
            ],
            Source::Note { note, .. } => note.segments().map(|s| s.to_owned().into()).collect(),
        }
    }
}

/// A note's buffer; one that doesn't exist yet starts empty and is made on
/// the first save.
fn open_buffer(vault: &Vault, note: &NotePath) -> (Buffer, Option<SharedString>) {
    match Buffer::open(vault, note) {
        Ok(buffer) => (buffer, None),
        Err(_) if !vault.path_of(note).exists() => (Buffer::new(""), None),
        Err(err) => (
            Buffer::new(""),
            Some(SharedString::from(format!("Couldn't open {note}: {err}"))),
        ),
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
            ..font(crate::theme::mono())
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

impl Editor {
    /// The panes as laid out, with a rule between parts.
    fn render_layout(
        &self,
        layout: &Layout,
        view: &gpui::Entity<Editor>,
        p: &Palette,
    ) -> AnyElement {
        match layout {
            Layout::Pane(id) => self.render_pane(*id, view, p),
            Layout::Split(axis, parts) => {
                let row = *axis == Axis::Row;
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .when(!row, |d| d.flex_col())
                    .children(parts.iter().enumerate().map(|(i, part)| {
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .when(i > 0 && row, |d| d.border_l_1())
                            .when(i > 0 && !row, |d| d.border_t_1())
                            .border_color(Theme::color(p.line))
                            .child(self.render_layout(part, view, p))
                    }))
                    .into_any_element()
            }
        }
    }

    /// One pane: its note's visible lines. With several panes, the focused
    /// one has a `cursor` rule along its top and the others are quieter.
    fn render_pane(&self, id: PaneId, view: &gpui::Entity<Editor>, p: &Palette) -> AnyElement {
        let focused = id == self.focused;
        let several = self.panes.len() > 1;
        let lines = if focused {
            Some(self.lines.clone())
        } else {
            self.panes
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.local.as_ref())
                .map(|l| l.lines.clone())
        };
        let Some(lines) = lines else {
            return div().flex_1().into_any_element();
        };
        let view = view.clone();
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .justify_center()
            .pt(px(space::S8))
            .when(several, |d| {
                d.px(px(space::S3)).border_t_2().border_color(if focused {
                    Theme::color(p.cursor)
                } else {
                    gpui::transparent_black()
                })
            })
            .when(several && !focused, |d| d.opacity(0.72))
            .child(
                list(lines, move |ix, _window, cx: &mut App| {
                    view.update(cx, |this, cx| {
                        if this.focused == id {
                            this.render_line(ix, cx)
                        } else {
                            this.render_other(id, ix, cx)
                        }
                    })
                })
                .size_full()
                .max_w(px(size::GUTTER_W + size::MEASURE + space::S8)),
            )
            .into_any_element()
    }
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The frame has started drawing: the key's wait for it ends here.
        if let Some(at) = self.key_at {
            self.frame_wait = Some(at.elapsed().as_secs_f32() * 1000.);
        }
        if let Some(first_run) = self.render_first_run(cx) {
            return div()
                .size_full()
                .track_focus(&self.focus)
                .on_key_down(cx.listener(Self::on_key))
                .child(first_run)
                .into_any_element();
        }
        self.tree = self.buffer.syntax_tree().cloned();
        self.refresh_parked_trees();
        self.count_words_soon(cx);
        self.blame_soon(cx);
        // A page for Page Up/Down and ⌃f ⌃b: the lines that fit the window.
        let text_h = window.viewport_size().height / px(1.) - 4. * crate::theme::bar_h();
        let page = (text_h / (text::NOTE_BODY.line_height().max(px(1.)) / px(1.))).floor();
        self.vim.set_page_lines(page.max(1.) as usize);
        let p = cx.theme().palette;
        let width = window.viewport_size().width / px(1.);
        let height = window.viewport_size().height / px(1.);
        let caret_on = self.caret_on;
        let mouse = {
            let rows = cx.entity().downgrade();
            let scrim = rows.clone();
            crate::picker::PickerMouse {
                on_row: Rc::new(move |ix, clicks, _window, cx| {
                    let _ = rows.update(cx, |this, cx| this.picker_click(ix, clicks, cx));
                }),
                on_scrim: Rc::new(move |_window, cx| {
                    let _ = scrim.update(cx, |this, cx| {
                        this.picker = None;
                        this.restart_blink(cx);
                        cx.notify();
                    });
                }),
            }
        };
        let picker = self
            .picker
            .as_mut()
            .map(|picker| picker.render(width, height, caret_on, &mouse, p));
        let folder_edit = self
            .folder_edit
            .as_ref()
            .map(|editor| editor.render(width, height, caret_on, p));
        let rope = self.buffer.text();
        let cursor = self.vim.cursor();
        let line = line_of(rope, cursor);
        let column = cursor - line_start(rope, line);
        let file = match &self.source {
            Source::Scratch => SharedString::new_static("scratch (not saved)"),
            Source::Config(_) => SharedString::new_static("config.toml"),
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
        for file in self.other_pane_files() {
            status = status.item(format!("│ {file}"));
        }
        let words = self
            .words
            .as_ref()
            .filter(|(key, _, _)| *key == self.source.doc_key())
            .map(|(_, _, words)| *words);
        let status = status
            .when_some(words, |status, words| {
                status.right(if words == 1 {
                    "1 word".to_owned()
                } else {
                    format!("{words} words")
                })
            })
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
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, _| this.drag_anchor = None),
            )
            .child(
                TopBar::new(self.path_segments())
                    .dirty(self.buffer.is_dirty())
                    .leading_inset(px(TRAFFIC_LIGHT_INSET))
                    .when(self.panes.len() > 1, |bar| {
                        bar.hint("⌃w h j k l move · ⌃w c close")
                    })
                    .when(self.vim.command_line().is_some(), |bar| {
                        bar.hint("⇥ complete · ↑ history · ⌃f edit")
                    })
                    .when(
                        self.vim.command_line().is_none() && self.view != styled::View::Styled,
                        |bar| {
                            bar.hint(format!(
                                "{} view: {}",
                                crate::commands::with_leader("space m"),
                                self.view.next().name()
                            ))
                        },
                    ),
            )
            .child({
                // Registers the editor with the system's text input each
                // frame (accents, IME); see `ime`.
                let focus = self.focus.clone();
                let input = view.clone();
                gpui::canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &focus,
                            gpui::ElementInputHandler::new(bounds, input),
                            cx,
                        );
                    },
                )
                .absolute()
                .size_full()
            })
            .child(self.render_layout(&self.layout.clone(), &view, p))
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
                    .bottom(px(crate::theme::bar_h() + space::S1))
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
            .children(self.latency.clone().map(|log| {
                // Painted last: the time from the key to here is the key's
                // handling, render, layout and paint.
                let key_at = self.key_at.take();
                let wait = self.frame_wait.take().unwrap_or(0.);
                gpui::canvas(
                    |_, _, _| (),
                    move |_, _, _, _| {
                        if let Some(at) = key_at {
                            log.borrow_mut()
                                .push((at.elapsed().as_secs_f32() * 1000., wait));
                        }
                    },
                )
                .absolute()
                .size_0()
            }))
            .into_any_element()
    }
}
