//! The pane's `…` menu (ADR-0014): at the right end of each tab row, a small
//! menu of what to do with the pane's tabs — close in bulk, reopen, split,
//! move. `⌃w .` opens the focused pane's from the keyboard; `j` `k` `⏎`
//! `esc` drive it. Every item is also a registry command (palette, keys).

use std::cell::RefCell;
use std::collections::HashMap;

use gpui::{
    AnyElement, Bounds, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels,
    SharedString, Styled, div, prelude::FluentBuilder, px,
};

use super::panes::{DocKey, PaneId};
use super::*;
use crate::theme::{Palette, StyledExt, Theme, radius, space, text};

/// The menu's width.
const MENU_W: f32 = 300.;

/// One item: its label and the registry command it runs. `None` is a rule
/// between groups.
const ITEMS: &[Option<(&str, &str)>] = &[
    Some(("Close all", "pane-close-tabs")),
    Some(("Close others", "tab-only")),
    Some(("Close saved", "tab-close-saved")),
    Some(("Close to the right", "tab-close-right")),
    None,
    Some(("Reopen closed tab", "tab-reopen")),
    None,
    Some(("Split right", "vsplit")),
    Some(("Split down", "split")),
    Some(("Move tab to new pane", "tab-to-pane")),
];

/// The open menu: which pane's, and the selected item (an index into the
/// items that aren't rules).
pub(super) struct PaneMenu {
    pane: PaneId,
    selected: usize,
}

/// Where each row's `…` button is, from the last frame, to open the menu
/// under it.
pub(super) type MenuAnchors = std::rc::Rc<RefCell<HashMap<PaneId, Bounds<Pixels>>>>;

/// Which tabs a bulk close takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BulkClose {
    /// Every tab, and so the pane (the last pane keeps a fresh untitled).
    All,
    /// All but the active one.
    Others,
    /// Those with nothing unsaved.
    Saved,
    /// Those after the active one.
    Right,
}

fn entries() -> impl Iterator<Item = (&'static str, &'static str)> {
    ITEMS.iter().flatten().copied()
}

/// How an item's key is shown: its binding, else its ex command.
fn shown_key(command: &str) -> Option<String> {
    crate::keymap::current()
        .shown_for(command)
        .or(match command {
            "tab-close-others" => Some(":tabonly".to_owned()),
            _ => None,
        })
}

impl Editor {
    /// Opens pane `pane`'s menu (`…`, or `⌃w .` for the focused pane).
    pub(super) fn open_pane_menu(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if pane != self.focused {
            self.focus_pane(pane, cx);
        }
        self.pane_menu = Some(PaneMenu { pane, selected: 0 });
        cx.notify();
    }

    /// A key while the menu is open: it takes every key.
    pub(super) fn pane_menu_key(&mut self, key: mnote_vim::Key, cx: &mut Context<Self>) {
        use mnote_vim::Key;
        let Some(menu) = &mut self.pane_menu else {
            return;
        };
        let count = entries().count();
        match key {
            Key::Char('j') | Key::Down | Key::Ctrl('n') | Key::Tab => {
                menu.selected = (menu.selected + 1) % count;
            }
            Key::Char('k') | Key::Up | Key::Ctrl('p') | Key::BackTab => {
                menu.selected = (menu.selected + count - 1) % count;
            }
            Key::Enter => {
                let selected = menu.selected;
                self.run_menu_item(selected, cx);
            }
            Key::Esc | Key::Char('q') => self.pane_menu = None,
            _ => {}
        }
        cx.notify();
    }

    fn run_menu_item(&mut self, n: usize, cx: &mut Context<Self>) {
        let Some(menu) = self.pane_menu.take() else {
            return;
        };
        let Some((_, command)) = entries().nth(n) else {
            return;
        };
        if menu.pane != self.focused {
            self.focus_pane(menu.pane, cx);
        }
        if let Some(ix) = crate::commands::by_id(command) {
            self.run_command(ix, cx);
        }
        cx.notify();
    }

    /// The open menu, under its pane's `…` button (or at the window's top
    /// right when the pane shows no row), over a catcher that closes it.
    pub(super) fn render_pane_menu(
        &self,
        width: f32,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let menu = self.pane_menu.as_ref()?;
        let anchor = self.menu_anchors.borrow().get(&menu.pane).copied();
        let (left, top) = match anchor {
            Some(b) => (
                (b.right() / px(1.) - MENU_W).max(space::S2),
                b.bottom() / px(1.) + 2.,
            ),
            None => ((width - MENU_W - space::S3).max(space::S2), space::S8),
        };
        let selected = menu.selected;
        let mut n = 0;
        let mut rows: Vec<AnyElement> = Vec::new();
        for item in ITEMS {
            let Some((label, command)) = item else {
                rows.push(
                    div()
                        .h(px(1.))
                        .my(px(space::S1))
                        .bg(Theme::color(p.line))
                        .into_any_element(),
                );
                continue;
            };
            let index = n;
            n += 1;
            let on = index == selected;
            rows.push(
                div()
                    .id(("pane-menu-item", index))
                    .flex()
                    .items_center()
                    .gap(px(space::S3))
                    .h(px(crate::theme::row_h()))
                    .px(px(space::S2))
                    .rounded(px(radius::SM))
                    .text_style(text::UI)
                    .text_color(Theme::color(if on { p.select_ink } else { p.ink }))
                    .when(on, |d| d.bg(Theme::color(p.select)))
                    .hover(|s| s.bg(Theme::color(p.select)))
                    .child(SharedString::new_static(label))
                    .child(div().flex_1())
                    .children(shown_key(command).map(|k| {
                        div()
                            .text_style(text::HINT)
                            .text_color(Theme::color(p.ink_3))
                            .child(SharedString::from(k))
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.run_menu_item(index, cx);
                        }),
                    )
                    .into_any_element(),
            );
        }
        let hints = div()
            .flex()
            .gap(px(space::S3))
            .mt(px(space::S1))
            .pt(px(space::S2))
            .px(px(space::S2))
            .border_t_1()
            .border_color(Theme::color(p.line))
            .text_style(text::HINT)
            .text_color(Theme::color(p.ink_2))
            .child(SharedString::new_static("j k move"))
            .child(SharedString::new_static("⏎ run"))
            .child(SharedString::new_static("esc close"));
        Some(
            div()
                .id("pane-menu-catcher")
                .absolute()
                .inset_0()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.pane_menu = None;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .id("pane-menu")
                        .absolute()
                        .left(px(left))
                        .top(px(top))
                        .w(px(MENU_W))
                        .p(px(space::S1))
                        .flex()
                        .flex_col()
                        .bg(Theme::color(p.overlay))
                        .border_1()
                        .border_color(Theme::color(p.line_strong))
                        .rounded(crate::theme::panel_radius(radius::LG))
                        .shadow(cx.theme().modal_shadow())
                        // Clicks inside don't reach the catcher.
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .children(rows)
                        .child(hints),
                )
                .into_any_element(),
        )
    }

    /// The tabs of the focused pane a bulk close would take, by index.
    fn bulk_targets(&self, which: BulkClose) -> Vec<usize> {
        let Some(slot) = self.panes.iter().find(|p| p.id == self.focused) else {
            return Vec::new();
        };
        let active = slot.active;
        (0..slot.tabs.len())
            .filter(|&i| match which {
                BulkClose::All => true,
                BulkClose::Others => i != active,
                BulkClose::Right => i > active,
                BulkClose::Saved => !self.has_unsaved(&slot.tabs[i].doc),
            })
            .collect()
    }

    /// Whether `key`'s text has changes not on disk (or isn't in a file).
    fn has_unsaved(&self, key: &DocKey) -> bool {
        if self.source.doc_key() == *key {
            return self.buffer.is_dirty() && self.buffer.len_chars() > 0;
        }
        self.parked_index(key).is_some_and(|i| {
            let b = &self.parked[i].buffer;
            b.is_dirty() && b.len_chars() > 0
        })
    }

    /// Closes the focused pane's tabs `which` takes. Text that isn't in a
    /// file asks once, naming the notes, unless `force`.
    pub(super) fn close_tabs(&mut self, which: BulkClose, force: bool, cx: &mut Context<Self>) {
        let targets = self.bulk_targets(which);
        let Some(fi) = self.panes.iter().position(|p| p.id == self.focused) else {
            return;
        };
        let lost: Vec<DocKey> = targets
            .iter()
            .map(|&i| self.panes[fi].tabs[i].doc.clone())
            .filter(|k| self.loses_text(k))
            .collect();
        if !force && !lost.is_empty() {
            return self.ask_bulk_discard(which, lost, cx);
        }
        self.save_now(cx);
        // The last pane can't go: a fresh untitled tab stays when all go.
        if which == BulkClose::All && self.panes.len() < 2 {
            self.open_untitled(cx);
            return self.close_tabs(BulkClose::Others, true, cx);
        }
        let active = self.panes[fi].active;
        // From the right, the active tab last, so indexes stay put and the
        // pane only goes with its last tab.
        let mut order: Vec<usize> = targets.into_iter().filter(|&i| i != active).collect();
        order.sort_unstable_by(|a, b| b.cmp(a));
        let closes_active = self.bulk_targets(which).contains(&active);
        for i in order {
            self.close_tab_at(i, cx);
        }
        if closes_active {
            let (now, _) = self.focused_tabs();
            self.close_tab_at(now, cx);
        }
        cx.notify();
    }

    /// "Close 2 tabs with unsaved text?": Discard all, or Cancel.
    fn ask_bulk_discard(&mut self, which: BulkClose, lost: Vec<DocKey>, cx: &mut Context<Self>) {
        let names: Vec<String> = lost.iter().map(DocKey::label).collect();
        let title = match names.len() {
            1 => format!("Close \u{201c}{}\u{201d} without saving it?", names[0]),
            n => format!("Close {n} tabs with text that isn't saved?"),
        };
        let items = vec![
            crate::picker::custom::Item {
                text: "Discard and close".into(),
                detail: Some(names.join(", ")),
            },
            crate::picker::custom::Item {
                text: "Cancel".into(),
                detail: Some("keep them open".into()),
            },
        ];
        let picker = crate::picker::custom::CustomPicker::new(
            title,
            items,
            std::rc::Rc::new(move |choice| {
                let lost = lost.clone();
                crate::picker::PickerCallback(std::rc::Rc::new(move |e: &mut Editor, cx| {
                    if choice == 0 {
                        e.discard_text(&lost);
                        e.close_tabs(which, true, cx);
                    }
                }))
            }),
        );
        self.open_picker(crate::picker::Picker::new(Box::new(picker)), cx);
    }

    /// Lets go of `keys`' unsaved text (and their drafts): closing them
    /// won't ask again.
    fn discard_text(&mut self, keys: &[DocKey]) {
        for key in keys {
            if let DocKey::Untitled(n) = key {
                Self::remove_draft(*n);
            }
            if self.source.doc_key() == *key {
                self.buffer.mark_saved();
            } else if let Some(i) = self.parked_index(key) {
                self.parked[i].buffer.mark_saved();
            }
        }
    }
}
