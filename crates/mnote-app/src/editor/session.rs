//! Restoring panes and tabs (pane-tabs design, AMI-223): mNote reopens a
//! vault as it was left — the panes, each pane's tabs in order with their
//! cursor, view and preview flag, which tab each pane shows, and which pane
//! has focus. Untitled notes come back from their drafts.
//!
//! The session is kept per vault in mNote's config folder
//! (`sessions/{hash}.toml`), not in the vault, so notes synced with others
//! don't carry it. It's written a second after the panes or tabs change, and
//! when mNote quits. Only the notes the panes show are read at start; the
//! other tabs are dormant until shown, so a large session doesn't slow the
//! start. A missing note is skipped, a pane left with no tabs goes, and a
//! file that can't be read (or is from a newer mNote) is ignored.

use std::path::{Path, PathBuf};

use gpui::{Context, ListAlignment, ListState, px};
use mnote_core::config::Config;
use mnote_core::{Buffer, NotePath};

use super::Editor;
use super::panes::{Axis, DocKey, Layout, PaneId, PaneLocal, PaneSlot, Source};
use super::styled::View;
use super::tabs::Tab;

/// The session file's format; a newer one is ignored.
const VERSION: i64 = 1;

/// One tab as the session keeps it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SavedTab {
    pub what: SavedDoc,
    pub row: usize,
    pub col: usize,
    pub view: View,
    pub preview: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum SavedDoc {
    Note(String),
    /// An untitled note's draft file.
    Draft(PathBuf),
}

/// One pane: its tabs and which it shows.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SavedPane {
    pub tabs: Vec<SavedTab>,
    pub active: usize,
}

/// A whole session: the layout (over pane indexes), the panes in reading
/// order, and the focused one.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Session {
    pub layout: SavedLayout,
    pub panes: Vec<SavedPane>,
    pub focused: usize,
}

/// A layout with panes named by their index in [`Session::panes`].
#[derive(Debug, Clone, PartialEq)]
pub(super) enum SavedLayout {
    Pane(usize),
    Split(Axis, Vec<SavedLayout>),
}

impl SavedLayout {
    fn write(&self, out: &mut String) {
        match self {
            SavedLayout::Pane(i) => out.push_str(&i.to_string()),
            SavedLayout::Split(axis, parts) => {
                out.push_str(if *axis == Axis::Row { "row(" } else { "col(" });
                for (n, part) in parts.iter().enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    part.write(out);
                }
                out.push(')');
            }
        }
    }

    /// Reads `row(0,col(1,2))`.
    fn parse(text: &str) -> Option<SavedLayout> {
        let (layout, rest) = Self::parse_part(text.trim())?;
        rest.trim().is_empty().then_some(layout)
    }

    fn parse_part(text: &str) -> Option<(SavedLayout, &str)> {
        let text = text.trim_start();
        let axis = if let Some(rest) = text.strip_prefix("row(") {
            Some((Axis::Row, rest))
        } else {
            text.strip_prefix("col(").map(|rest| (Axis::Column, rest))
        };
        if let Some((axis, mut rest)) = axis {
            let mut parts = Vec::new();
            loop {
                let (part, after) = Self::parse_part(rest)?;
                parts.push(part);
                let after = after.trim_start();
                if let Some(after) = after.strip_prefix(',') {
                    rest = after;
                    continue;
                }
                let after = after.strip_prefix(')')?;
                return (parts.len() > 1).then_some((SavedLayout::Split(axis, parts), after));
            }
        }
        let digits = text.chars().take_while(char::is_ascii_digit).count();
        let n = text[..digits].parse().ok()?;
        Some((SavedLayout::Pane(n), &text[digits..]))
    }

    /// Drops the panes `keep` says no to; a split left with one part
    /// becomes that part. `None` when nothing is left.
    fn retain(self, keep: &dyn Fn(usize) -> bool) -> Option<SavedLayout> {
        match self {
            SavedLayout::Pane(i) => keep(i).then_some(SavedLayout::Pane(i)),
            SavedLayout::Split(axis, parts) => {
                let mut parts: Vec<SavedLayout> =
                    parts.into_iter().filter_map(|p| p.retain(keep)).collect();
                match parts.len() {
                    0 => None,
                    1 => parts.pop(),
                    _ => Some(SavedLayout::Split(axis, parts)),
                }
            }
        }
    }

    fn indexes(&self, out: &mut Vec<usize>) {
        match self {
            SavedLayout::Pane(i) => out.push(*i),
            SavedLayout::Split(_, parts) => parts.iter().for_each(|p| p.indexes(out)),
        }
    }
}

/// A TOML basic string.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl Session {
    /// The session as TOML.
    pub fn to_toml(&self) -> String {
        let mut layout = String::new();
        self.layout.write(&mut layout);
        let mut out = format!(
            "# mNote's panes and tabs for this vault, as they were left (mNote writes it).\nversion = {VERSION}\nfocused = {}\nlayout = {}\n",
            self.focused,
            quoted(&layout)
        );
        for pane in &self.panes {
            out.push_str(&format!("\n[[pane]]\nactive = {}\n", pane.active));
            for tab in &pane.tabs {
                out.push_str("\n[[pane.tab]]\n");
                match &tab.what {
                    SavedDoc::Note(note) => out.push_str(&format!("note = {}\n", quoted(note))),
                    SavedDoc::Draft(path) => {
                        out.push_str(&format!("draft = {}\n", quoted(&path.to_string_lossy())))
                    }
                }
                out.push_str(&format!(
                    "row = {}\ncol = {}\nview = {}\npreview = {}\n",
                    tab.row,
                    tab.col,
                    quoted(tab.view.name()),
                    tab.preview
                ));
            }
        }
        out
    }

    /// Reads a session; `None` for anything off (a bad file, a newer
    /// version, a layout that doesn't match its panes).
    pub fn parse(text: &str) -> Option<Session> {
        let table: toml::Table = text.parse().ok()?;
        if table.get("version")?.as_integer()? != VERSION {
            return None;
        }
        let focused = usize::try_from(table.get("focused")?.as_integer()?).ok()?;
        let layout = SavedLayout::parse(table.get("layout")?.as_str()?)?;
        let mut panes = Vec::new();
        for pane in table.get("pane")?.as_array()? {
            let pane = pane.as_table()?;
            let active = usize::try_from(pane.get("active")?.as_integer()?).ok()?;
            let mut tabs = Vec::new();
            for tab in pane
                .get("tab")
                .and_then(|t| t.as_array())
                .into_iter()
                .flatten()
            {
                let tab = tab.as_table()?;
                let what = match (tab.get("note"), tab.get("draft")) {
                    (Some(note), _) => SavedDoc::Note(note.as_str()?.to_owned()),
                    (None, Some(draft)) => SavedDoc::Draft(PathBuf::from(draft.as_str()?)),
                    (None, None) => return None,
                };
                let num = |key: &str| {
                    tab.get(key)
                        .and_then(|v| v.as_integer())
                        .and_then(|n| usize::try_from(n).ok())
                        .unwrap_or(0)
                };
                tabs.push(SavedTab {
                    what,
                    row: num("row"),
                    col: num("col"),
                    view: tab
                        .get("view")
                        .and_then(|v| v.as_str())
                        .and_then(View::from_name)
                        .unwrap_or_default(),
                    preview: tab
                        .get("preview")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                });
            }
            panes.push(SavedPane { tabs, active });
        }
        // Every pane the layout names, and only those, once each.
        let mut named = Vec::new();
        layout.indexes(&mut named);
        let mut sorted = named.clone();
        sorted.sort_unstable();
        sorted.dedup();
        if sorted.len() != named.len() || sorted != (0..panes.len()).collect::<Vec<_>>() {
            return None;
        }
        (focused < panes.len()).then_some(Session {
            layout,
            panes,
            focused,
        })
    }
}

/// FNV-1a: a hash that stays the same between runs and builds, for the
/// session's file name.
fn stable_hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Where `vault`'s session is kept.
pub(super) fn session_path(vault_root: &Path) -> Option<PathBuf> {
    let root = vault_root
        .canonicalize()
        .unwrap_or_else(|_| vault_root.to_path_buf());
    let name = format!("{:016x}.toml", stable_hash(&root.to_string_lossy()));
    Some(Config::path()?.parent()?.join("sessions").join(name))
}

/// A character's row and column in `rope`.
fn row_col(rope: &mnote_core::Rope, at: usize) -> (usize, usize) {
    let at = at.min(rope.len_chars());
    let row = rope.char_to_line(at);
    (row, at - rope.line_to_char(row))
}

/// The character at `row`, `col` of `rope`, clamped into the text.
pub(super) fn char_at(rope: &mnote_core::Rope, row: usize, col: usize) -> usize {
    let rows = rope.len_lines();
    let row = row.min(rows.saturating_sub(1));
    let start = rope.line_to_char(row);
    let len = mnote_vim::text::line_len(rope, row);
    start + col.min(len)
}

impl Editor {
    /// The panes and tabs now, as a session. Untitled notes with text get
    /// their drafts written, so the session can name them.
    fn current_session(&mut self) -> Option<Session> {
        self.vault.as_ref()?;
        let order = self.layout.panes();
        let index_of = |id: PaneId| order.iter().position(|&p| p == id);
        let layout = saved_layout(&self.layout, &index_of)?;
        let mut panes = Vec::new();
        for &id in &order {
            let slot = self.panes.iter().find(|p| p.id == id)?;
            let mut tabs = Vec::new();
            let mut active = 0;
            for (i, tab) in slot.tabs.iter().enumerate() {
                // Where its cursor is: the editor's, the pane's, the tab's,
                // or (a tab never shown) where it was restored to.
                let cursor = if id == self.focused && i == slot.active {
                    Some((self.vim.cursor(), self.view))
                } else if i == slot.active {
                    slot.local.as_ref().map(|l| (l.cursor, l.view))
                } else {
                    tab.local.as_ref().map(|l| (l.cursor, l.view))
                };
                let (row, col, view) = match (cursor, &tab.restore) {
                    (Some((at, view)), _) => match self.text_of(&tab.doc) {
                        Some(rope) => {
                            let (r, c) = row_col(rope, at);
                            (r, c, view)
                        }
                        None => (0, 0, view),
                    },
                    (None, Some((r, c, view))) => (*r, *c, *view),
                    (None, None) => (0, 0, View::default()),
                };
                let what = match &tab.doc {
                    DocKey::Note(note) => SavedDoc::Note(note.to_string()),
                    DocKey::Untitled(n) => match self.keep_draft(*n) {
                        Some(path) => SavedDoc::Draft(path),
                        None => continue,
                    },
                    DocKey::Scratch | DocKey::Config => continue,
                };
                if i == slot.active {
                    active = tabs.len();
                }
                tabs.push(SavedTab {
                    what,
                    row,
                    col,
                    view,
                    preview: tab.preview,
                });
            }
            panes.push(SavedPane { tabs, active });
        }
        // Panes left with nothing to keep (a scratch note) go.
        let keep: Vec<bool> = panes.iter().map(|p| !p.tabs.is_empty()).collect();
        let layout = layout.retain(&|i| keep[i])?;
        let mut renumber = vec![0; panes.len()];
        let mut n = 0;
        for (i, kept) in keep.iter().enumerate() {
            renumber[i] = n;
            if *kept {
                n += 1;
            }
        }
        let layout = renumbered(layout, &renumber);
        let focused_index = order.iter().position(|&p| p == self.focused).unwrap_or(0);
        let focused = if keep.get(focused_index) == Some(&true) {
            renumber[focused_index]
        } else {
            0
        };
        let panes: Vec<SavedPane> = panes.into_iter().filter(|p| !p.tabs.is_empty()).collect();
        Some(Session {
            layout,
            panes,
            focused,
        })
    }

    /// The text of `key` if it's open.
    fn text_of(&self, key: &DocKey) -> Option<&mnote_core::Rope> {
        if self.source.doc_key() == *key {
            return Some(self.buffer.text());
        }
        let i = self.parked_index(key)?;
        Some(self.parked[i].buffer.text())
    }

    /// Writes untitled note `n`'s draft now (it has text), returning where;
    /// `None` for one with no text.
    fn keep_draft(&self, n: u32) -> Option<PathBuf> {
        let rope = self.text_of(&DocKey::Untitled(n))?;
        if rope.len_chars() == 0 {
            return None;
        }
        let path = Self::draft_path(n)?;
        mnote_core::config::write_file(&path, &rope.to_string()).ok()?;
        Some(path)
    }

    /// Writes the session now (`restore_session` on, a vault open).
    pub(super) fn save_session(&mut self) {
        if !self.config.restore_session {
            return;
        }
        let Some(path) = self.vault.as_ref().and_then(|v| session_path(v.root())) else {
            return;
        };
        let Some(session) = self.current_session() else {
            return;
        };
        let _ = mnote_core::config::write_file(&path, &session.to_toml());
    }

    /// The panes and tabs as they stand (not cursors): when it changes, the
    /// session is written a second later.
    fn session_shape(&self) -> String {
        let mut out = format!("{:?}|{:?}", self.layout, self.focused);
        for slot in &self.panes {
            out.push_str(&format!("|{:?}:{}", slot.id, slot.active));
            for tab in &slot.tabs {
                out.push_str(&format!(",{:?}{}", tab.doc, tab.preview));
            }
        }
        out
    }

    /// Each frame: if the panes or tabs changed, the session is written a
    /// second from now (changes meanwhile push it back).
    pub(super) fn schedule_session_save(&mut self, cx: &mut Context<Self>) {
        if !self.config.restore_session || self.vault.is_none() || !self.session_restored {
            return;
        }
        let shape = self.session_shape();
        if shape == self.session_shape_saved {
            return;
        }
        self.session_shape_saved = shape;
        self.session_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(1))
                .await;
            let _ = this.update(cx, |this, _| this.save_session());
        }));
    }

    /// At start: reopens the vault's panes and tabs as they were left.
    /// `opened` is a note named on the command line, opened again as a kept
    /// tab in the focused pane afterwards.
    pub fn restore_session(&mut self, opened: Option<NotePath>, cx: &mut Context<Self>) {
        self.session_restored = true;
        if !self.config.restore_session {
            return;
        }
        let started = std::time::Instant::now();
        let Some(vault) = self.vault.clone() else {
            return;
        };
        let Some(session) = session_path(vault.root())
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| Session::parse(&text))
        else {
            return;
        };
        // Each saved tab as a tab now; notes that are gone are skipped.
        let mut panes: Vec<Option<(Vec<Tab>, usize)>> = Vec::new();
        for saved in &session.panes {
            let mut tabs = Vec::new();
            let mut active = 0;
            for (i, t) in saved.tabs.iter().enumerate() {
                let doc = match &t.what {
                    SavedDoc::Note(path) => {
                        let Some(note) = NotePath::new(path)
                            .ok()
                            .or_else(|| NotePath::file(path).ok())
                            .filter(|n| vault.path_of(n).exists())
                        else {
                            continue;
                        };
                        DocKey::Note(note)
                    }
                    SavedDoc::Draft(path) => match self.untitled_from_draft(path) {
                        Some(key) => key,
                        None => continue,
                    },
                };
                if i == saved.active {
                    active = tabs.len();
                }
                let mut tab = Tab::new(doc);
                tab.preview = t.preview;
                tab.restore = Some((t.row, t.col, t.view));
                tabs.push(tab);
            }
            panes.push((!tabs.is_empty()).then(|| {
                let active = active.min(tabs.len() - 1);
                (tabs, active)
            }));
        }
        let Some(layout) = session
            .layout
            .clone()
            .retain(&|i| panes.get(i).is_some_and(Option::is_some))
        else {
            return;
        };
        // New pane ids, the layout over them, and the focused one.
        let mut ids = vec![PaneId(0); panes.len()];
        for (i, pane) in panes.iter().enumerate() {
            if pane.is_some() {
                ids[i] = PaneId(self.next_pane);
                self.next_pane += 1;
            }
        }
        let layout = to_layout(&layout, &ids);
        let focused_ix = if panes.get(session.focused).is_some_and(Option::is_some) {
            session.focused
        } else {
            let mut first = Vec::new();
            session.layout.indexes(&mut first);
            first.into_iter().find(|&i| panes[i].is_some()).unwrap_or(0)
        };
        // The note shown now makes way: the editor holds a placeholder.
        self.save_now(cx);
        let mut placeholder = super::panes::Doc::new(Buffer::new_plain(""), Source::Scratch);
        self.swap_doc(&mut placeholder);
        placeholder.autosave_task = None;
        self.parked.push(placeholder);
        let mut slots = Vec::new();
        for (i, pane) in panes.into_iter().enumerate() {
            let Some((tabs, active)) = pane else {
                continue;
            };
            let note = tabs[active].doc.clone();
            let mut slot = PaneSlot::new(ids[i], note.clone(), None);
            slot.tabs = tabs;
            slot.active = active;
            slot.used = vec![note];
            slots.push(slot);
        }
        self.layout = layout;
        self.panes = slots;
        self.focused = ids[focused_ix];
        // The shown tabs' notes are read now; the others wait until shown.
        for si in 0..self.panes.len() {
            let id = self.panes[si].id;
            let active = self.panes[si].active;
            let key = self.panes[si].tabs[active].doc.clone();
            let restore = self.panes[si].tabs[active].restore.take();
            if !self.ensure_parked(&key) {
                continue;
            }
            if id == self.focused {
                if let Some(mut doc) = self.unpark(&key) {
                    self.swap_doc(&mut doc);
                }
                self.lines.reset(self.buffer.text().len_lines());
                if let Some((row, col, view)) = restore {
                    self.view = view;
                    let at = char_at(self.buffer.text(), row, col);
                    self.vim.set_cursor(at);
                    self.lines
                        .scroll_to_reveal_item(row.min(self.buffer.text().len_lines()));
                }
            } else {
                let text = self.text_of(&key).cloned().unwrap_or_default();
                let (row, col, view) = restore.unwrap_or((0, 0, self.view));
                let lines = ListState::new(text.len_lines(), ListAlignment::Top, px(400.));
                lines.scroll_to_reveal_item(row);
                self.panes[si].local = Some(PaneLocal {
                    lines,
                    view,
                    cursor: char_at(&text, row, col),
                    scroll_x: Default::default(),
                });
            }
        }
        self.drop_unshown_docs();
        if let Some(note) = opened
            && self.source.key() != Some(&note)
        {
            self.switch_to(note, cx);
        }
        self.session_shape_saved = self.session_shape();
        self.wraps.borrow_mut().clear();
        if std::env::var_os("MNOTE_LATENCY").is_some() {
            eprintln!(
                "mnote restore: {:.1} ms",
                started.elapsed().as_secs_f32() * 1000.
            );
        }
        cx.notify();
    }

    /// An untitled note with the text of the draft at `path` (the old draft
    /// goes; the note keeps a draft of its own). `None` if it's gone.
    fn untitled_from_draft(&mut self, path: &Path) -> Option<DocKey> {
        let text = std::fs::read_to_string(path).ok()?;
        let _ = std::fs::remove_file(path);
        self.next_untitled += 1;
        let n = self.next_untitled;
        self.untitled_folders.insert(n, String::new());
        // Its text isn't in a file: an unsaved change, as it was.
        let mut buffer = Buffer::new("");
        let cursor = self.vim.cursor();
        self.vim.replace(&mut buffer, 0..0, &text);
        self.vim.set_cursor(cursor);
        let doc = self.new_doc(buffer, Source::Untitled(n));
        self.parked.push(doc);
        Some(DocKey::Untitled(n))
    }
}

/// A layout's panes as indexes in reading order.
fn saved_layout(
    layout: &Layout,
    index_of: &dyn Fn(PaneId) -> Option<usize>,
) -> Option<SavedLayout> {
    Some(match layout {
        Layout::Pane(id) => SavedLayout::Pane(index_of(*id)?),
        Layout::Split(axis, parts) => SavedLayout::Split(
            *axis,
            parts
                .iter()
                .map(|p| saved_layout(p, index_of))
                .collect::<Option<_>>()?,
        ),
    })
}

fn renumbered(layout: SavedLayout, to: &[usize]) -> SavedLayout {
    match layout {
        SavedLayout::Pane(i) => SavedLayout::Pane(to[i]),
        SavedLayout::Split(axis, parts) => {
            SavedLayout::Split(axis, parts.into_iter().map(|p| renumbered(p, to)).collect())
        }
    }
}

fn to_layout(layout: &SavedLayout, ids: &[PaneId]) -> Layout {
    match layout {
        SavedLayout::Pane(i) => Layout::Pane(ids[*i]),
        SavedLayout::Split(axis, parts) => {
            Layout::Split(*axis, parts.iter().map(|p| to_layout(p, ids)).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(note: &str, row: usize) -> SavedTab {
        SavedTab {
            what: SavedDoc::Note(note.to_owned()),
            row,
            col: 2,
            view: View::default(),
            preview: false,
        }
    }

    fn sample() -> Session {
        Session {
            layout: SavedLayout::Split(
                Axis::Row,
                vec![
                    SavedLayout::Pane(0),
                    SavedLayout::Split(
                        Axis::Column,
                        vec![SavedLayout::Pane(1), SavedLayout::Pane(2)],
                    ),
                ],
            ),
            panes: vec![
                SavedPane {
                    tabs: vec![tab("a.md", 3), tab("notes/\"odd\" name.md", 0)],
                    active: 1,
                },
                SavedPane {
                    tabs: vec![SavedTab {
                        what: SavedDoc::Draft(PathBuf::from("/tmp/untitled-1-1.md")),
                        row: 1,
                        col: 0,
                        view: View::default(),
                        preview: true,
                    }],
                    active: 0,
                },
                SavedPane {
                    tabs: vec![tab("src/main.rs", 7)],
                    active: 0,
                },
            ],
            focused: 2,
        }
    }

    #[test]
    fn sessions_round_trip() {
        let session = sample();
        let text = session.to_toml();
        assert!(text.contains("layout = \"row(0,col(1,2))\""), "{text}");
        assert_eq!(Session::parse(&text), Some(session));
    }

    #[test]
    fn bad_and_newer_files_are_ignored() {
        assert_eq!(Session::parse("not toml ["), None);
        assert_eq!(Session::parse(""), None);
        let text = sample().to_toml();
        assert_eq!(
            Session::parse(&text.replace("version = 1", "version = 2")),
            None
        );
        // A layout that doesn't name every pane once.
        assert_eq!(
            Session::parse(&text.replace("row(0,col(1,2))", "row(0,col(1,1))")),
            None
        );
        assert_eq!(
            Session::parse(&text.replace("focused = 2", "focused = 9")),
            None
        );
    }

    #[test]
    fn layouts_drop_panes_and_collapse() {
        let layout = SavedLayout::parse("row(0,col(1,2))").expect("valid");
        assert_eq!(
            layout.clone().retain(&|i| i != 1),
            Some(SavedLayout::Split(
                Axis::Row,
                vec![SavedLayout::Pane(0), SavedLayout::Pane(2)]
            ))
        );
        assert_eq!(
            layout.clone().retain(&|i| i == 2),
            Some(SavedLayout::Pane(2))
        );
        assert_eq!(layout.retain(&|_| false), None);
        assert_eq!(SavedLayout::parse("row(0)"), None, "a split has 2+ parts");
        assert_eq!(SavedLayout::parse("row(0,1"), None);
    }

    #[test]
    fn hashes_stay_the_same() {
        assert_eq!(stable_hash(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(stable_hash("/notes"), stable_hash("/notes"));
        assert_ne!(stable_hash("/notes"), stable_hash("/notes2"));
    }

    #[test]
    fn cursors_are_rows_and_columns() {
        let rope = mnote_core::Rope::from_str("one\ntwo\nthree\n");
        assert_eq!(row_col(&rope, 5), (1, 1));
        assert_eq!(char_at(&rope, 1, 1), 5);
        assert_eq!(char_at(&rope, 1, 99), 7, "clamped to the line");
        assert_eq!(char_at(&rope, 99, 0), rope.line_to_char(3));
    }
}
