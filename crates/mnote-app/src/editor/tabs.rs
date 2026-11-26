//! Tab pages (ADR-0013): each tab is a layout of panes; notes are shared by
//! every tab, as they are by every pane.
//!
//! The editor's own fields hold the current tab's `layout`, `panes` and
//! `focused`, as they hold the focused pane's note. Every other tab waits as
//! a [`TabPage`], with each of its panes' state in its [`PaneSlot`]s, and
//! switching swaps them. A [`Doc`] stays open while a pane in any tab shows
//! it. [`TabSet`] is the order of the tabs and which were used last, kept
//! apart as plain data.

use gpui::{Context, ListAlignment, ListOffset, ListState, px};
use mnote_core::{Buffer, NotePath};
use mnote_vim::text::{clamp_normal, line_of};

use super::Editor;
use super::panes::{Doc, DocKey, Layout, PaneId, PaneLocal, PaneSlot, Source};

/// A tab's name for as long as it's open; indexes change as tabs move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(pub u32);

/// Closed tabs kept for `:tabreopen`.
const CLOSED_KEPT: usize = 10;

/// The tabs in order, which is current, and the order they were used in.
#[derive(Debug, Clone)]
pub struct TabSet {
    order: Vec<TabId>,
    current: TabId,
    /// Most recently used first; the current tab leads.
    used: Vec<TabId>,
    next: u32,
}

impl Default for TabSet {
    fn default() -> Self {
        TabSet {
            order: vec![TabId(0)],
            current: TabId(0),
            used: vec![TabId(0)],
            next: 1,
        }
    }
}

impl TabSet {
    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn current(&self) -> TabId {
        self.current
    }

    /// The tabs in order.
    pub fn order(&self) -> &[TabId] {
        &self.order
    }

    /// Where `id` is, from 0.
    pub fn index_of(&self, id: TabId) -> Option<usize> {
        self.order.iter().position(|&t| t == id)
    }

    pub fn current_index(&self) -> usize {
        self.index_of(self.current).unwrap_or(0)
    }

    /// A new tab at `index` (clamped), not yet current.
    pub fn add_at(&mut self, index: usize) -> TabId {
        let id = TabId(self.next);
        self.next += 1;
        self.order.insert(index.min(self.order.len()), id);
        self.used.push(id);
        id
    }

    /// A new tab just after the current one, not yet current.
    pub fn add_after_current(&mut self) -> TabId {
        self.add_at(self.current_index() + 1)
    }

    /// Makes `id` current.
    pub fn focus(&mut self, id: TabId) {
        if self.index_of(id).is_none() {
            return;
        }
        self.current = id;
        self.used.retain(|&t| t != id);
        self.used.insert(0, id);
    }

    /// The tab used before the current one (`:tabnext #`).
    pub fn previous(&self) -> Option<TabId> {
        self.used.get(1).copied()
    }

    /// Tab `n`, counting from 1.
    pub fn nth(&self, n: usize) -> Option<TabId> {
        n.checked_sub(1).and_then(|i| self.order.get(i)).copied()
    }

    /// The tab `delta` places from the current one, wrapping round.
    pub fn step(&self, delta: isize) -> TabId {
        let len = self.order.len() as isize;
        let at = (self.current_index() as isize + delta).rem_euclid(len.max(1));
        self.order[at as usize]
    }

    /// Takes `id` out. The last tab can't go (`None`). When it was the
    /// current one, the last used of the others becomes current.
    pub fn remove(&mut self, id: TabId) -> Option<TabId> {
        if self.order.len() < 2 {
            return None;
        }
        let at = self.index_of(id)?;
        self.order.remove(at);
        self.used.retain(|&t| t != id);
        if self.current == id {
            let next = self.used.first().copied().unwrap_or(self.order[0]);
            self.focus(next);
        }
        Some(self.current)
    }
}

/// A tab that isn't current: its layout and every pane's own state.
pub(super) struct TabPage {
    pub id: TabId,
    pub layout: Layout,
    /// Every pane, each with its [`PaneLocal`].
    pub panes: Vec<PaneSlot>,
    pub focused: PaneId,
    /// Each note's revision when the tab was last shown: panes on a note
    /// edited since then measure their lines again.
    pub seen: Vec<(DocKey, u64)>,
}

/// A closed tab, for `:tabreopen`: where it was, and the tab itself.
pub(super) struct ClosedTab {
    pub index: usize,
    pub page: TabPage,
}

impl Editor {
    /// Whether any pane of a tab other than the current one shows `key`.
    pub(super) fn shown_in_other_tabs(&self, key: &DocKey) -> bool {
        self.tab_pages
            .iter()
            .any(|t| t.panes.iter().any(|p| p.note == *key))
    }

    /// Whether any pane at all shows `key` (the focused pane's included).
    fn shown_anywhere(&self, key: &DocKey) -> bool {
        self.source.doc_key() == *key
            || self.panes.iter().any(|p| p.note == *key)
            || self.shown_in_other_tabs(key)
    }

    /// Closes the parked docs no pane shows any more (an edit group's
    /// stay, for its undo). An untitled note's draft goes with it.
    fn drop_unshown_docs(&mut self) {
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

    /// The revision of `key`'s text, if it's open.
    fn revision_of(&self, key: &DocKey) -> Option<u64> {
        if self.source.doc_key() == *key {
            return Some(self.buffer.revision());
        }
        let i = self.parked_index(key)?;
        Some(self.parked[i].buffer.revision())
    }

    /// The line count of `key`'s text, if it's open.
    fn lines_of(&self, key: &DocKey) -> Option<usize> {
        if self.source.doc_key() == *key {
            return Some(self.buffer.text().len_lines());
        }
        let i = self.parked_index(key)?;
        Some(self.parked[i].buffer.text().len_lines())
    }

    /// Moves the current tab's panes into a page, the focused pane's own
    /// state with them. Its note stays the editor's.
    fn stash_tab(&mut self) -> TabPage {
        let fresh = ListState::new(self.buffer.text().len_lines(), ListAlignment::Top, px(400.));
        let local = PaneLocal {
            lines: std::mem::replace(&mut self.lines, fresh),
            view: self.view,
            cursor: self.vim.cursor(),
            scroll_x: std::mem::take(&mut self.scroll_x),
        };
        if let Some(slot) = self.panes.iter_mut().find(|p| p.id == self.focused) {
            slot.local = Some(local);
        }
        let panes = std::mem::take(&mut self.panes);
        let seen = panes
            .iter()
            .filter_map(|p| Some((p.note.clone(), self.revision_of(&p.note)?)))
            .collect();
        TabPage {
            id: self.tabs.current(),
            layout: std::mem::replace(&mut self.layout, Layout::Pane(self.focused)),
            panes,
            focused: self.focused,
            seen,
        }
    }

    /// Parks the editor's note (its tab is being left).
    fn park_focused_doc(&mut self) {
        let mut doc = Doc::new(Buffer::new_plain(""), Source::Scratch);
        self.swap_doc(&mut doc);
        doc.autosave_task = None;
        self.parked.push(doc);
    }

    /// Opens `key`'s doc as a parked one if it isn't open. `false`: it
    /// can't be (a scratch or untitled note that was closed).
    fn ensure_doc(&mut self, key: &DocKey) -> bool {
        if self.source.doc_key() == *key || self.parked_index(key).is_some() {
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

    /// Makes `page` the current tab: its panes the editor's, its focused
    /// pane's note and state swapped in. The editor's note must have been
    /// parked first ([`Self::park_focused_doc`]).
    fn enter_tab(&mut self, mut page: TabPage, cx: &mut Context<Self>) {
        // Panes on notes edited while the tab was hidden measure again,
        // keeping their place.
        for slot in &mut page.panes {
            let Some(local) = &mut slot.local else {
                continue;
            };
            let seen = page.seen.iter().find(|(k, _)| *k == slot.note).map(|s| s.1);
            let now = self.revision_of(&slot.note);
            if seen.is_some() && seen == now {
                continue;
            }
            if let Some(lines) = self.lines_of(&slot.note) {
                let top = local.lines.logical_scroll_top();
                local.lines.reset(lines);
                local.lines.scroll_to(ListOffset {
                    item_ix: top.item_ix.min(lines.saturating_sub(1)),
                    offset_in_item: px(0.),
                });
            }
        }
        self.layout = page.layout;
        self.panes = page.panes;
        self.focused = page.focused;
        let Some(fi) = self.panes.iter().position(|p| p.id == self.focused) else {
            return;
        };
        // The editor holds a placeholder (its note was parked): the
        // focused pane's doc takes its place.
        let key = self.panes[fi].note.clone();
        if let Some(mut doc) = self.unpark(&key) {
            self.swap_doc(&mut doc);
        }
        if let Some(mut local) = self.panes[fi].local.take() {
            self.swap_local(&mut local);
        }
        // Every other pane's cursor stays inside its note's text.
        for slot in &mut self.panes {
            if let Some(local) = &mut slot.local
                && let Some(i) = self
                    .parked
                    .iter()
                    .position(|d| d.source.doc_key() == slot.note)
            {
                local.cursor = clamp_normal(self.parked[i].buffer.text(), local.cursor);
            }
        }
        let cursor = clamp_normal(self.buffer.text(), self.vim.cursor());
        self.vim.set_cursor(cursor);
        let lines = self.buffer.text().len_lines();
        if self.lines.item_count() != lines {
            self.lines.reset(lines);
        }
        self.lines
            .scroll_to_reveal_item(line_of(self.buffer.text(), cursor));
        self.wraps.borrow_mut().clear();
        self.restart_blink(cx);
        cx.notify();
    }

    /// Shows tab `target`.
    pub(super) fn switch_tab(&mut self, target: TabId, cx: &mut Context<Self>) {
        if target == self.tabs.current() {
            return;
        }
        let Some(i) = self.tab_pages.iter().position(|t| t.id == target) else {
            return;
        };
        let next = self.tab_pages.remove(i);
        self.save_now(cx);
        let page = self.stash_tab();
        self.park_focused_doc();
        self.tab_pages.push(page);
        self.tabs.focus(target);
        self.enter_tab(next, cx);
    }

    /// A new tab after this one, on the focused note (as `:split` is),
    /// which becomes current.
    fn open_tab_here(&mut self, cx: &mut Context<Self>) {
        self.save_now(cx);
        let id = self.tabs.add_after_current();
        let view = self.view;
        let top = self.lines.logical_scroll_top();
        let page = self.stash_tab();
        self.tab_pages.push(page);
        self.tabs.focus(id);
        let pane = PaneId(self.next_pane);
        self.next_pane += 1;
        self.layout = Layout::Pane(pane);
        self.panes = vec![PaneSlot {
            id: pane,
            note: self.source.doc_key(),
            local: None,
        }];
        self.focused = pane;
        self.view = view;
        self.lines.scroll_to(top);
        self.wraps.borrow_mut().clear();
        self.restart_blink(cx);
        cx.notify();
    }

    /// `:tabnew [note]`, `:tabedit {note}`, `⌘T`: a new tab after this one,
    /// on the note named, or else an untitled note (as Neovim's empty
    /// buffer).
    pub(super) fn tab_new(&mut self, arg: Option<&str>, cx: &mut Context<Self>) {
        let arg = arg.map(str::trim).filter(|a| !a.is_empty());
        self.open_tab_here(cx);
        match arg {
            Some(arg) => self.edit_path(Some(arg), cx),
            None => self.open_untitled(cx),
        }
    }

    /// Whether closing the tab `panes` would lose text that isn't in a
    /// file: a scratch or untitled note with text that no other tab shows.
    fn unsaved_in(&self, panes: &[PaneSlot], focused_doc: Option<&DocKey>) -> Option<String> {
        let mut keys: Vec<DocKey> = panes.iter().map(|p| p.note.clone()).collect();
        if let Some(k) = focused_doc {
            keys.push(k.clone());
        }
        keys.into_iter().find_map(|key| {
            if !matches!(key, DocKey::Scratch | DocKey::Untitled(_)) {
                return None;
            }
            let dirty = if self.source.doc_key() == key {
                self.buffer.is_dirty() && self.buffer.len_chars() > 0
            } else {
                self.parked_index(&key).is_some_and(|i| {
                    self.parked[i].buffer.is_dirty() && self.parked[i].buffer.len_chars() > 0
                })
            };
            dirty.then(|| key.label())
        })
    }

    /// `:tabclose[!] [N]`: closes the current tab, or tab N. The last tab
    /// stays. Unsaved scratch or untitled text needs `!`.
    pub(super) fn tab_close(&mut self, arg: Option<&str>, force: bool, cx: &mut Context<Self>) {
        let target = match arg.map(str::trim).filter(|a| !a.is_empty()) {
            None => self.tabs.current(),
            Some(n) => match n.parse::<usize>().ok().and_then(|n| self.tabs.nth(n)) {
                Some(id) => id,
                None => {
                    self.reply = Some((format!("E475: Invalid argument: {n}").into(), true));
                    return;
                }
            },
        };
        if self.tabs.len() < 2 {
            self.reply = Some(("E784: Cannot close last tab page".into(), true));
            return;
        }
        if !force {
            let lost = if target == self.tabs.current() {
                self.unsaved_in(&self.panes, None)
            } else {
                self.tab_pages
                    .iter()
                    .find(|t| t.id == target)
                    .and_then(|t| self.unsaved_in(&t.panes, None))
            };
            if let Some(name) = lost {
                self.reply = Some((
                    format!("E37: No write since last change in {name} (add ! to override)").into(),
                    true,
                ));
                return;
            }
        }
        self.close_tab(target, cx);
    }

    /// Closes tab `target` (it isn't the last), keeping it for
    /// `:tabreopen`. The last-used tab shows when it was current.
    pub(super) fn close_tab(&mut self, target: TabId, cx: &mut Context<Self>) {
        let index = self.tabs.index_of(target).unwrap_or(0);
        let page = if target == self.tabs.current() {
            self.save_now(cx);
            let page = self.stash_tab();
            self.park_focused_doc();
            let Some(next) = self.tabs.remove(target) else {
                return;
            };
            let Some(i) = self.tab_pages.iter().position(|t| t.id == next) else {
                return;
            };
            let next = self.tab_pages.remove(i);
            self.enter_tab(next, cx);
            page
        } else {
            let Some(i) = self.tab_pages.iter().position(|t| t.id == target) else {
                return;
            };
            self.tabs.remove(target);
            self.tab_pages.remove(i)
        };
        self.closed_tabs.push(ClosedTab { index, page });
        if self.closed_tabs.len() > CLOSED_KEPT {
            self.closed_tabs.remove(0);
        }
        self.drop_unshown_docs();
        cx.notify();
    }

    /// `:tabonly[!]`: closes every other tab.
    pub(super) fn tab_only(&mut self, force: bool, cx: &mut Context<Self>) {
        if !force
            && let Some(name) = self
                .tab_pages
                .iter()
                .find_map(|t| self.unsaved_in(&t.panes, None))
        {
            self.reply = Some((
                format!("E37: No write since last change in {name} (add ! to override)").into(),
                true,
            ));
            return;
        }
        let others: Vec<TabId> = self.tab_pages.iter().map(|t| t.id).collect();
        for id in others {
            self.close_tab(id, cx);
        }
    }

    /// `:tabreopen`, `⌘⇧T`: brings back the most recently closed tab where
    /// it was. Panes on notes that can't be opened again are left out.
    pub(super) fn tab_reopen(&mut self, cx: &mut Context<Self>) {
        let Some(closed) = self.closed_tabs.pop() else {
            self.reply = Some(("No closed tab to reopen".into(), true));
            return;
        };
        let ClosedTab { index, mut page } = closed;
        let gone: Vec<PaneId> = page
            .panes
            .iter()
            .filter(|p| !self.ensure_doc(&p.note))
            .map(|p| p.id)
            .collect();
        for id in &gone {
            if !page.layout.remove(*id) {
                // Its only pane: nothing to bring back.
                self.reply = Some(("The closed tab's notes can't be opened again".into(), true));
                return;
            }
            page.panes.retain(|p| p.id != *id);
        }
        if !page.panes.iter().any(|p| p.id == page.focused)
            && let Some(first) = page.layout.panes().first()
        {
            page.focused = *first;
        }
        // Pane ids are kept: they're unique in the window, and no other
        // pane took them while the tab was closed.
        page.seen.clear();
        let id = self.tabs.add_at(index);
        page.id = id;
        self.save_now(cx);
        let current = self.stash_tab();
        self.park_focused_doc();
        self.tab_pages.push(current);
        self.tabs.focus(id);
        self.enter_tab(page, cx);
    }

    /// `:tabnext [N]`, `gt`: the next tab, or tab N.
    pub(super) fn tab_next(&mut self, arg: Option<&str>, cx: &mut Context<Self>) {
        let arg = arg.map(str::trim).filter(|a| !a.is_empty());
        let target = match arg {
            None => Some(self.tabs.step(1)),
            Some("#") => self.tabs.previous(),
            Some(n) => n.parse::<usize>().ok().and_then(|n| self.tabs.nth(n)),
        };
        match target {
            Some(id) => self.switch_tab(id, cx),
            None => {
                self.reply = Some((
                    format!("E475: Invalid argument: {}", arg.unwrap_or("")).into(),
                    true,
                ))
            }
        }
    }

    /// `:tabprevious [N]`, `gT`: the previous tab, or N tabs back.
    pub(super) fn tab_previous(&mut self, arg: Option<&str>, cx: &mut Context<Self>) {
        let arg = arg.map(str::trim).filter(|a| !a.is_empty());
        let steps = match arg {
            None => Some(1),
            Some(n) => n.parse::<isize>().ok(),
        };
        match steps {
            Some(n) => {
                let id = self.tabs.step(-n);
                self.switch_tab(id, cx);
            }
            None => {
                self.reply = Some((
                    format!("E475: Invalid argument: {}", arg.unwrap_or("")).into(),
                    true,
                ))
            }
        }
    }

    /// `:tabfirst`, `:tablast`.
    pub(super) fn tab_edge(&mut self, last: bool, cx: &mut Context<Self>) {
        let order = self.tabs.order();
        let id = if last { order.last() } else { order.first() };
        if let Some(&id) = id {
            self.switch_tab(id, cx);
        }
    }

    /// `:tabs`: every tab and its panes' notes, `>` at the focused pane and
    /// `+` at unsaved text, as Vim lists them.
    pub(super) fn tab_list(&mut self) {
        let mut out = Vec::new();
        for (i, &id) in self.tabs.order().iter().enumerate() {
            out.push(format!("Tab page {}", i + 1));
            let (layout, panes, focused) = if id == self.tabs.current() {
                (&self.layout, &self.panes, Some(self.focused))
            } else {
                match self.tab_pages.iter().find(|t| t.id == id) {
                    Some(t) => (&t.layout, &t.panes, None),
                    None => continue,
                }
            };
            for pane in layout.panes() {
                let Some(slot) = panes.iter().find(|p| p.id == pane) else {
                    continue;
                };
                let mark = if focused == Some(pane) { '>' } else { ' ' };
                let dirty = if self.source.doc_key() == slot.note {
                    self.buffer.is_dirty()
                } else {
                    self.parked_index(&slot.note)
                        .is_some_and(|d| self.parked[d].buffer.is_dirty())
                };
                let plus = if dirty { '+' } else { ' ' };
                out.push(format!("{mark}{plus}  {}", slot.note.label()));
            }
        }
        self.vim.show_message(out.join("\n"));
    }

    /// A scratch or untitled note with text that isn't in a file, in any
    /// tab, by name.
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

    /// The tabs, panes and focused text in one line, for `MNOTE_SCRIPT`
    /// checks: `tab 2/2 | >b.md a.md | [a.md] | parked 1 | reply … | text …`.
    pub(super) fn state_line(&self) -> String {
        let pane_list = |layout: &Layout, panes: &[PaneSlot], focused: PaneId| {
            layout
                .panes()
                .into_iter()
                .filter_map(|id| panes.iter().find(|p| p.id == id))
                .map(|p| {
                    let mark = if p.id == focused { ">" } else { "" };
                    format!("{mark}{}", p.note.label())
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let others: Vec<String> = self
            .tabs
            .order()
            .iter()
            .filter_map(|id| self.tab_pages.iter().find(|t| t.id == *id))
            .map(|t| format!("[{}]", pane_list(&t.layout, &t.panes, t.focused)))
            .collect();
        let text: String = self.buffer.to_string().replace('\n', "\\n");
        format!(
            "tab {}/{} | {} | {} | parked {} | closed {} | reply {} | cursor {} | text {}",
            self.tabs.current_index() + 1,
            self.tabs.len(),
            pane_list(&self.layout, &self.panes, self.focused),
            others.join(" "),
            self.parked.len(),
            self.closed_tabs.len(),
            self.reply
                .as_ref()
                .map_or("-".to_owned(), |r| r.0.to_string()),
            self.vim.cursor(),
            text,
        )
    }

    /// For the status line while there are several tabs: `tab 2/3`.
    pub(super) fn tab_position(&self) -> Option<String> {
        (self.tabs.len() > 1)
            .then(|| format!("tab {}/{}", self.tabs.current_index() + 1, self.tabs.len()))
    }

    /// After a note moved: panes in other tabs follow it.
    pub(super) fn follow_in_other_tabs(&mut self, follow: &dyn Fn(&mut NotePath)) {
        for page in &mut self.tab_pages {
            for slot in &mut page.panes {
                if let DocKey::Note(note) = &mut slot.note {
                    follow(note);
                }
            }
            for (key, _) in &mut page.seen {
                if let DocKey::Note(note) = key {
                    follow(note);
                }
            }
        }
        for closed in &mut self.closed_tabs {
            for slot in &mut closed.page.panes {
                if let DocKey::Note(note) = &mut slot.note {
                    follow(note);
                }
            }
        }
    }

    /// After an untitled note was named: every pane on it, in every tab,
    /// shows the note by its new name.
    pub(super) fn rename_doc_everywhere(&mut self, from: &DocKey, to: &DocKey) {
        let all = self
            .panes
            .iter_mut()
            .chain(self.tab_pages.iter_mut().flat_map(|t| t.panes.iter_mut()));
        for slot in all {
            if slot.note == *from {
                slot.note = to.clone();
            }
        }
    }

    /// Whether this is the last pane of its tab with other tabs open:
    /// closing it closes the tab.
    pub(super) fn last_pane_of_a_tab(&self) -> bool {
        self.panes.len() < 2 && self.tabs.len() > 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_open_after_the_current_one_and_step_round() {
        let mut tabs = TabSet::default();
        let b = tabs.add_after_current();
        tabs.focus(b);
        let c = tabs.add_after_current();
        assert_eq!(tabs.order(), [TabId(0), b, c]);
        tabs.focus(TabId(0));
        let d = tabs.add_after_current();
        assert_eq!(tabs.order(), [TabId(0), d, b, c]);
        assert_eq!(tabs.step(1), d);
        assert_eq!(tabs.step(-1), c, "wraps");
        assert_eq!(tabs.step(5), d);
        assert_eq!(tabs.nth(4), Some(c));
        assert_eq!(tabs.nth(0), None);
        assert_eq!(tabs.nth(5), None);
    }

    #[test]
    fn closing_shows_the_last_used_tab() {
        let mut tabs = TabSet::default();
        let b = tabs.add_after_current();
        let c = tabs.add_at(9);
        tabs.focus(c);
        tabs.focus(b);
        // Used: b, c, 0. Closing b shows c, not 0 to its left.
        assert_eq!(tabs.remove(b), Some(c));
        assert_eq!(tabs.current(), c);
        assert_eq!(tabs.previous(), Some(TabId(0)));
        // Closing another tab keeps the current one.
        assert_eq!(tabs.remove(TabId(0)), Some(c));
        assert_eq!(tabs.remove(c), None, "the last tab stays");
        assert_eq!(tabs.len(), 1);
    }

    #[test]
    fn the_previous_tab_is_the_one_used_before() {
        let mut tabs = TabSet::default();
        let b = tabs.add_after_current();
        let c = tabs.add_after_current();
        tabs.focus(c);
        assert_eq!(tabs.previous(), Some(TabId(0)));
        tabs.focus(b);
        assert_eq!(tabs.previous(), Some(c));
        assert_ne!(b, c);
    }
}
