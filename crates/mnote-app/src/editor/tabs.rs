//! Tabs on every pane (ADR-0014): each pane holds several open notes, shows
//! one, and remembers the cursor, scroll and view of each. Notes are shared
//! by every tab and pane, as they were by panes: one note in two places is
//! one buffer with one undo history.
//!
//! The editor's fields hold the focused pane's active tab. A pane's other
//! tabs wait as [`Tab`]s with their [`PaneLocal`]; switching tabs parks the
//! shown note and swaps the chosen one in, as focusing a pane does. Tab
//! commands (`:tabnew`, `gt`, `⌘W` …) act on the focused pane's tabs.

use gpui::{Context, ListAlignment, ListOffset, ListState, px};
use mnote_core::{Buffer, NotePath};
use mnote_vim::text::{clamp_normal, line_of};

use super::Editor;
use super::panes::{Axis, Doc, DocKey, PaneId, PaneLocal, PaneSlot, Source};

/// Closed tabs kept for `:tabreopen`.
const CLOSED_KEPT: usize = 10;

/// A note in a pane. The active tab's state is the pane's (the editor's,
/// when it's the focused pane); another tab keeps its own here.
pub(super) struct Tab {
    pub doc: DocKey,
    /// Only looked at (italics): the next looked-at note takes its place,
    /// and editing or keeping it makes it a kept tab.
    pub preview: bool,
    /// Its state while it isn't the active tab.
    pub local: Option<PaneLocal>,
    /// The note's revision when it was last shown: a tab on a note edited
    /// meanwhile measures its lines again.
    pub seen: u64,
}

impl Tab {
    pub(super) fn new(doc: DocKey) -> Self {
        Tab {
            doc,
            preview: false,
            local: None,
            seen: 0,
        }
    }
}

/// How a note opened in the focused pane takes its place (design: Opening
/// notes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum OpenAs {
    /// A tab of its own, kept (the finder, `:e`, a new note).
    #[default]
    Kept,
    /// The pane's preview tab, replacing the last one (the explorer,
    /// search results, backlinks, `gd`).
    Preview,
    /// In place of the active tab (a pane just split, which opens on it).
    Replace,
}

/// A closed tab, for `:tabreopen`: its pane, its place, and its state.
pub(super) struct ClosedTab {
    pub pane: PaneId,
    pub index: usize,
    pub tab: Tab,
}

/// Where `:tabmove {arg}` puts tab `at` of `len`: no arg is last, `0`
/// first, `N` after tab N, `+N` / `-N` that many places along.
pub(super) fn move_target(at: usize, len: usize, arg: Option<&str>) -> Option<usize> {
    let last = len.checked_sub(1)?;
    let arg = arg.map(str::trim).filter(|a| !a.is_empty());
    let Some(arg) = arg else {
        return Some(last);
    };
    let steps = |n: &str| {
        if n.is_empty() {
            Some(1)
        } else {
            n.parse::<usize>().ok()
        }
    };
    if let Some(n) = arg.strip_prefix('+') {
        return Some((at + steps(n)?).min(last));
    }
    if let Some(n) = arg.strip_prefix('-') {
        return Some(at.saturating_sub(steps(n)?));
    }
    let n = arg.parse::<usize>().ok()?;
    if n == 0 {
        return Some(0);
    }
    // After tab N as numbered now: one place less once this tab is out of
    // the way, if it was before N.
    let n = n.min(len);
    Some(if at < n { n - 1 } else { n })
}

/// The tab `delta` places from `at` among `len`, wrapping round.
fn step(at: usize, len: usize, delta: isize) -> usize {
    (at as isize + delta).rem_euclid(len.max(1) as isize) as usize
}

impl PaneSlot {
    /// A pane with one tab, on `note`.
    pub(super) fn new(id: PaneId, note: DocKey, local: Option<PaneLocal>) -> Self {
        PaneSlot {
            id,
            tabs: vec![Tab::new(note.clone())],
            active: 0,
            used: vec![note.clone()],
            note,
            local,
        }
    }

    /// Marks tab `i` as just used.
    fn touch(&mut self, i: usize) {
        let Some(key) = self.tabs.get(i).map(|t| t.doc.clone()) else {
            return;
        };
        self.used.retain(|k| *k != key);
        self.used.insert(0, key);
    }

    /// The tab used before the active one.
    fn previous(&self) -> Option<usize> {
        let key = self.used.get(1)?;
        self.tabs.iter().position(|t| t.doc == *key)
    }

    /// The tab to show when tab `i` (the active one) goes: the last used
    /// other one, else a neighbour.
    fn after_closing(&self, i: usize) -> usize {
        self.used
            .iter()
            .filter_map(|k| self.tabs.iter().position(|t| t.doc == *k))
            .find(|&p| p != i)
            .unwrap_or(if i > 0 { i - 1 } else { 1 })
    }
}

impl Editor {
    fn focused_slot(&self) -> Option<usize> {
        self.panes.iter().position(|p| p.id == self.focused)
    }

    /// Whether any tab of any pane shows `key`.
    fn shown_anywhere(&self, key: &DocKey) -> bool {
        self.source.doc_key() == *key
            || self
                .panes
                .iter()
                .any(|p| p.tabs.iter().any(|t| t.doc == *key))
    }

    /// Closes the parked docs no tab shows any more (an edit group's stay,
    /// for its undo). An untitled note's draft goes with it.
    pub(super) fn drop_unshown_docs(&mut self) {
        let keep: Vec<bool> = self
            .parked
            .iter()
            .map(|d| {
                let key = d.source.doc_key();
                self.shown_anywhere(&key) || self.edit_groups.contains(&key)
            })
            .collect();
        for (doc, kept) in self.parked.iter().zip(&keep) {
            if !kept && let Source::Untitled(n) = doc.source {
                Self::remove_draft(n);
            }
        }
        let mut keep = keep.into_iter();
        self.parked.retain(|_| keep.next().unwrap_or(true));
    }

    /// The editor's view of the focused tab, taken out (it's being left).
    fn stash_local(&mut self) -> PaneLocal {
        let fresh = ListState::new(0, ListAlignment::Top, px(400.));
        PaneLocal {
            lines: std::mem::replace(&mut self.lines, fresh),
            view: self.view,
            cursor: self.vim.cursor(),
            scroll_x: std::mem::take(&mut self.scroll_x),
        }
    }

    /// Parks the editor's note; the editor holds a placeholder until
    /// another is swapped in.
    fn park_focused_doc(&mut self) {
        let mut doc = Doc::new(Buffer::new_plain(""), Source::Scratch);
        self.swap_doc(&mut doc);
        doc.autosave_task = None;
        self.parked.push(doc);
    }

    /// Opens `key`'s doc as a parked one if it isn't open. `false`: it
    /// can't be (a scratch or untitled note that was closed).
    fn ensure_parked(&mut self, key: &DocKey) -> bool {
        if self.parked_index(key).is_some() {
            return true;
        }
        let DocKey::Note(note) = key else {
            return false;
        };
        let Some(vault) = self.vault.clone() else {
            return false;
        };
        match Buffer::open(&vault, note) {
            Ok(buffer) => {
                let source = Source::Note {
                    vault,
                    note: note.clone(),
                };
                let doc = self.new_doc(buffer, source);
                self.parked.push(doc);
                true
            }
            Err(_) => false,
        }
    }

    /// Whether the focused pane's active tab is a blank scratch or untitled
    /// note, which a note opened over it replaces (no empty tab is left).
    fn active_tab_is_blank(&self) -> bool {
        matches!(self.source, Source::Scratch | Source::Untitled(_))
            && self.buffer.len_chars() == 0
            && !self.shown_elsewhere(&self.source.doc_key())
    }

    /// Runs `f` with notes it opens taking their place as `how` says.
    pub(super) fn open_as(&mut self, how: OpenAs, f: impl FnOnce(&mut Self)) {
        let was = std::mem::replace(&mut self.opening, how);
        f(self);
        self.opening = was;
    }

    /// The focused pane's active tab stops being a preview.
    pub(super) fn keep_active_tab(&mut self) {
        if let Some(fi) = self.focused_slot() {
            let slot = &mut self.panes[fi];
            let active = slot.active;
            if let Some(tab) = slot.tabs.get_mut(active) {
                tab.preview = false;
            }
        }
    }

    /// `:keep`, `space k`, a double click on the tab: the preview tab of the
    /// focused pane is kept.
    pub(super) fn keep_preview(&mut self, cx: &mut Context<Self>) {
        let Some(fi) = self.focused_slot() else {
            return;
        };
        match self.panes[fi].tabs.iter_mut().find(|t| t.preview) {
            Some(tab) => tab.preview = false,
            None => self.reply = Some(("No preview tab here".into(), false)),
        }
        cx.notify();
    }

    /// Opens `next` in the focused pane: a new tab after the active one,
    /// or that pane's tab for the note if it has one. A blank untitled or
    /// scratch tab is replaced instead; so is the pane's preview tab when
    /// this is a preview too.
    pub(super) fn open_in_tab(&mut self, mut next: Doc) {
        let key = next.source.doc_key();
        let Some(fi) = self.focused_slot() else {
            return;
        };
        let how = if self.config.preview_tabs || self.opening != OpenAs::Preview {
            self.opening
        } else {
            OpenAs::Kept
        };
        if let Some(i) = self.panes[fi].tabs.iter().position(|t| t.doc == key) {
            // It's already here: show that tab (its doc goes back to wait).
            if key != self.source.doc_key() {
                next.autosave_task = None;
                self.parked.push(next);
                self.switch_active_tab(i);
            }
            return;
        }
        if how == OpenAs::Replace || self.active_tab_is_blank() {
            return self.replace_active(next);
        }
        if how == OpenAs::Preview
            && let Some(p) = self.panes[fi].tabs.iter().position(|t| t.preview)
        {
            // The last preview gives its place to this one.
            self.switch_active_tab(p);
            self.replace_active(next);
            if let Some(tab) = self.panes[fi].tabs.get_mut(p) {
                tab.preview = true;
            }
            return;
        }
        let revision = self.buffer.revision();
        let local = self.stash_local();
        let active = self.panes[fi].active;
        if let Some(tab) = self.panes[fi].tabs.get_mut(active) {
            tab.local = Some(local);
            tab.seen = revision;
        }
        self.park_focused_doc();
        self.swap_doc(&mut next);
        let at = active + 1;
        let slot = &mut self.panes[fi];
        let mut tab = Tab::new(key.clone());
        tab.preview = how == OpenAs::Preview;
        slot.tabs.insert(at, tab);
        slot.active = at;
        slot.note = key;
        slot.touch(at);
        self.fresh_view();
    }

    /// Puts `next` in the focused tab's place (the blank note there goes).
    fn replace_active(&mut self, mut next: Doc) {
        self.swap_doc(&mut next);
        let key = self.source.doc_key();
        if let Some(fi) = self.focused_slot() {
            let slot = &mut self.panes[fi];
            let active = slot.active;
            let old = std::mem::replace(&mut slot.tabs[active].doc, key.clone());
            slot.tabs[active].preview = false;
            slot.used.retain(|k| *k != old);
            slot.note = key;
            slot.touch(active);
        }
        self.park_or_drop(next);
        self.fresh_view();
    }

    /// The view of a note just shown: top, no scroll, measured anew.
    fn fresh_view(&mut self) {
        self.wraps.borrow_mut().clear();
        self.scroll_x.set(px(0.));
        self.vim.set_cursor(0);
        self.vim.clear_jumps();
        self.lines.reset(self.buffer.text().len_lines());
    }

    /// Shows tab `i` of the focused pane: the shown note is parked, tab
    /// `i`'s swapped in with its cursor and scroll.
    fn switch_active_tab(&mut self, i: usize) {
        let Some(fi) = self.focused_slot() else {
            return;
        };
        let active = self.panes[fi].active;
        if i == active || i >= self.panes[fi].tabs.len() {
            return;
        }
        let key = self.panes[fi].tabs[i].doc.clone();
        if !self.ensure_parked(&key) {
            self.reply = Some((format!("{} can't be opened", key.label()).into(), true));
            return;
        }
        let revision = self.buffer.revision();
        let local = self.stash_local();
        let tab = &mut self.panes[fi].tabs[active];
        tab.local = Some(local);
        tab.seen = revision;
        self.park_focused_doc();
        if let Some(mut doc) = self.unpark(&key) {
            self.swap_doc(&mut doc);
        }
        let tab = &mut self.panes[fi].tabs[i];
        let seen = tab.seen;
        match tab.local.take() {
            Some(mut local) => {
                self.swap_local(&mut local);
                // Edited while hidden: its lines measure again, in place.
                let lines = self.buffer.text().len_lines();
                if self.buffer.revision() != seen || self.lines.item_count() != lines {
                    let top = self.lines.logical_scroll_top();
                    self.lines.reset(lines);
                    self.lines.scroll_to(ListOffset {
                        item_ix: top.item_ix.min(lines.saturating_sub(1)),
                        offset_in_item: px(0.),
                    });
                }
            }
            None => self.fresh_view(),
        }
        let slot = &mut self.panes[fi];
        slot.active = i;
        slot.note = key;
        slot.touch(i);
        let cursor = clamp_normal(self.buffer.text(), self.vim.cursor());
        self.vim.set_cursor(cursor);
        self.lines
            .scroll_to_reveal_item(line_of(self.buffer.text(), cursor));
        self.wraps.borrow_mut().clear();
    }

    /// Shows tab `i` of the focused pane, saving the note left.
    pub(super) fn activate_tab(&mut self, i: usize, cx: &mut Context<Self>) {
        self.save_now(cx);
        self.switch_active_tab(i);
        self.restart_blink(cx);
        cx.notify();
    }

    /// The focused pane's active tab and how many it has.
    pub(super) fn focused_tabs(&self) -> (usize, usize) {
        self.focused_slot()
            .map(|fi| (self.panes[fi].active, self.panes[fi].tabs.len()))
            .unwrap_or((0, 1))
    }

    /// `:tabnew [note]`, `:tabedit {note}`, `⌘T`: a new tab after the
    /// active one, on the note named, or an untitled note.
    pub(super) fn tab_new(&mut self, arg: Option<&str>, cx: &mut Context<Self>) {
        match arg.map(str::trim).filter(|a| !a.is_empty()) {
            Some(arg) => self.edit_path(Some(arg), cx),
            None => self.open_untitled(cx),
        }
    }

    /// Whether closing a tab on `key` loses text: a scratch or untitled
    /// note with text, shown by no other tab.
    pub(super) fn loses_text(&self, key: &DocKey) -> bool {
        if !matches!(key, DocKey::Scratch | DocKey::Untitled(_)) || self.tabs_showing(key) > 1 {
            return false;
        }
        if self.source.doc_key() == *key {
            return self.buffer.is_dirty() && self.buffer.len_chars() > 0;
        }
        self.parked_index(key).is_some_and(|i| {
            let buffer = &self.parked[i].buffer;
            buffer.is_dirty() && buffer.len_chars() > 0
        })
    }

    /// How many tabs, in all panes, show `key`.
    fn tabs_showing(&self, key: &DocKey) -> usize {
        self.panes
            .iter()
            .map(|p| p.tabs.iter().filter(|t| t.doc == *key).count())
            .sum()
    }

    fn no_write(&mut self, key: &DocKey) {
        self.reply = Some((
            format!(
                "E37: No write since last change in {} (add ! to override)",
                key.label()
            )
            .into(),
            true,
        ));
    }

    /// `:tabclose[!] [N]`: closes the focused pane's active tab, or tab N.
    /// The last tab of the last pane stays. Unsaved scratch or untitled text
    /// needs `!`.
    pub(super) fn tab_close(&mut self, arg: Option<&str>, force: bool, cx: &mut Context<Self>) {
        let (active, len) = self.focused_tabs();
        let target = match arg.map(str::trim).filter(|a| !a.is_empty()) {
            None => active,
            Some(n) => match n.parse::<usize>().ok().filter(|n| (1..=len).contains(n)) {
                Some(n) => n - 1,
                None => {
                    self.reply = Some((format!("E475: Invalid argument: {n}").into(), true));
                    return;
                }
            },
        };
        if len < 2 && self.panes.len() < 2 {
            self.reply = Some(("E784: Cannot close last tab page".into(), true));
            return;
        }
        if let Some(fi) = self.focused_slot() {
            let key = self.panes[fi].tabs[target].doc.clone();
            if !force && self.loses_text(&key) {
                return self.no_write(&key);
            }
        }
        self.close_tab_at(target, cx);
    }

    /// Closes tab `i` of the focused pane, keeping it for `:tabreopen`. The
    /// pane's last-used tab shows; its last tab closes the pane. `false`
    /// when it's the last tab of the last pane (nothing closes).
    pub(super) fn close_tab_at(&mut self, i: usize, cx: &mut Context<Self>) -> bool {
        let Some(fi) = self.focused_slot() else {
            return false;
        };
        let len = self.panes[fi].tabs.len();
        if i >= len {
            return false;
        }
        self.save_now(cx);
        if len < 2 {
            // The pane's last tab: the pane goes, and with it the tab.
            let pane = self.focused;
            let tab = Tab::new(self.panes[fi].tabs[0].doc.clone());
            if !self.close_pane(cx) {
                return false;
            }
            self.keep_closed(pane, 0, tab);
            self.drop_unshown_docs();
            return true;
        }
        if i == self.panes[fi].active {
            let next = self.panes[fi].after_closing(i);
            self.switch_active_tab(next);
        }
        let slot = &mut self.panes[fi];
        let mut tab = slot.tabs.remove(i);
        if i < slot.active {
            slot.active -= 1;
        }
        let key = tab.doc.clone();
        slot.used.retain(|k| *k != key);
        tab.seen = 0;
        self.keep_closed(self.focused, i, tab);
        self.drop_unshown_docs();
        self.restart_blink(cx);
        cx.notify();
        true
    }

    fn keep_closed(&mut self, pane: PaneId, index: usize, tab: Tab) {
        // A note with no name can't be opened again.
        if matches!(tab.doc, DocKey::Scratch | DocKey::Untitled(_)) {
            return;
        }
        self.closed_tabs.push(ClosedTab { pane, index, tab });
        if self.closed_tabs.len() > CLOSED_KEPT {
            self.closed_tabs.remove(0);
        }
    }

    /// `:tabreopen`, `⌘⇧T`: brings back the most recently closed tab where
    /// it was, or in the focused pane if its pane is gone.
    pub(super) fn tab_reopen(&mut self, cx: &mut Context<Self>) {
        let Some(ClosedTab {
            pane,
            index,
            mut tab,
        }) = self.closed_tabs.pop()
        else {
            self.reply = Some(("No closed tab to reopen".into(), true));
            return;
        };
        if self.source.doc_key() != tab.doc && !self.ensure_parked(&tab.doc) {
            self.reply = Some((
                format!("{} can't be opened again", tab.doc.label()).into(),
                true,
            ));
            return;
        }
        if pane != self.focused && self.panes.iter().any(|p| p.id == pane) {
            self.focus_pane(pane, cx);
        }
        let Some(fi) = self.focused_slot() else {
            return;
        };
        if let Some(i) = self.panes[fi].tabs.iter().position(|t| t.doc == tab.doc) {
            return self.activate_tab(i, cx);
        }
        let at = index.min(self.panes[fi].tabs.len());
        tab.seen = 0;
        self.panes[fi].tabs.insert(at, tab);
        if at <= self.panes[fi].active {
            self.panes[fi].active += 1;
        }
        self.activate_tab(at, cx);
    }

    fn invalid(&mut self, arg: Option<&str>) {
        self.reply = Some((
            format!("E475: Invalid argument: {}", arg.unwrap_or("")).into(),
            true,
        ));
    }

    /// `:tabnext [N]`, `gt`: the next tab, tab N, or the last used (`#`).
    pub(super) fn tab_next(&mut self, arg: Option<&str>, cx: &mut Context<Self>) {
        let (active, len) = self.focused_tabs();
        let arg = arg.map(str::trim).filter(|a| !a.is_empty());
        let target = match arg {
            None => Some(step(active, len, 1)),
            Some("#") => self.focused_slot().and_then(|fi| self.panes[fi].previous()),
            Some(n) => n
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=len).contains(n))
                .map(|n| n - 1),
        };
        match target {
            Some(i) => self.activate_tab(i, cx),
            None => self.invalid(arg),
        }
    }

    /// `:tabprevious [N]`, `gT`: the previous tab, or N tabs back.
    pub(super) fn tab_previous(&mut self, arg: Option<&str>, cx: &mut Context<Self>) {
        let (active, len) = self.focused_tabs();
        let arg = arg.map(str::trim).filter(|a| !a.is_empty());
        match arg.map_or(Some(1), |n| n.parse::<isize>().ok()) {
            Some(n) => self.activate_tab(step(active, len, -n), cx),
            None => self.invalid(arg),
        }
    }

    /// `:tabfirst`, `:tablast`.
    pub(super) fn tab_edge(&mut self, last: bool, cx: &mut Context<Self>) {
        let (_, len) = self.focused_tabs();
        self.activate_tab(if last { len - 1 } else { 0 }, cx);
    }

    /// `⌘1` … `⌘8`: tab N, if there is one.
    pub(super) fn tab_number(&mut self, n: usize, cx: &mut Context<Self>) {
        let (_, len) = self.focused_tabs();
        if (1..=len).contains(&n) {
            self.activate_tab(n - 1, cx);
        }
    }

    /// `:tabmove [N|+N|-N]`: moves the active tab in its row.
    pub(super) fn tab_move(&mut self, arg: Option<&str>, cx: &mut Context<Self>) {
        let (active, len) = self.focused_tabs();
        let (Some(fi), Some(to)) = (self.focused_slot(), move_target(active, len, arg)) else {
            return self.invalid(arg);
        };
        let slot = &mut self.panes[fi];
        let tab = slot.tabs.remove(active);
        slot.tabs.insert(to, tab);
        slot.active = to;
        cx.notify();
    }

    /// Moves tab `from` of pane `pane` to `to` (a tab dragged in its row).
    pub(super) fn move_tab_within(&mut self, pane: PaneId, from: usize, to: usize) {
        let Some(slot) = self.panes.iter_mut().find(|p| p.id == pane) else {
            return;
        };
        if from >= slot.tabs.len() || to >= slot.tabs.len() {
            return;
        }
        let tab = slot.tabs.remove(from);
        slot.tabs.insert(to, tab);
        let a = slot.active;
        slot.active = if a == from {
            to
        } else if from < a && a <= to {
            a - 1
        } else if to <= a && a < from {
            a + 1
        } else {
            a
        };
    }

    /// `:tabs`: every pane and its tabs, `>` at the active one and `+` at
    /// unsaved text.
    pub(super) fn tab_list(&mut self) {
        let mut out = Vec::new();
        for (n, id) in self.layout.panes().iter().enumerate() {
            let Some(slot) = self.panes.iter().find(|p| p.id == *id) else {
                continue;
            };
            let focused = if *id == self.focused {
                " (focused)"
            } else {
                ""
            };
            out.push(format!("Pane {}{focused}", n + 1));
            for (i, tab) in slot.tabs.iter().enumerate() {
                let mark = if i == slot.active { '>' } else { ' ' };
                let dirty = if self.source.doc_key() == tab.doc {
                    self.buffer.is_dirty()
                } else {
                    self.parked_index(&tab.doc)
                        .is_some_and(|d| self.parked[d].buffer.is_dirty())
                };
                let plus = if dirty { '+' } else { ' ' };
                out.push(format!("{mark}{plus}  {}", tab.doc.label()));
            }
        }
        self.vim.show_message(out.join("\n"));
    }

    /// For the status line while the focused pane has several tabs: `2/5`.
    pub(super) fn tab_position(&self) -> Option<String> {
        let (active, len) = self.focused_tabs();
        (len > 1).then(|| format!("{}/{len}", active + 1))
    }

    /// `⌃w T`: the active tab leaves for a new pane on the right. A pane
    /// with one tab has nothing to move.
    pub(super) fn tab_to_pane(&mut self, cx: &mut Context<Self>) {
        let (_, len) = self.focused_tabs();
        if len < 2 {
            self.reply = Some(("This pane has one tab: nothing to move".into(), true));
            return;
        }
        let from = self.focused;
        // A pane beside on the note, focused; then the tab leaves the old
        // one, which shows its last-used other tab.
        self.split_pane(Axis::Row, cx);
        self.take_active_tab_from(from);
        cx.notify();
    }

    /// Takes the active tab out of pane `pane`, which isn't the focused one:
    /// it shows its last-used other tab, or goes when that was its last.
    fn take_active_tab_from(&mut self, pane: PaneId) {
        if pane == self.focused {
            return;
        }
        let Some(oi) = self.panes.iter().position(|p| p.id == pane) else {
            return;
        };
        if self.panes[oi].tabs.len() < 2 {
            self.layout.remove(pane);
            self.panes.remove(oi);
            return;
        }
        let active = self.panes[oi].active;
        let key = self.panes[oi].tabs[active].doc.clone();
        let next = self.panes[oi].after_closing(active);
        let next_key = self.panes[oi].tabs[next].doc.clone();
        let fallback = self
            .parked_index(&next_key)
            .map_or(1, |i| self.parked[i].buffer.text().len_lines());
        let view = self.view;
        let old = &mut self.panes[oi];
        let shown = old.tabs[next].local.take().unwrap_or_else(|| PaneLocal {
            lines: ListState::new(fallback, ListAlignment::Top, px(400.)),
            view,
            cursor: 0,
            scroll_x: Default::default(),
        });
        old.tabs.remove(active);
        old.active = if next > active { next - 1 } else { next };
        old.note = next_key;
        old.used.retain(|k| *k != key);
        old.local = Some(shown);
    }

    /// A tab dropped in a tab row: tab `i` of pane `from` goes before tab
    /// `at` of pane `to` (`None`: at the end), and shows there. Across panes
    /// it leaves its pane (which goes with its last tab); if `to` already
    /// has the note, that tab shows instead.
    pub(super) fn drop_tab(
        &mut self,
        from: PaneId,
        i: usize,
        to: PaneId,
        at: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let len_of = |e: &Self, pane: PaneId| {
            e.panes
                .iter()
                .find(|p| p.id == pane)
                .map_or(0, |p| p.tabs.len())
        };
        if i >= len_of(self, from) || len_of(self, to) == 0 {
            return;
        }
        if from == to {
            let last = len_of(self, to) - 1;
            let target = match at {
                None => last,
                Some(j) if i < j => j - 1,
                Some(j) => j.min(last),
            };
            if from != self.focused {
                self.focus_pane(from, cx);
            }
            self.move_tab_within(to, i, target);
            return self.activate_tab(target, cx);
        }
        // Show it in its own pane first: its note is then that pane's.
        if from != self.focused {
            self.focus_pane(from, cx);
        }
        self.activate_tab(i, cx);
        let key = self.source.doc_key();
        self.focus_pane(to, cx);
        let has = self
            .panes
            .iter()
            .find(|p| p.id == to)
            .and_then(|p| p.tabs.iter().position(|t| t.doc == key));
        match has {
            Some(j) => self.activate_tab(j, cx),
            None => {
                let before = len_of(self, to);
                if let Some(doc) = self.unpark(&key) {
                    self.open_in_tab(doc);
                }
                // A new tab (not a blank one replaced): put it where it fell.
                if len_of(self, to) > before {
                    let (now, len) = self.focused_tabs();
                    let target = at.map_or(len - 1, |j| j.min(len - 1));
                    self.move_tab_within(to, now, target);
                }
            }
        }
        self.take_active_tab_from(from);
        self.drop_unshown_docs();
        self.restart_blink(cx);
        cx.notify();
    }

    /// `⌘W`: closes one thing, and never mNote. An overlay or the command
    /// line first; else the focused pane's active tab (its last tab closes
    /// the pane). On the very last tab it only says that `⌘Q` quits. Text
    /// that isn't in a file (an untitled or scratch note) asks first.
    pub(super) fn close_something(&mut self, cx: &mut Context<Self>) {
        if self.pane_menu.take().is_some() {
            return cx.notify();
        }
        if self.picker.is_some() || self.folder_edit.is_some() || self.vim.command_line().is_some()
        {
            self.press(mnote_vim::Key::Esc, cx);
            return;
        }
        let (active, len) = self.focused_tabs();
        if len < 2 && self.panes.len() < 2 {
            self.reply = Some(("⌘Q quits mNote".into(), false));
            return;
        }
        if self.loses_text(&self.source.doc_key()) {
            return self.ask_before_closing(cx);
        }
        self.close_tab_at(active, cx);
    }

    /// "Save "untitled" before closing?": Save as… (the save prompt),
    /// Discard (closes the tab), or Cancel.
    fn ask_before_closing(&mut self, cx: &mut Context<Self>) {
        let name = self.source.doc_key().label();
        let item = |text: &str, detail: &str| crate::picker::custom::Item {
            text: text.into(),
            detail: Some(detail.into()),
        };
        let items = vec![
            item("Save as…", "name it; ⌘W again closes it"),
            item("Discard", "its text goes"),
            item("Cancel", "keep it open"),
        ];
        let picker = crate::picker::custom::CustomPicker::new(
            format!("Save \u{201c}{name}\u{201d} before closing?"),
            items,
            std::rc::Rc::new(move |choice| {
                crate::picker::PickerCallback(std::rc::Rc::new(move |e: &mut Editor, cx| {
                    match choice {
                        0 => e.save_prompt(cx),
                        1 => {
                            if let Source::Untitled(n) = e.source {
                                Self::remove_draft(n);
                            }
                            // Nothing to keep: the text isn't asked about again.
                            e.buffer.mark_saved();
                            let (active, _) = e.focused_tabs();
                            e.close_tab_at(active, cx);
                        }
                        _ => {}
                    }
                }))
            }),
        );
        self.open_picker(crate::picker::Picker::new(Box::new(picker)), cx);
    }

    /// A scratch or untitled note with text that isn't in a file, in any
    /// pane, by name.
    pub(super) fn unsaved_anywhere(&self) -> Option<String> {
        let unsaved = |source: &Source, buffer: &Buffer| {
            matches!(source, Source::Scratch | Source::Untitled(_))
                && buffer.is_dirty()
                && buffer.len_chars() > 0
        };
        if unsaved(&self.source, &self.buffer) {
            return Some(self.source.doc_key().label());
        }
        self.parked
            .iter()
            .find(|d| unsaved(&d.source, &d.buffer))
            .map(|d| d.source.doc_key().label())
    }

    /// The panes and their tabs in one line, for `MNOTE_SCRIPT` checks:
    /// `pane 1/2 | a.md >b.md | [>c.md] | parked 1 | …`: the focused pane's
    /// tabs, then the other panes' in layout order.
    pub(super) fn state_line(&self) -> String {
        let row = |slot: &PaneSlot| {
            slot.tabs
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let mark = if i == slot.active { ">" } else { "" };
                    let preview = if t.preview { "~" } else { "" };
                    format!("{mark}{preview}{}", t.doc.label())
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let order = self.layout.panes();
        let at = order.iter().position(|&p| p == self.focused).unwrap_or(0);
        let focused = self
            .focused_slot()
            .map(|fi| row(&self.panes[fi]))
            .unwrap_or_default();
        let others: Vec<String> = order
            .iter()
            .filter(|&&id| id != self.focused)
            .filter_map(|id| self.panes.iter().find(|p| p.id == *id))
            .map(|p| format!("[{}]", row(p)))
            .collect();
        let text: String = self.buffer.to_string().replace('\n', "\\n");
        format!(
            "pane {}/{} | {} | {} | parked {} | closed {} | overlay {} | mode {} | reply {} | cursor {} | text {}",
            at + 1,
            order.len(),
            focused,
            others.join(" "),
            self.parked.len(),
            self.closed_tabs.len(),
            if self.pane_menu.is_some() {
                "menu"
            } else if self.picker.is_some() {
                "open"
            } else {
                "none"
            },
            self.vim.mode().label(),
            self.reply
                .as_ref()
                .map_or("-".to_owned(), |r| r.0.to_string()),
            self.vim.cursor(),
            text,
        )
    }

    /// After a note moved: every tab on it follows, closed ones too.
    pub(super) fn follow_in_tabs(&mut self, follow: &dyn Fn(&mut NotePath)) {
        for slot in &mut self.panes {
            let keys = slot
                .tabs
                .iter_mut()
                .map(|t| &mut t.doc)
                .chain(slot.used.iter_mut());
            for key in keys {
                if let DocKey::Note(note) = key {
                    follow(note);
                }
            }
        }
        for closed in &mut self.closed_tabs {
            if let DocKey::Note(note) = &mut closed.tab.doc {
                follow(note);
            }
        }
    }

    /// After an untitled note was named: every tab on it, in every pane,
    /// shows it by its new name.
    pub(super) fn rename_doc_everywhere(&mut self, from: &DocKey, to: &DocKey) {
        for slot in &mut self.panes {
            if slot.note == *from {
                slot.note = to.clone();
            }
            let keys = slot
                .tabs
                .iter_mut()
                .map(|t| &mut t.doc)
                .chain(slot.used.iter_mut());
            for key in keys {
                if key == from {
                    *key = to.clone();
                }
            }
        }
    }

    /// Whether `:q` here closes a tab (or the pane with it) rather than
    /// quitting mNote.
    pub(super) fn closes_not_quits(&self) -> bool {
        let (_, len) = self.focused_tabs();
        len > 1 || self.panes.len() > 1
    }

    /// `:q` with tabs or panes left: the active tab closes, and the pane
    /// with its last tab.
    pub(super) fn close_active_tab(&mut self, cx: &mut Context<Self>) {
        let (active, _) = self.focused_tabs();
        self.close_tab_at(active, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_step_round() {
        assert_eq!(step(0, 3, 1), 1);
        assert_eq!(step(2, 3, 1), 0, "wraps");
        assert_eq!(step(0, 3, -1), 2);
        assert_eq!(step(1, 3, 5), 0);
        assert_eq!(step(0, 1, 1), 0);
    }

    #[test]
    fn tabmove_places_the_tab_as_vim_does() {
        // Tab 0 of 4.
        assert_eq!(move_target(0, 4, None), Some(3));
        assert_eq!(move_target(0, 4, Some("0")), Some(0));
        assert_eq!(move_target(0, 4, Some("2")), Some(1), "after tab 2");
        assert_eq!(move_target(0, 4, Some("+2")), Some(2));
        assert_eq!(move_target(0, 4, Some("+9")), Some(3));
        assert_eq!(move_target(3, 4, Some("-")), Some(2));
        assert_eq!(move_target(3, 4, Some("1")), Some(1), "after tab 1");
        assert_eq!(move_target(0, 4, Some("x")), None);
    }

    #[test]
    fn a_pane_remembers_the_tab_used_before() {
        let a = DocKey::Config;
        let b = DocKey::Scratch;
        let c = DocKey::Untitled(1);
        let mut slot = PaneSlot::new(PaneId(0), a.clone(), None);
        slot.tabs.push(Tab::new(b.clone()));
        slot.tabs.push(Tab::new(c.clone()));
        slot.touch(2);
        assert_eq!(slot.previous(), Some(0));
        slot.touch(1);
        assert_eq!(slot.previous(), Some(2));
        assert_eq!(slot.used, [b, c.clone(), a]);
        // Closing the active tab (1) shows the last used other one.
        assert_eq!(slot.after_closing(1), 2);
    }
}
