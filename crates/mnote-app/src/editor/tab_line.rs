//! Tab rows (ADR-0014, ADR-0015): every pane draws its tabs along its top,
//! the lower pane of an above/below split included. There is no top bar:
//! rows at the window's top edge are its first row, and their empty part
//! moves the window.
//!
//! Each tab shows its note's name (with its folder when two share a name)
//! and a close slot at its right end, as VS Code draws it: `×` on the
//! active tab and under the pointer, `●` for text that isn't in a file yet.
//! The active tab sits on paper with no line below it, so it opens into its
//! note; in the focused pane it carries the cursor rule, which marks the
//! focus (panes draw no rule of their own). Rows that don't fit scroll
//! sideways, by trackpad or wheel, and keep the active tab in view.

use std::cell::RefCell;
use std::collections::HashMap;

use gpui::{
    AnyElement, InteractiveElement, IntoElement, MouseButton, ParentElement, ScrollDelta,
    ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Window, div, point,
    prelude::FluentBuilder, px,
};

use super::panes::{DocKey, Layout, PaneId, Source};
use super::*;
use crate::theme::{Palette, StyledExt, Theme, radius, space, text};

/// Longest tab name shown, in characters; longer ones end in `…`.
const NAME_MAX: usize = 24;

/// What the tab rows keep between frames.
#[derive(Default)]
pub(super) struct TabRows {
    /// Each pane's row scroll.
    scroll: RefCell<HashMap<PaneId, ScrollHandle>>,
    /// What each row last brought into view: its active tab, its number of
    /// tabs and its width.
    revealed: RefCell<HashMap<PaneId, (usize, usize, i32)>>,
    /// Whether the window buttons are shown now (`None`: not set yet).
    pub buttons_shown: Option<bool>,
    /// A window action a command asked for, done at the next frame (it
    /// needs the window).
    pub window_op: Option<WindowOp>,
}

/// What `⌘M` and `⌃⌘F` do to the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WindowOp {
    Minimize,
    FullScreen,
}

/// A tab being dragged: where it's from, and how its ghost looks.
#[derive(Clone)]
pub(super) struct DraggedTab {
    pane: PaneId,
    index: usize,
    name: SharedString,
    colors: (u32, u32, u32),
}

/// The tab drawn under the pointer while it's dragged.
struct TabGhost(DraggedTab);

impl gpui::Render for TabGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let (bg, ink, edge) = self.0.colors;
        div()
            .px(px(14.))
            .py(px(6.))
            .rounded(px(radius::SM))
            .border_1()
            .border_color(Theme::color(edge))
            .bg(Theme::color(bg))
            .text_color(Theme::color(ink))
            .text_style(text::STATUS)
            .opacity(0.9)
            .child(self.0.name.clone())
    }
}

/// One tab as its row draws it.
struct TabInfo {
    index: usize,
    name: String,
    folder: Option<String>,
    unsaved: bool,
    active: bool,
    preview: bool,
}

impl Layout {
    /// The panes along the window's top edge.
    pub fn top_panes(&self) -> Vec<PaneId> {
        match self {
            Layout::Pane(id) => vec![*id],
            Layout::Split(super::panes::Axis::Row, parts) => {
                parts.iter().flat_map(Layout::top_panes).collect()
            }
            Layout::Split(super::panes::Axis::Column, parts) => {
                parts.first().map(Layout::top_panes).unwrap_or_default()
            }
        }
    }

    /// The pane in the window's top left corner.
    pub fn top_left_pane(&self) -> PaneId {
        match self {
            Layout::Pane(id) => *id,
            Layout::Split(_, parts) => parts.first().map_or(PaneId(0), Layout::top_left_pane),
        }
    }
}

impl Editor {
    /// Whether panes show their tab rows now (`show_tabs`).
    pub(super) fn tab_rows_shown(&self) -> bool {
        let most = self.panes.iter().map(|p| p.tabs.len()).max().unwrap_or(1);
        self.config.show_tabs.shows(self.panes.len(), most)
    }

    /// Whether `key`'s text isn't in a file and won't be saved by itself:
    /// a scratch or untitled note with text, or any unsaved note when
    /// autosave is off (autosaving notes would blink a dot as you type).
    pub(super) fn waits_for_saving(&self, key: &DocKey) -> bool {
        let (source, buffer) = if self.source.doc_key() == *key {
            (&self.source, &self.buffer)
        } else {
            match self.parked_index(key) {
                Some(i) => (&self.parked[i].source, &self.parked[i].buffer),
                None => return false,
            }
        };
        if !buffer.is_dirty() {
            return false;
        }
        match source {
            Source::Scratch | Source::Untitled(_) => buffer.len_chars() > 0,
            Source::Note { .. } | Source::Config(_) => !self.config.autosave,
        }
    }

    fn tab_infos(&self, pane: PaneId) -> (Vec<TabInfo>, bool) {
        let Some(slot) = self.panes.iter().find(|p| p.id == pane) else {
            return (Vec::new(), false);
        };
        let labels: Vec<String> = slot.tabs.iter().map(|t| t.doc.label()).collect();
        let tabs = slot
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                let name = labels[i].clone();
                // Two tabs with one name: each says its folder.
                let clash = labels.iter().filter(|l| **l == name).count() > 1;
                let folder = match (&tab.doc, clash) {
                    (DocKey::Note(note), true) => note
                        .as_path()
                        .parent()
                        .and_then(|p| p.file_name())
                        .map(|f| f.to_string_lossy().into_owned()),
                    _ => None,
                };
                TabInfo {
                    index: i,
                    name: shorten(&name),
                    folder,
                    unsaved: self.waits_for_saving(&tab.doc),
                    active: i == slot.active,
                    preview: tab.preview,
                }
            })
            .collect();
        (tabs, pane == self.focused)
    }

    fn row_scroll(&self, pane: PaneId) -> ScrollHandle {
        self.tab_rows
            .scroll
            .borrow_mut()
            .entry(pane)
            .or_default()
            .clone()
    }

    /// Pane `pane`'s tab row.
    pub(super) fn render_tab_row(
        &self,
        pane: PaneId,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (tabs, focused) = self.tab_infos(pane);
        let scroll = self.row_scroll(pane);
        // Bring the active tab into view when it changes, and when the row
        // narrows or gains tabs (a split halves it), once each time.
        if let Some(active) = tabs.iter().find(|t| t.active).map(|t| t.index) {
            let width = (scroll.bounds().size.width / px(1.)).round() as i32;
            let seen = (active, tabs.len(), width);
            let mut revealed = self.tab_rows.revealed.borrow_mut();
            if revealed.get(&pane) != Some(&seen) {
                revealed.insert(pane, seen);
                scroll.scroll_to_item(active);
                // The width is the last frame's: look again once this one
                // is laid out (it settles when the width stops changing).
                cx.notify();
            }
        }
        let top = self.layout.top_panes().contains(&pane);
        let inset = self.config.window_buttons && self.layout.top_left_pane() == pane;
        let bar_h = crate::theme::bar_h();
        let wheel = scroll.clone();
        let cursor = p.cursor;
        div()
            .flex()
            .flex_none()
            .w_full()
            .h(px(bar_h))
            .bg(Theme::color(p.tabs))
            .text_style(text::STATUS)
            // Room for the window buttons when they're shown.
            .when(inset, |d| {
                d.child(
                    div()
                        .flex_none()
                        .w(px(TRAFFIC_LIGHT_INSET))
                        .h_full()
                        .border_b_1()
                        .border_color(Theme::color(p.line))
                        .on_mouse_down(MouseButton::Left, |event, window, cx| {
                            cx.stop_propagation();
                            move_window(event.click_count, window);
                        }),
                )
            })
            .child(
                div()
                    .id(("tab-row", pane.0 as usize))
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_x_scroll()
                    .track_scroll(&scroll)
                    // A wheel turns the row sideways.
                    .on_scroll_wheel(move |event, _, cx| {
                        let delta = match event.delta {
                            ScrollDelta::Pixels(d) => d,
                            ScrollDelta::Lines(l) => point(px(l.x * 20.), px(l.y * 20.)),
                        };
                        if delta.y.abs() > delta.x.abs() {
                            let max = wheel.max_offset().width;
                            let x = (wheel.offset().x + delta.y).clamp(-max, px(0.));
                            wheel.set_offset(point(x, px(0.)));
                            cx.stop_propagation();
                        }
                    })
                    .children(
                        tabs.into_iter()
                            .map(|tab| self.render_tab(pane, tab, focused, p, cx)),
                    )
                    // The empty part: the title bar when it's at the top
                    // edge; a double click opens an untitled tab.
                    .child(
                        div()
                            .id(("tab-row-rest", pane.0 as usize))
                            .flex_1()
                            .min_w(px(space::S8))
                            .h_full()
                            .border_b_1()
                            .border_color(Theme::color(p.line))
                            // A tab dragged here goes at the end of the row.
                            .drag_over::<DraggedTab>(move |style, _, _, _| {
                                style.border_l_2().border_color(Theme::color(cursor))
                            })
                            .on_drop(cx.listener(move |this, dragged: &DraggedTab, _, cx| {
                                this.drop_tab(dragged.pane, dragged.index, pane, None, cx);
                            }))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(
                                    move |this, event: &gpui::MouseDownEvent, window, cx| {
                                        cx.stop_propagation();
                                        if event.click_count >= 2 {
                                            if pane != this.focused {
                                                this.focus_pane(pane, cx);
                                            }
                                            this.tab_new(None, cx);
                                        } else if top {
                                            window.start_window_move();
                                        }
                                    },
                                ),
                            ),
                    ),
            )
            // The pane's tab menu: closing in bulk, reopening, splitting.
            .child(
                div()
                    .id(("tab-menu", pane.0 as usize))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .w(px(36.))
                    .h_full()
                    .border_l_1()
                    .border_b_1()
                    .border_color(Theme::color(p.line))
                    .text_color(Theme::color(p.ink_2))
                    .hover(|s| s.bg(Theme::color(p.well)))
                    .relative()
                    .child(SharedString::new_static("…"))
                    // Where it is, for its menu to open under it.
                    .child({
                        let anchors = self.menu_anchors.clone();
                        gpui::canvas(
                            move |bounds, _, _| {
                                anchors.borrow_mut().insert(pane, bounds);
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0()
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.open_pane_menu(pane, cx);
                        }),
                    ),
            )
            .into_any_element()
    }

    fn render_tab(
        &self,
        pane: PaneId,
        tab: TabInfo,
        focused: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let i = tab.index;
        let id = pane.0 as usize * 10_000 + i;
        let group: SharedString = format!("tab-{}-{i}", pane.0).into();
        let ink = if tab.active { p.ink } else { p.tab_text };
        let close = self.render_close_slot(pane, &tab, group.clone(), p, cx);
        let cursor = p.cursor;
        let dragged = DraggedTab {
            pane,
            index: i,
            name: SharedString::from(tab.name.clone()),
            colors: (p.overlay, p.ink, p.line_strong),
        };
        div()
            .id(("tab", id))
            .group(group)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(space::S1))
            .h_full()
            .pl(px(14.))
            .pr(px(2.))
            .border_r_1()
            .border_color(Theme::color(p.line))
            .text_color(Theme::color(ink))
            .map(|d| {
                if tab.active {
                    // On paper, open into its note: no line below it.
                    d.bg(Theme::color(p.tab_active))
                } else {
                    d.bg(Theme::color(p.tabs))
                        .border_b_1()
                        .hover(|s| s.bg(Theme::color(p.well)))
                }
            })
            // The active tab's rule: the cursor colour marks the focused
            // pane, a quiet one the others.
            .when(tab.active, |d| {
                let rule = if focused { p.cursor } else { p.ink_3 };
                d.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(2.))
                        .bg(Theme::color(rule)),
                )
            })
            .child(
                div()
                    .whitespace_nowrap()
                    // A preview tab's name is in italics.
                    .when(tab.preview, |d| {
                        d.italic()
                            .when(!tab.active, |d| d.text_color(Theme::color(p.tab_preview)))
                    })
                    .child(SharedString::from(tab.name.clone())),
            )
            .children(tab.folder.clone().map(|folder| {
                div()
                    .whitespace_nowrap()
                    .text_color(Theme::color(p.ink_3))
                    .child(SharedString::from(folder))
            }))
            .child(close)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    if pane != this.focused {
                        this.focus_pane(pane, cx);
                    }
                    this.activate_tab(i, cx);
                    // A double click keeps a preview tab.
                    if event.click_count >= 2 {
                        this.keep_active_tab();
                    }
                }),
            )
            // Dragged, it shows under the pointer; dropped on a tab, it goes
            // before that tab (in this row or another pane's).
            .on_drag(dragged, |tab, _, _, cx| cx.new(|_| TabGhost(tab.clone())))
            .drag_over::<DraggedTab>(move |style, _, _, _| {
                style.border_l_2().border_color(Theme::color(cursor))
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedTab, _, cx| {
                this.drop_tab(dragged.pane, dragged.index, pane, Some(i), cx);
            }))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.close_tab_by_mouse(pane, i, cx);
                }),
            )
            .into_any_element()
    }

    /// The close slot at a tab's right end: a `×` on the active tab, `●`
    /// for unsaved text, and a `×` on a soft square under the pointer.
    fn render_close_slot(
        &self,
        pane: PaneId,
        tab: &TabInfo,
        group: SharedString,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let i = tab.index;
        let glyph = |c: &'static str| {
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(SharedString::new_static(c))
        };
        let cross_always = tab.active && !tab.unsaved;
        div()
            .id(("tab-close", pane.0 as usize * 10_000 + i))
            .relative()
            .flex_none()
            .size(px(24.))
            .ml(px(6.))
            .rounded(px(radius::SM))
            .text_color(Theme::color(if tab.active { p.ink } else { p.ink_2 }))
            .hover(|s| s.bg(Theme::color(p.select)))
            .when(tab.unsaved, |d| {
                d.child(
                    glyph("●")
                        .text_size(px(10.))
                        .group_hover(group.clone(), |s| s.opacity(0.)),
                )
            })
            .child(glyph("×").text_size(px(20.)).map(|g| {
                if cross_always {
                    g
                } else {
                    g.opacity(0.).group_hover(group, |s| s.opacity(1.))
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.close_tab_by_mouse(pane, i, cx);
                }),
            )
            .into_any_element()
    }

    /// A tab's `×` or a middle click: closes it, as `⌘W` would close the
    /// active one (asking first for text that isn't in a file).
    fn close_tab_by_mouse(&mut self, pane: PaneId, i: usize, cx: &mut Context<Self>) {
        if pane != self.focused {
            self.focus_pane(pane, cx);
        }
        self.activate_tab(i, cx);
        self.close_something(cx);
    }

    /// The band along the window's top when no tab rows show: it moves the
    /// window, and a double click zooms it, as a title bar does.
    pub(super) fn render_top_band(&self) -> AnyElement {
        div()
            .id("top-band")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(space::S8))
            .on_mouse_down(MouseButton::Left, |event, window, cx| {
                cx.stop_propagation();
                move_window(event.click_count, window);
            })
            .into_any_element()
    }

    /// `⌘+` / `⌘-` (`by` steps) and `⌘0` (`None`): the note text and the
    /// interface bigger or smaller together, as VS Code and Zed zoom.
    pub(super) fn zoom(&mut self, by: Option<i32>, cx: &mut Context<Self>) {
        let level = match by {
            Some(by) => (self.zoom + by).clamp(crate::theme::ZOOM_MIN, crate::theme::ZOOM_MAX),
            None => 0,
        };
        self.zoom = level;
        if crate::theme::set_fonts(self.base_fonts.zoomed(level)) {
            self.remeasure_all();
            cx.refresh_windows();
        }
        let percent = (crate::theme::zoom_factor(level) * 100.).round();
        self.reply = Some((format!("Zoom {percent}%").into(), false));
        cx.notify();
    }

    /// `⌘M`, `⌃⌘F`: done at the next frame, which has the window.
    pub(super) fn window_op(&mut self, op: WindowOp, cx: &mut Context<Self>) {
        self.tab_rows.window_op = Some(op);
        cx.notify();
    }

    /// Shows or hides the window buttons to match `window_buttons`, and
    /// does a window action a command asked for. Each frame; cheap.
    pub(super) fn sync_window(&mut self, window: &mut Window) {
        let want = self.config.window_buttons;
        if self.tab_rows.buttons_shown != Some(want) {
            crate::summon::set_window_buttons(window, want);
            self.tab_rows.buttons_shown = Some(want);
        }
        match self.tab_rows.window_op.take() {
            Some(WindowOp::Minimize) => window.minimize_window(),
            Some(WindowOp::FullScreen) => window.toggle_fullscreen(),
            None => {}
        }
    }
}

/// A press on the window's top edge: a drag moves the window, a double
/// click zooms it.
fn move_window(clicks: usize, window: &mut Window) {
    if clicks >= 2 {
        window.titlebar_double_click();
    } else {
        window.start_window_move();
    }
}

/// A tab name cut to [`NAME_MAX`] characters, with `…`.
fn shorten(name: &str) -> String {
    if name.chars().count() <= NAME_MAX {
        return name.to_owned();
    }
    let mut out: String = name.chars().take(NAME_MAX - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::super::panes::Axis;
    use super::*;

    #[test]
    fn long_names_end_in_an_ellipsis() {
        assert_eq!(shorten("ideas.md"), "ideas.md");
        let short = shorten("roadmap-2027-and-beyond-in-detail.md");
        assert_eq!(short.chars().count(), NAME_MAX);
        assert!(short.ends_with('…'));
    }

    #[test]
    fn top_panes_are_those_along_the_top_edge() {
        let (a, b, c, d) = (PaneId(0), PaneId(1), PaneId(2), PaneId(3));
        // a | (b over c), then d below a.
        let mut layout = Layout::Pane(a);
        layout.split(a, b, Axis::Row);
        layout.split(b, c, Axis::Column);
        layout.split(a, d, Axis::Column);
        assert_eq!(layout.top_panes(), [a, b]);
        assert_eq!(layout.top_left_pane(), a);
    }
}
