//! Split panes: how the window is divided ([`Layout`]) and the state a pane
//! or a note keeps while it isn't the focused one.
//!
//! The editor's own fields always hold the focused pane's note and view.
//! Each other open note waits as a [`Doc`], each other pane as a
//! [`PaneLocal`]; focusing a pane swaps them in. Two panes on one note share
//! the one [`Doc`] (one buffer, one undo history), like Vim's windows.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{ListState, SharedString, Task};
use mnote_core::{Autosave, Buffer, NotePath, Vault};

use super::styled::View;
use super::{CodeLines, TableLayout};

/// Where a note lives. A scratch note isn't saved.
pub(super) enum Source {
    Scratch,
    Note {
        vault: Vault,
        note: NotePath,
    },
    /// mNote's own `config.toml` (`space ,`), outside the vault.
    Config(std::path::PathBuf),
}

impl Source {
    /// The note, if this is one of the vault's.
    pub(super) fn key(&self) -> Option<&NotePath> {
        match self {
            Source::Note { note, .. } => Some(note),
            Source::Scratch | Source::Config(_) => None,
        }
    }

    /// Which document this is, for telling docs apart.
    pub(super) fn doc_key(&self) -> DocKey {
        match self {
            Source::Scratch => DocKey::Scratch,
            Source::Note { note, .. } => DocKey::Note(note.clone()),
            Source::Config(_) => DocKey::Config,
        }
    }
}

/// A document's identity: two panes with equal keys share one [`Doc`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocKey {
    Scratch,
    Note(NotePath),
    Config,
}

/// An open note that isn't the focused pane's: everything the editor keeps
/// per note.
pub(super) struct Doc {
    pub buffer: Buffer,
    pub source: Source,
    pub autosave: Autosave,
    pub autosave_task: Option<Task<()>>,
    pub notice: Option<SharedString>,
    pub tree: Option<mnote_core::SyntaxTree>,
    pub code_cache: HashMap<usize, (u64, Rc<CodeLines>)>,
    pub table_cache: HashMap<usize, (u64, TableLayout)>,
    pub image_paths: RefCell<HashMap<String, Option<std::path::PathBuf>>>,
}

impl Doc {
    pub(super) fn new(buffer: Buffer, source: Source) -> Self {
        Doc {
            buffer,
            source,
            autosave: Autosave::default(),
            autosave_task: None,
            notice: None,
            tree: None,
            code_cache: HashMap::new(),
            table_cache: HashMap::new(),
            image_paths: RefCell::default(),
        }
    }
}

/// A pane's own state while another pane has focus.
pub(super) struct PaneLocal {
    pub lines: ListState,
    pub view: View,
    /// Vim's cursor in this pane.
    pub cursor: usize,
    /// How far it's scrolled sideways (`nowrap`).
    pub scroll_x: Rc<std::cell::Cell<gpui::Pixels>>,
}

/// A pane: its note, and its state while it isn't focused.
pub(super) struct PaneSlot {
    pub id: PaneId,
    pub note: DocKey,
    /// `None` for the focused pane, whose state is the editor's.
    pub local: Option<PaneLocal>,
}

/// A pane's name in the [`Layout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(pub u32);

/// Which way a split divides its space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Side by side (`⌃w v`, `:vsplit`).
    Row,
    /// One above the other (`⌃w s`, `:split`).
    Column,
}

/// A direction to move between panes (`⌃w h/j/k/l`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Down,
    Up,
    Right,
}

impl Dir {
    fn axis(self) -> Axis {
        match self {
            Dir::Left | Dir::Right => Axis::Row,
            Dir::Up | Dir::Down => Axis::Column,
        }
    }

    fn forward(self) -> bool {
        matches!(self, Dir::Right | Dir::Down)
    }
}

/// How the window is divided into panes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    Pane(PaneId),
    /// Two or more parts, in order (left to right, or top to bottom).
    Split(Axis, Vec<Layout>),
}

impl Layout {
    /// Every pane, in reading order.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Layout::Pane(id) => out.push(*id),
            Layout::Split(_, parts) => parts.iter().for_each(|p| p.collect(out)),
        }
    }

    /// Puts `new` beside `at` (after it), dividing along `axis`.
    pub fn split(&mut self, at: PaneId, new: PaneId, axis: Axis) -> bool {
        match self {
            Layout::Pane(id) if *id == at => {
                *self = Layout::Split(axis, vec![Layout::Pane(at), Layout::Pane(new)]);
                true
            }
            Layout::Pane(_) => false,
            Layout::Split(own, parts) => {
                // A split along the same axis takes the new pane as a sibling.
                if *own == axis
                    && let Some(i) = parts.iter().position(|p| *p == Layout::Pane(at))
                {
                    parts.insert(i + 1, Layout::Pane(new));
                    return true;
                }
                parts.iter_mut().any(|p| p.split(at, new, axis))
            }
        }
    }

    /// Takes pane `id` out; a split left with one part becomes that part.
    /// The last pane can't be removed.
    pub fn remove(&mut self, id: PaneId) -> bool {
        let Layout::Split(_, parts) = self else {
            return false;
        };
        let removed = match parts.iter().position(|p| *p == Layout::Pane(id)) {
            Some(i) => {
                parts.remove(i);
                true
            }
            None => parts.iter_mut().any(|p| p.remove(id)),
        };
        if removed && parts.len() == 1 {
            *self = parts.remove(0);
        }
        removed
    }

    /// The pane next to `from` in direction `dir`, if there is one.
    pub fn neighbor(&self, from: PaneId, dir: Dir) -> Option<PaneId> {
        let mut path = Vec::new();
        if !self.path_to(from, &mut path) {
            return None;
        }
        // From the innermost split out: the first one along `dir`'s axis
        // with a part on that side.
        for depth in (0..path.len()).rev() {
            let node = self.at(&path[..depth]);
            let Layout::Split(axis, parts) = node else {
                continue;
            };
            if *axis != dir.axis() {
                continue;
            }
            let i = path[depth];
            let next = if dir.forward() {
                i + 1
            } else {
                i.checked_sub(1)?
            };
            if let Some(part) = parts.get(next) {
                return Some(part.edge(dir));
            }
        }
        None
    }

    /// The child indexes from the root down to pane `id`.
    fn path_to(&self, id: PaneId, path: &mut Vec<usize>) -> bool {
        match self {
            Layout::Pane(own) => *own == id,
            Layout::Split(_, parts) => {
                for (i, part) in parts.iter().enumerate() {
                    path.push(i);
                    if part.path_to(id, path) {
                        return true;
                    }
                    path.pop();
                }
                false
            }
        }
    }

    fn at(&self, path: &[usize]) -> &Layout {
        match (self, path.split_first()) {
            (Layout::Split(_, parts), Some((&i, rest))) => parts[i].at(rest),
            _ => self,
        }
    }

    /// The pane you arrive at entering this part going `dir`.
    fn edge(&self, dir: Dir) -> PaneId {
        match self {
            Layout::Pane(id) => *id,
            Layout::Split(axis, parts) => {
                let part = if *axis == dir.axis() && !dir.forward() {
                    parts.last()
                } else {
                    parts.first()
                };
                part.map_or(PaneId(0), |p| p.edge(dir))
            }
        }
    }
}

// ---- the editor's panes ---------------------------------------------------

use gpui::{AnyElement, Context, IntoElement, ListAlignment, px};
use mnote_vim::text::clamp_normal;

use super::Editor;

impl Editor {
    /// Swaps the focused note's state with `doc`'s.
    fn swap_doc(&mut self, doc: &mut Doc) {
        std::mem::swap(&mut self.buffer, &mut doc.buffer);
        std::mem::swap(&mut self.source, &mut doc.source);
        std::mem::swap(&mut self.autosave, &mut doc.autosave);
        std::mem::swap(&mut self.autosave_task, &mut doc.autosave_task);
        std::mem::swap(&mut self.notice, &mut doc.notice);
        std::mem::swap(&mut self.tree, &mut doc.tree);
        std::mem::swap(&mut self.code_cache, &mut doc.code_cache);
        std::mem::swap(&mut self.table_cache, &mut doc.table_cache);
        std::mem::swap(&mut self.image_paths, &mut doc.image_paths);
    }

    /// Swaps the focused pane's scroll, view and cursor with `local`'s.
    fn swap_local(&mut self, local: &mut PaneLocal) {
        std::mem::swap(&mut self.lines, &mut local.lines);
        std::mem::swap(&mut self.view, &mut local.view);
        std::mem::swap(&mut self.scroll_x, &mut local.scroll_x);
        let cursor = self.vim.cursor();
        self.vim.set_cursor(local.cursor);
        local.cursor = cursor;
    }

    fn slot(&self, id: PaneId) -> Option<usize> {
        self.panes.iter().position(|p| p.id == id)
    }

    pub(super) fn parked_index(&self, key: &DocKey) -> Option<usize> {
        self.parked.iter().position(|d| d.source.doc_key() == *key)
    }

    /// Whether a pane other than the focused one shows `note`.
    fn shown_elsewhere(&self, key: &DocKey) -> bool {
        self.panes
            .iter()
            .any(|p| p.id != self.focused && p.note == *key)
    }

    /// A new doc for `buffer`, redrawing when its background parse lands.
    pub(super) fn new_doc(&self, mut buffer: Buffer, source: Source) -> Doc {
        let tx = self.syntax_ready.clone();
        buffer.on_syntax_ready(move || {
            let _ = tx.try_send(());
        });
        Doc::new(buffer, source)
    }

    /// Records the focused pane's note (after it opened another).
    pub(super) fn set_pane_note(&mut self) {
        let note = self.source.doc_key();
        if let Some(i) = self.slot(self.focused) {
            self.panes[i].note = note;
        }
    }

    /// The focused pane's note goes away (it opens another): keep its doc
    /// if another pane still shows it.
    pub(super) fn park_or_drop(&mut self, mut old: Doc) {
        let key = old.source.doc_key();
        // A note in an edit group stays, so `u` elsewhere can reach it.
        if self.shown_elsewhere(&key) || self.edit_groups.contains(&key) {
            old.autosave_task = None;
            self.parked.push(old);
        }
    }

    /// Takes the open doc for `note` out of the parked ones, if there is one.
    pub(super) fn unpark(&mut self, key: &DocKey) -> Option<Doc> {
        let i = self.parked_index(key)?;
        Some(self.parked.swap_remove(i))
    }

    /// Moves the keyboard to pane `target`.
    pub(super) fn focus_pane(&mut self, target: PaneId, cx: &mut Context<Self>) {
        if target == self.focused {
            return;
        }
        let (Some(ti), Some(fi)) = (self.slot(target), self.slot(self.focused)) else {
            return;
        };
        let Some(mut local) = self.panes[ti].local.take() else {
            return;
        };
        let note = self.panes[ti].note.clone();
        if note != self.source.doc_key() {
            self.save_now(cx);
            self.autosave_task = None;
            if let Some(i) = self.parked_index(&note) {
                let mut doc = self.parked.swap_remove(i);
                self.swap_doc(&mut doc);
                self.parked.push(doc);
            }
        }
        self.swap_local(&mut local);
        self.panes[fi].local = Some(local);
        self.focused = target;
        self.wraps.borrow_mut().clear();
        let cursor = clamp_normal(self.buffer.text(), self.vim.cursor());
        self.vim.set_cursor(cursor);
        self.restart_blink(cx);
        cx.notify();
    }

    /// `⌃w v` / `⌃w s`: a new pane on the same note, beside or below, which
    /// takes the focus.
    pub(super) fn split_pane(&mut self, axis: Axis, cx: &mut Context<Self>) {
        let id = PaneId(self.next_pane);
        self.next_pane += 1;
        let lines = ListState::new(self.buffer.text().len_lines(), ListAlignment::Top, px(400.));
        lines.scroll_to(self.lines.logical_scroll_top());
        self.panes.push(PaneSlot {
            id,
            note: self.source.doc_key(),
            local: Some(PaneLocal {
                lines,
                view: self.view,
                cursor: self.vim.cursor(),
                scroll_x: Rc::new(std::cell::Cell::new(self.scroll_x.get())),
            }),
        });
        self.layout.split(self.focused, id, axis);
        self.focus_pane(id, cx);
    }

    /// `⌃w c`, `:close`, `:q` with panes: closes the focused pane. The last
    /// one stays (returns `false`).
    pub(super) fn close_pane(&mut self, cx: &mut Context<Self>) -> bool {
        let order = self.layout.panes();
        if order.len() < 2 {
            return false;
        }
        let closing = self.focused;
        let at = order.iter().position(|&p| p == closing).unwrap_or(0);
        let next = if at > 0 { order[at - 1] } else { order[1] };
        self.focus_pane(next, cx);
        self.layout.remove(closing);
        if let Some(i) = self.slot(closing) {
            let slot = self.panes.remove(i);
            // Its note's doc goes if no pane shows it any more.
            let still_shown = slot.note == self.source.doc_key()
                || self.panes.iter().any(|p| p.note == slot.note);
            if !still_shown
                && !self.edit_groups.contains(&slot.note)
                && let Some(d) = self.parked_index(&slot.note)
            {
                self.parked.swap_remove(d);
            }
        }
        cx.notify();
        true
    }

    /// `⌃w o`, `:only`: closes every pane but the focused one.
    pub(super) fn only_pane(&mut self, cx: &mut Context<Self>) {
        let focused = self.focused;
        self.panes.retain(|p| p.id == focused);
        self.layout = Layout::Pane(focused);
        // Parked docs were saved when they were parked; those in an edit
        // group stay for its undo.
        let groups = &self.edit_groups;
        self.parked.retain(|d| groups.contains(&d.source.doc_key()));
        cx.notify();
    }

    /// The key after `⌃w`.
    /// `⌃w h/j/k/l`: the pane that way, if there is one.
    pub(super) fn focus_dir(&mut self, dir: Dir, cx: &mut Context<Self>) {
        if let Some(next) = self.layout.neighbor(self.focused, dir) {
            self.focus_pane(next, cx);
        }
    }

    /// `⌃w w` / `⌃w W`: the next or previous pane, round the window.
    pub(super) fn cycle_pane(&mut self, step: isize, cx: &mut Context<Self>) {
        let order = self.layout.panes();
        let len = order.len() as isize;
        if len == 0 {
            return;
        }
        let at = order.iter().position(|&p| p == self.focused).unwrap_or(0) as isize;
        let next = (at + step).rem_euclid(len) as usize;
        self.focus_pane(order[next], cx);
    }

    /// After edits to the focused note: the same lines change in other
    /// panes on it.
    pub(super) fn splice_other_panes(&mut self, range: std::ops::Range<usize>, count: usize) {
        let note = self.source.doc_key();
        for slot in &mut self.panes {
            if slot.note == note
                && let Some(local) = &mut slot.local
            {
                local.lines.splice(range.clone(), count);
            }
        }
    }

    /// After a parked note's text was replaced: its panes measure again.
    fn reset_panes_on(&mut self, key: &DocKey, lines: usize) {
        for slot in &mut self.panes {
            if slot.note == *key
                && let Some(local) = &mut slot.local
            {
                local.lines.reset(lines);
                local.cursor = 0;
            }
        }
    }

    /// Runs `f` with the parked doc for `note` swapped in as the focused
    /// note (the panes stay as they are), then parks it again.
    pub(super) fn with_parked<R>(
        &mut self,
        note: &NotePath,
        f: impl FnOnce(&mut Self) -> R,
    ) -> Option<R> {
        let key = DocKey::Note(note.clone());
        let i = self.parked_index(&key)?;
        let mut doc = self.parked.swap_remove(i);
        self.swap_doc(&mut doc);
        let cursor = self.vim.cursor();
        let out = f(self);
        self.vim.set_cursor(cursor);
        // Line changes were the parked panes', not the focused pane's.
        let _ = self.buffer.take_changed_lines();
        let lines = self.buffer.text().len_lines();
        self.swap_doc(&mut doc);
        self.parked.push(doc);
        self.reset_panes_on(&key, lines);
        Some(out)
    }

    /// Open notes other than the focused one, for file events.
    pub(super) fn parked_notes(&self) -> Vec<NotePath> {
        self.parked
            .iter()
            .filter_map(|d| d.source.key().cloned())
            .collect()
    }

    /// Refreshes the parked docs' syntax trees for drawing.
    pub(super) fn refresh_parked_trees(&mut self) {
        for doc in &mut self.parked {
            let tree = doc.buffer.syntax_tree().cloned();
            mnote_core::drop_tree_later(std::mem::replace(&mut doc.tree, tree));
        }
    }

    /// Draws line `ix` of unfocused pane `id`, by swapping its note and
    /// view in for the moment.
    pub(super) fn render_other(
        &mut self,
        id: PaneId,
        ix: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let blank = || gpui::div().into_any_element();
        let Some(si) = self.slot(id) else {
            return blank();
        };
        let Some(mut local) = self.panes[si].local.take() else {
            return blank();
        };
        let note = self.panes[si].note.clone();
        let mut doc = match note != self.source.doc_key() {
            true => match self.parked_index(&note) {
                Some(i) => Some(self.parked.swap_remove(i)),
                None => {
                    self.panes[si].local = Some(local);
                    return blank();
                }
            },
            false => None,
        };
        if let Some(doc) = &mut doc {
            self.swap_doc(doc);
        }
        self.swap_local(&mut local);
        self.drawing_other = true;
        self.drawing_pane = id;
        let element = self.render_line(ix, cx);
        self.drawing_other = false;
        self.swap_local(&mut local);
        if let Some(mut doc) = doc {
            self.swap_doc(&mut doc);
            self.parked.push(doc);
        }
        self.panes[si].local = Some(local);
        element
    }

    /// Every pane measures its lines again (the fonts changed), keeping
    /// the cursor in view.
    pub(super) fn remeasure_all(&mut self) {
        self.lines.reset(self.buffer.text().len_lines());
        let line = mnote_vim::text::line_of(self.buffer.text(), self.vim.cursor());
        self.lines.scroll_to_reveal_item(line);
        let live = self.source.doc_key();
        let live_lines = self.buffer.text().len_lines();
        let counts: Vec<(DocKey, usize)> = self
            .parked
            .iter()
            .map(|d| (d.source.doc_key(), d.buffer.text().len_lines()))
            .collect();
        for slot in &mut self.panes {
            let Some(local) = &mut slot.local else {
                continue;
            };
            let lines = if slot.note == live {
                Some(live_lines)
            } else {
                counts
                    .iter()
                    .find(|(k, _)| *k == slot.note)
                    .map(|(_, n)| *n)
            };
            if let Some(lines) = lines {
                local.lines.reset(lines);
            }
        }
    }

    /// The other panes' file names, for the status line.
    pub(super) fn other_pane_files(&self) -> Vec<String> {
        self.layout
            .panes()
            .into_iter()
            .filter(|&id| id != self.focused)
            .filter_map(|id| self.slot(id))
            .map(|i| match &self.panes[i].note {
                DocKey::Note(note) => note.file_name().to_owned(),
                DocKey::Scratch => "scratch".to_owned(),
                DocKey::Config => "config.toml".to_owned(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: PaneId = PaneId(0);
    const B: PaneId = PaneId(1);
    const C: PaneId = PaneId(2);
    const D: PaneId = PaneId(3);

    #[test]
    fn splits_nest_and_same_axis_splits_stay_flat() {
        let mut layout = Layout::Pane(A);
        assert!(layout.split(A, B, Axis::Row));
        assert!(layout.split(B, C, Axis::Row));
        assert_eq!(
            layout,
            Layout::Split(
                Axis::Row,
                vec![Layout::Pane(A), Layout::Pane(B), Layout::Pane(C)]
            )
        );
        assert!(layout.split(A, D, Axis::Column));
        assert_eq!(layout.panes(), [A, D, B, C]);
    }

    #[test]
    fn moving_between_panes() {
        // A | (B over C)
        let mut layout = Layout::Pane(A);
        layout.split(A, B, Axis::Row);
        layout.split(B, C, Axis::Column);
        assert_eq!(layout.neighbor(A, Dir::Right), Some(B));
        assert_eq!(layout.neighbor(C, Dir::Left), Some(A));
        assert_eq!(layout.neighbor(B, Dir::Down), Some(C));
        assert_eq!(layout.neighbor(C, Dir::Up), Some(B));
        assert_eq!(layout.neighbor(A, Dir::Left), None);
        assert_eq!(layout.neighbor(B, Dir::Up), None);
    }

    #[test]
    fn removing_collapses_single_part_splits() {
        let mut layout = Layout::Pane(A);
        layout.split(A, B, Axis::Row);
        layout.split(B, C, Axis::Column);
        assert!(layout.remove(C));
        assert_eq!(
            layout,
            Layout::Split(Axis::Row, vec![Layout::Pane(A), Layout::Pane(B)])
        );
        assert!(layout.remove(A));
        assert_eq!(layout, Layout::Pane(B));
        assert!(!layout.remove(B), "the last pane stays");
    }
}
