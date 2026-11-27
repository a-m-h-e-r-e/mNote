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

mod actions;
pub mod blocks;
mod code_files;
pub mod commands;
mod edit_groups;
pub mod events;
mod first_run;
mod grammars;
mod ime;
mod input;
pub(crate) mod keys;
pub(crate) mod line;
mod lines;
mod overlays;
mod pane_chrome;
mod pane_menu;
mod panes;
mod plugin_list;
mod plugins;
mod saving;
mod services;
mod session;
mod styled;
mod tab_line;
mod tabs;
mod untitled;
mod view;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, ClipboardItem, Context, FocusHandle, Font, IntoElement, KeyDownEvent,
    ListAlignment, ListState, ParentElement, Render, SharedString, Styled, Subscription, Task,
    Window, div, font, list, prelude::*, px,
};
use mnote_core::config::Config;
use mnote_core::files::FileOp;
use mnote_core::folder_edit::{Change, FolderEntry};
use mnote_core::lang::scopes::Scope;
use mnote_core::links::{LinkMove, count_relinks, relink_vault};
use mnote_core::styled::{LineKind, StyledLine, line_kind};
use mnote_core::{Autosave, Buffer, NoteIndex, NotePath, Reload, Vault, VaultEvent, VaultWatcher};
use mnote_vim::ex::AppCommand;
use mnote_vim::ex::cmdline::{Completer, line_matches};
use mnote_vim::ex::cmdwin::CmdWindow;
use mnote_vim::text::{clamp_normal, line_count, line_len, line_of, line_start};
use mnote_vim::{Clipboard, Effect, Mode, Vim};

use crate::commands::{Invocation, Run};
use crate::folder_edit::{FolderEditor, FolderOutcome};
use crate::picker::backlinks::BacklinksPicker;
use crate::picker::explorer::{Explorer, FileHistory, FileUndo};
use crate::picker::finder::{CurrentNote, Finder};
use crate::picker::list::ListPicker;
use crate::picker::palette::CommandPalette;
use crate::picker::{Job, JobResult, ListEntry, Picker, PickerAction, PickerEffect, PickerOutcome};
use crate::theme::{ActiveTheme, Palette, StyledExt, Theme, radius, size, space, text};
use crate::ui::{CommandLine, CompletionPopup, MessagePanel, StatusLine, Toast};
use line::{Caret, Highlight, LineText};

/// A table's column widths and alignments, shared by its rows.
type TableLayout = Rc<(Vec<usize>, Vec<mnote_core::styled::Align>)>;

/// Each measured key's (total, frame wait) in milliseconds.
/// A code block's colours, line by line: byte ranges and their style.
type CodeLines = Vec<Vec<(std::ops::Range<usize>, Scope)>>;

/// Width left for the macOS traffic-light buttons in the top bar.
const TRAFFIC_LIGHT_INSET: f32 = 76.;
/// How long keys wait to become a mapping before they run as typed.
const MAPPING_TIMEOUT: Duration = Duration::from_millis(1000);
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
    /// Keys typed in NORMAL that start a key binding (`space`, `⌃w`), until
    /// it completes or turns out not to be one (see [`crate::keymap`]).
    keyseq: Vec<mnote_vim::Key>,
    /// Runs a binding that's also the start of a longer one, if no key
    /// follows in a moment.
    keyseq_task: Option<Task<()>>,
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
    /// Open notes other than the focused pane's, shared by every tab.
    parked: Vec<Doc>,
    /// Closed tabs, for `:tabreopen`.
    closed_tabs: Vec<tabs::ClosedTab>,
    /// The tab rows' scroll, and the window buttons' state.
    tab_rows: tab_line::TabRows,
    /// How the next note opened takes its place (kept, preview, replace).
    opening: tabs::OpenAs,
    /// A scrollbar held by the mouse (its pane; vertical), and where each
    /// pane's are.
    scrollbar_drag: Option<(PaneId, bool)>,
    scrollbar_bounds: pane_chrome::ScrollbarBounds,
    /// `⌘+` / `⌘-` zoom (steps of 10%), over config.toml's fonts.
    zoom: i32,
    base_fonts: crate::theme::Fonts,
    /// The session (panes and tabs) has been restored, so changes save it;
    /// the shape last saved, and the save waiting to happen.
    session_restored: bool,
    session_shape_saved: String,
    session_task: Option<Task<()>>,
    /// The open `…` menu, and where each row's `…` button is.
    pane_menu: Option<pane_menu::PaneMenu>,
    menu_anchors: pane_menu::MenuAnchors,
    /// The last untitled note's number, and the folder each one was
    /// opened from (where the save prompt suggests it goes).
    next_untitled: u32,
    untitled_folders: std::collections::HashMap<u32, String>,
    /// Which notes each edit group spanning several notes touched, so `u`
    /// in one carries to the others.
    edit_groups: mnote_core::text::GroupDocs<DocKey>,
    /// A line of an unfocused pane is being drawn: no caret, selection or
    /// typed-command preview.
    drawing_other: bool,
    /// The unfocused pane being drawn (while `drawing_other`).
    drawing_pane: PaneId,
    /// Where a mouse drag started (a character), while the button is down.
    drag_anchor: Option<usize>,
    /// Gives up on keys waiting to become a mapping.
    map_task: Option<Task<()>>,
    /// Where the focused pane's visible lines wrap (characters each screen
    /// row starts at, by line), for `gj` / `gk`; filled as lines paint.
    wraps: Rc<RefCell<std::collections::HashMap<usize, Vec<usize>>>>,
    /// With `nowrap`, how far the focused pane's text is scrolled sideways;
    /// every row shares it, and the cursor line moves it to keep the caret
    /// in view as it's laid out.
    scroll_x: Rc<std::cell::Cell<gpui::Pixels>>,
    /// The notes folder, whatever the focused pane shows.
    vault: Option<Vault>,
    /// The focused note's word count, for the status line: its document and
    /// text revision when counted, and the count.
    words: services::WordCount,
    /// The next word count, waiting for typing to pause.
    /// The cursor line's git blame: its document, line and label (`None`
    /// when git has nothing to say).
    blame: services::Blame,
    /// Keystroke-to-pixels timing (`MNOTE_LATENCY=1`): when the last key
    /// arrived, how long it waited for the display's next frame to start
    /// drawing, and each key's (total, wait) in milliseconds.
    latency: services::LatencyProbe,
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
    /// Watches ~/.config/mnote, so a theme file saved anywhere applies.
    _config_watcher: Option<mnote_core::DirWatcher>,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
    /// Editor events and their subscribers.
    events: events::Bus,
    /// The Lua plugin host, when `init.lua` loaded (see `plugins`), and
    /// whether loading was tried.
    plugins: Vec<plugin_list::Entry>,
    plugins_tried: bool,
    /// What the user allowed to run (plugins; later grammars and servers).
    pub(super) permissions: mnote_core::permissions::Permissions,
    /// Where language packs were last loaded from (reloaded when that
    /// changes).
    language_sources: Option<mnote_core::lang::languages::Sources>,
    /// Toasts and problems so far, for `:messages`.
    pub(super) messages: std::collections::VecDeque<String>,
    /// Fence languages whose grammar was looked for this session (loaded,
    /// asked about, or not found), and those waiting to be.
    grammars_seen: std::collections::HashSet<String>,
    /// When the code file shown was opened, until its first colours, and
    /// when its first frame of text was drawn.
    code_opened: Option<std::time::Instant>,
    code_opened_frame: Option<std::time::Instant>,
    /// The code file's colours per line.
    line_cache: code_files::LineCache,
    grammars_wanted: Vec<String>,
    /// Block renderers, tried in order (see `blocks`).
    blocks: Rc<Vec<(String, blocks::BlockRenderer)>>,
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
        editor.code_file_opened(cx);
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
        self.code_file_opened(cx);
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
        // The command registry (and with it the app's ex commands in Vim)
        // and its default key bindings; config.toml's `[keys]` come later.
        crate::commands::install(commands::builtin());
        crate::keymap::set(crate::keymap::Keymap::build(&crate::commands::all(), &[]).0);
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
            // The panes and tabs, with every cursor, are kept for next time.
            cx.on_app_quit(|this, _| {
                this.save_session();
                async {}
            }),
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
            // Every change: what it changed becomes events (see `events`).
            cx.observe_self(|this, _| {
                let now = this.snapshot();
                this.events.observe(now);
            }),
        ];

        let first_note = source.doc_key();
        let wraps: Rc<RefCell<std::collections::HashMap<usize, Vec<usize>>>> = Rc::default();
        vim.set_screen_rows(Box::new(WrapRows(wraps.clone())));
        let mut editor = Editor {
            events: events::Bus::default(),
            blocks: Rc::default(),
            plugins: Vec::new(),
            plugins_tried: false,
            permissions: plugin_list::load_permissions(),
            language_sources: None,
            messages: std::collections::VecDeque::new(),
            grammars_seen: Default::default(),
            code_opened: None,
            code_opened_frame: None,
            line_cache: Default::default(),
            grammars_wanted: Vec::new(),
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
            keyseq: Vec::new(),
            keyseq_task: None,
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
            panes: vec![PaneSlot::new(PaneId(0), first_note, None)],
            focused: PaneId(0),
            layout: Layout::Pane(PaneId(0)),
            next_pane: 1,
            parked: Vec::new(),
            closed_tabs: Vec::new(),
            tab_rows: Default::default(),
            opening: Default::default(),
            scrollbar_drag: None,
            scrollbar_bounds: Default::default(),
            zoom: 0,
            base_fonts: Default::default(),
            session_restored: false,
            session_shape_saved: String::new(),
            session_task: None,
            pane_menu: None,
            menu_anchors: Default::default(),
            next_untitled: 0,
            untitled_folders: Default::default(),
            edit_groups: Default::default(),
            drawing_other: false,
            drawing_pane: PaneId(0),
            drag_anchor: None,
            map_task: None,
            wraps,
            scroll_x: Rc::default(),
            vault: None,
            first_run: None,
            marked: None,
            words: services::WordCount::default(),
            blame: services::Blame::default(),
            latency: services::LatencyProbe::from_env(),
            config: Config::default(),
            config_path: Config::path(),
            _watcher: None,
            _config_watcher: None,
            _tasks: vec![redraw],
            _subscriptions: subscriptions,
        };
        editor.watch_themes(cx);
        editor.subscribe_builtin();
        editor.register_builtin_blocks();
        // The first snapshot opens the note (and counts its words).
        cx.notify();
        editor
    }

    // ---- input ------------------------------------------------------------

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
        self.code_file_opened(cx);
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
        // An older file gains the settings it doesn't have yet.
        let mut missing = None;
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
                missing = Config::missing_settings(&text);
                self.new_doc(Buffer::new(&text), Source::Config(path))
            }
        };
        self.show_doc(next);
        // Added as one change (u takes it back), saved like any edit. They're
        // at their defaults, so nothing changes until one is edited.
        if let Some((at, missing)) = missing {
            let rope = self.buffer.text();
            let end = rope.byte_to_char(at.min(rope.len_bytes()));
            let gap = if end == 0 || rope.char(end - 1) == '\n' {
                ""
            } else {
                "\n"
            };
            let added = missing
                .lines()
                .filter(|l| !l.starts_with('#') && l.contains(" = "))
                .count();
            self.vim
                .replace(&mut self.buffer, end..end, &format!("{gap}{missing}"));
            self.sync_lines();
            self.schedule_autosave(cx);
            let what = match added {
                0 => "the [keys] table".to_owned(),
                1 => "1 setting".to_owned(),
                n => format!("{n} settings"),
            };
            self.reply = Some((
                format!("Added {what} this file didn't have yet (u undoes)").into(),
                false,
            ));
        }
        cx.notify();
    }

    /// Opens `next` in the focused pane, as a tab of its own (ADR-0014).
    fn show_doc(&mut self, next: Doc) {
        // A new tab in the focused pane (or its tab for the note).
        self.open_in_tab(next);
    }

    /// Takes settings into use: the view and line numbers here, the theme,
    /// and (on macOS) the global keys. At startup and when config.toml is
    /// saved.
    pub fn apply_config(&mut self, config: Config, cx: &mut Context<Self>) {
        // Language packs, then plugins (which may add languages), so their
        // commands and keys join the keymap below.
        self.load_languages(&config);
        let mb = |n: u32| n as usize * 1024 * 1024;
        mnote_core::lang::syntax::set_limits(mnote_core::lang::syntax::Limits {
            injections: mb(config.large_file_injections_mb),
            highlight: mb(config.large_file_highlight_mb),
        });
        self.load_plugins(&config, cx);
        self.report_language_problems(cx);
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
        let wrap = if config.wrap {
            "set wrap"
        } else {
            "set nowrap"
        };
        let _ = self.vim.execute(wrap, &mut self.buffer);
        if let Err(err) = crate::theme::Theme::set_choice(&config.theme, cx) {
            self.notice = Some(err.into());
        }
        crate::commands::set_leader(config.leader);
        // Key bindings: the commands' defaults, then config.toml's [keys].
        let (keymap, problems) =
            crate::keymap::Keymap::build(&crate::commands::all(), &config.keys);
        crate::keymap::set(keymap);
        if let Some(first) = problems.first() {
            self.notice = Some(format!("config.toml keys: {first}").into());
        }
        self.vim.share_clipboard(config.system_clipboard);
        self.vim.set_wrap_motion(config.wrap_motion);
        self.vim.options_mut().scrolloff = config.scrolloff as usize;
        // config.toml's mappings replace the built-in ones (only mapping
        // commands; anything else is reported).
        if let Some(lines) = &config.mappings {
            self.vim.clear_mappings();
            let mut problems = Vec::new();
            for line in lines {
                let name = line.split_whitespace().next().unwrap_or("");
                if !mnote_core::config::MAPPING_COMMANDS.contains(&name) {
                    problems.push(format!("{line}: only mapping commands go here"));
                    continue;
                }
                if let Err(err) = self.vim.execute(line, &mut self.buffer) {
                    problems.push(format!("{line}: {err}"));
                }
            }
            if let Some(first) = problems.first() {
                self.notice = Some(format!("config.toml mappings: {first}").into());
            }
        }
        // Fonts: an installed one, else the bundled default (and say so).
        // Listing the system's fonts takes ~90 ms, so it's only done for a
        // font that isn't one of the bundled ones (the defaults).
        let bundled = crate::theme::fonts::is_bundled;
        let installed = if bundled(&config.font) && bundled(&config.ui_font) {
            Vec::new()
        } else {
            cx.text_system().all_font_names()
        };
        let mut missing = Vec::new();
        let mut pick = |want: &str, default: &'static str| {
            if bundled(want) || installed.iter().any(|n| n.eq_ignore_ascii_case(want)) {
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
        let panels = crate::theme::Panels {
            radius: config.panel_radius.map(|r| r as f32),
            shadow: config.panel_shadow,
        };
        if crate::theme::set_panels(panels) {
            cx.refresh_windows();
        }
        if !config.autosave {
            self.autosave_task = None;
        }
        self.base_fonts = fonts.clone();
        if crate::theme::set_fonts(fonts.zoomed(self.zoom)) {
            self.remeasure_all();
            cx.refresh_windows();
        }
        if !missing.is_empty() {
            self.notice =
                Some(format!("{} isn't installed; using IBM Plex", missing.join(" and ")).into());
        }
        let relist = config.show_code_files != self.config.show_code_files;
        self.config = config;
        if relist {
            self.build_index(cx);
        }
        self.emit(events::EditorEvent::ConfigApplied, cx);
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
        let files = self.config.show_code_files;
        let task = cx
            .background_executor()
            .spawn(async move { NoteIndex::build_with(&vault, files) });
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

    // ---- rendering ----------------------------------------------------------
}

/// A note's buffer; one that doesn't exist yet starts empty and is made on
/// the first save.
fn open_buffer(vault: &Vault, note: &NotePath) -> (Buffer, Option<SharedString>) {
    match Buffer::open(vault, note) {
        Ok(buffer) => (buffer, None),
        // Not made yet: empty, in its language (a note is Markdown).
        Err(_) if !vault.path_of(note).exists() => {
            let language = mnote_core::filetype::for_path(note.as_path()).and_then(|f| f.language);
            let buffer = match language {
                Some(language) => Buffer::with_language("", &language),
                None => Buffer::new_plain(""),
            };
            (buffer, None)
        }
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

/// A keystroke's key as one lower-case character (`k` for ⌘K).
fn single_key(key: &str) -> Option<char> {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c.to_ascii_lowercase()),
        _ => None,
    }
}

fn char_to_byte(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map_or(s.len(), |(b, _)| b)
}
