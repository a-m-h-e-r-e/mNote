//! What a pane draws around its text: breadcrumbs in the band above it,
//! and scrollbars along its right and bottom edges, as VS Code has them.
//!
//! Breadcrumbs say where the cursor is: the file's path, then `›` and the
//! symbols around it — the items enclosing it in code (`struct Registry ›
//! motions`), the heading chain in a note (`Design › Tabs › Rows`). They're
//! a walk up the syntax tree from the cursor, on the frame.
//!
//! The vertical scrollbar shows the visible part of the note as its thumb,
//! and the cursor's line as a mark; dragging it or clicking its track
//! scrolls. Without wrapping, a horizontal one shows the sideways scroll of
//! the visible lines; dragging it moves the cursor along, as the view
//! follows the cursor sideways.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, Bounds, InteractiveElement, IntoElement, ListOffset, MouseButton, ParentElement,
    Pixels, SharedString, Styled, canvas, div, fill, point, prelude::FluentBuilder, px, size,
};
use mnote_core::lang::scopes::intern;

use super::panes::{DocKey, PaneId};
use super::*;
use crate::theme::{Palette, StyledExt, Theme, space, text};

/// Where each pane's scrollbars are (vertical: `true`), from the last
/// frame, for clicks and drags.
pub(super) type ScrollbarBounds = Rc<RefCell<HashMap<(PaneId, bool), Bounds<Pixels>>>>;

/// A pane's scroll as its scrollbars need it.
struct PaneView {
    lines: gpui::ListState,
    /// Its note's line count, and the cursor's line.
    total: usize,
    cursor_line: usize,
    /// Its sideways offset, without wrapping.
    scroll_x: Rc<Cell<Pixels>>,
    /// The longest visible line, in characters.
    widest: usize,
    /// Its closed folds' hidden lines, so a fold counts as one row.
    rows: Rows,
}

/// Lines as rows on screen, with closed folds' lines hidden (AMI-239).
struct Rows(Vec<std::ops::RangeInclusive<usize>>);

impl Rows {
    /// The row line `line` is shown on (a hidden line, its fold's).
    fn row(&self, line: usize) -> usize {
        let hidden: usize = self
            .0
            .iter()
            .filter(|r| *r.start() < line)
            .map(|r| r.end().min(&line) - r.start())
            .sum();
        line - hidden
    }

    /// The line shown on row `row`.
    fn line(&self, row: usize) -> usize {
        let mut line = row;
        for r in &self.0 {
            if *r.start() < line {
                line += r.end() - r.start();
            }
        }
        line
    }
}

/// The scrollbars' thickness, as in VS Code; the thumb fills it.
const BAR: f32 = 14.;
/// The shortest thumb.
const MIN_THUMB: f32 = 24.;

/// One breadcrumb after the path: a kind word (`struct`, `fn`, or none)
/// and a name, with the scopes they're coloured as.
struct Crumb {
    kind: Option<&'static str>,
    name: String,
    scope: &'static str,
}

/// Code items with names, by node kind: the word shown before the name,
/// and the scope its name is coloured as.
fn item_kind(kind: &str) -> Option<(Option<&'static str>, &'static str)> {
    Some(match kind {
        "function_item" | "function_signature_item" => (Some("fn"), "function"),
        "struct_item" => (Some("struct"), "type"),
        "enum_item" => (Some("enum"), "type"),
        "union_item" => (Some("union"), "type"),
        "trait_item" => (Some("trait"), "type"),
        "impl_item" => (Some("impl"), "type"),
        "mod_item" => (Some("mod"), "module"),
        "macro_definition" => (Some("macro"), "function.macro"),
        "const_item" | "static_item" => (None, "constant"),
        "field_declaration" | "enum_variant" => (None, "property"),
        "function_definition" | "function_declaration" | "method_definition" => {
            (Some("fn"), "function")
        }
        "class_definition" | "class_declaration" => (Some("class"), "type"),
        "interface_declaration" => (Some("interface"), "type"),
        "type_alias_declaration" | "type_item" => (Some("type"), "type"),
        "pair" => (None, "property"),
        _ => return None,
    })
}

/// An outline symbol as a breadcrumb: its kind word and colour.
fn symbol_crumb(symbol: &mnote_core::lang::outline::Symbol) -> Crumb {
    let (kind, scope) = match symbol.kind.as_str() {
        "function" | "method" => (Some("fn"), "function"),
        "module" | "namespace" => (Some("mod"), "module"),
        "interface" => (Some("interface"), "type"),
        "macro" => (Some("macro"), "function.macro"),
        "heading" => (None, "markup.heading"),
        "constant" => (None, "constant"),
        _ => (None, "type"),
    };
    Crumb {
        kind,
        name: symbol.name.lines().next().unwrap_or("").to_owned(),
        scope,
    }
}

impl Editor {
    /// Builds, in the background, the outline of each note another pane
    /// shows whose text changed since (its breadcrumbs come from it).
    pub(super) fn refresh_outlines(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<DocKey> = self
            .panes
            .iter()
            .filter(|s| s.id != self.focused)
            .map(|s| s.note.clone())
            .collect();
        for key in keys {
            // The note may be the focused pane's too, and then it's shown.
            let buffer = if key == self.source.doc_key() {
                &mut self.buffer
            } else {
                match self.parked_index(&key) {
                    Some(i) => &mut self.parked[i].buffer,
                    None => continue,
                }
            };
            let revision = buffer.revision();
            if self
                .outlines
                .get(&key)
                .is_some_and(|(at, _)| *at == revision)
                || self.outline_finding.contains(&key)
            {
                continue;
            }
            let Some(snapshot) = buffer.syntax_snapshot() else {
                continue;
            };
            self.outline_finding.insert(key.clone());
            cx.spawn(async move |this, cx| {
                let outline = cx
                    .background_executor()
                    .spawn(async move { snapshot.outline() })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.outline_finding.remove(&key);
                    this.outlines.insert(key, (revision, Rc::new(outline)));
                    cx.notify();
                });
            })
            .detach();
        }
        // Forget those no pane shows.
        let shown: Vec<DocKey> = self.panes.iter().map(|s| s.note.clone()).collect();
        self.outlines.retain(|k, _| shown.contains(k));
    }

    /// The symbols around another pane's cursor, from its note's outline.
    fn other_crumbs(&self, pane: PaneId) -> Vec<Crumb> {
        let Some(slot) = self.panes.iter().find(|s| s.id == pane) else {
            return Vec::new();
        };
        let (Some(local), Some((_, outline))) = (&slot.local, self.outlines.get(&slot.note)) else {
            return Vec::new();
        };
        mnote_core::lang::view::SyntaxSnapshot::symbols_at(outline, local.cursor)
            .into_iter()
            .map(symbol_crumb)
            .collect()
    }

    /// The symbols around the focused pane's cursor, outermost first.
    fn crumbs(&self) -> Vec<Crumb> {
        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let rope = self.buffer.text();
        let at = rope.char_to_byte(self.vim.cursor().min(rope.len_chars()));
        let Some(mut node) = tree.root_node().descendant_for_byte_range(at, at) else {
            return Vec::new();
        };
        let text_of = |range: std::ops::Range<usize>| -> String {
            let end = range.end.min(rope.len_bytes());
            let start = range.start.min(end);
            rope.byte_slice(start..end).to_string()
        };
        let is_note = matches!(self.buffer.language(), Some("markdown") | None);
        let mut out = Vec::new();
        loop {
            if is_note {
                // A section's heading names it.
                if node.kind() == "section"
                    && let Some(heading) = node.child(0).filter(|h| h.kind().ends_with("heading"))
                {
                    let name = text_of(heading.byte_range())
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim_start_matches('#')
                        .trim()
                        .to_owned();
                    if !name.is_empty() {
                        out.push(Crumb {
                            kind: None,
                            name,
                            scope: "markup.heading",
                        });
                    }
                }
            } else if let Some((kind, scope)) = item_kind(node.kind()) {
                let name = node
                    .child_by_field_name("name")
                    .or_else(|| node.child_by_field_name("type"))
                    .or_else(|| node.child_by_field_name("key"))
                    .map(|n| text_of(n.byte_range()))
                    .map(|n| n.lines().next().unwrap_or("").trim().to_owned())
                    .filter(|n| !n.is_empty());
                if let Some(name) = name {
                    out.push(Crumb { kind, name, scope });
                }
            }
            match node.parent() {
                Some(parent) => node = parent,
                None => break,
            }
        }
        out.reverse();
        out
    }

    /// The band above a pane's text: its path, then (for the focused
    /// pane) the symbols around the cursor.
    pub(super) fn render_breadcrumbs(
        &self,
        pane: PaneId,
        full_width: bool,
        p: &Palette,
        cx: &App,
    ) -> AnyElement {
        let key = self
            .panes
            .iter()
            .find(|s| s.id == pane)
            .map(|s| s.note.clone());
        let path: SharedString = match &key {
            Some(DocKey::Note(note)) => note.to_string().into(),
            Some(other) => other.label().into(),
            None => SharedString::default(),
        };
        let crumbs = if pane == self.focused {
            self.crumbs()
        } else {
            self.other_crumbs(pane)
        };
        let theme = cx.theme();
        let color_of =
            |scope: &str| Theme::color(theme.syntax_color(intern(scope)).unwrap_or(p.ink));
        let sep = || {
            div()
                .flex_none()
                .px(px(space::S1))
                .text_color(Theme::color(p.ink_3))
                .child(SharedString::new_static("›"))
        };
        let mut row = div()
            .flex()
            .items_center()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_style(text::STATUS)
            .text_color(Theme::color(p.ink_2))
            .child(div().flex_none().child(path));
        for crumb in crumbs {
            row = row.child(sep()).child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(space::S1))
                    .children(crumb.kind.map(|k| {
                        div()
                            .text_color(color_of("keyword"))
                            .child(SharedString::new_static(k))
                    }))
                    .child(
                        div()
                            .text_color(color_of(crumb.scope))
                            .child(SharedString::from(crumb.name)),
                    ),
            );
        }
        div()
            .flex()
            .flex_none()
            .w_full()
            .h(px(space::S8))
            .items_center()
            .when(!full_width, |d| d.justify_center())
            .child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .min_w_0()
                    .px(px(space::S4))
                    .when(!full_width, |d| {
                        d.max_w(px(size::GUTTER_W + size::MEASURE + space::S8))
                    })
                    .child(row),
            )
            .into_any_element()
    }

    /// The pane's lines, its scroll and cursor line, and its sideways
    /// offset (each pane keeps its own while another has focus).
    fn pane_view(&self, pane: PaneId) -> Option<PaneView> {
        let slot = self.panes.iter().find(|s| s.id == pane)?;
        if pane == self.focused {
            let rope = self.buffer.text();
            let cursor_line = mnote_vim::text::line_of(rope, self.vim.cursor());
            let widest = self.widest_visible(rope, &self.lines);
            return Some(PaneView {
                lines: self.lines.clone(),
                total: rope.len_lines(),
                cursor_line,
                scroll_x: self.scroll_x.clone(),
                widest,
                rows: Rows(self.vim.hidden_folds(rope)),
            });
        }
        let local = slot.local.as_ref()?;
        let i = self.parked_index(&slot.note)?;
        let rope = self.parked[i].buffer.text();
        let cursor_line = mnote_vim::text::line_of(rope, local.cursor.min(rope.len_chars()));
        let widest = self.widest_visible(rope, &local.lines);
        Some(PaneView {
            lines: local.lines.clone(),
            total: rope.len_lines(),
            cursor_line,
            scroll_x: local.scroll_x.clone(),
            widest,
            rows: Rows(mnote_vim::hidden_lines(&local.folds, rope)),
        })
    }

    /// The longest visible line, in characters (a screenful from the top).
    fn widest_visible(&self, rope: &mnote_core::Rope, lines: &gpui::ListState) -> usize {
        let top = lines.logical_scroll_top().item_ix;
        let rows = rope.len_lines();
        (top..(top + 120).min(rows))
            .map(|r| mnote_vim::text::line_len(rope, r))
            .max()
            .unwrap_or(0)
    }

    /// The scrollbars over pane `pane`'s text: vertical along its right
    /// edge, and (without wrapping) horizontal along its bottom.
    pub(super) fn render_scrollbars(
        &self,
        pane: PaneId,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(PaneView {
            lines,
            total,
            cursor_line,
            scroll_x,
            widest,
            rows,
        }) = self.pane_view(pane)
        else {
            return Vec::new();
        };
        let line_h = text::NOTE_BODY.line_height() / px(1.);
        // In rows, a closed fold counting as one.
        let top = rows.row(lines.logical_scroll_top().item_ix);
        let cursor_line = rows.row(cursor_line);
        let total = rows.row(total.saturating_sub(1)) + 1;
        let (track, thumb, mark) = (p.line, p.line_strong, p.cursor);
        let mut out = Vec::new();

        // Vertical: the thumb is the visible part; a mark at the cursor.
        let map = self.scrollbar_bounds.clone();
        let total_f = total.max(1) as f32;
        out.push(
            div()
                .id(("vscroll", pane.0 as usize))
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .w(px(BAR))
                .child(
                    canvas(
                        move |bounds, _, _| {
                            map.borrow_mut().insert((pane, true), bounds);
                        },
                        move |bounds, _, window, _| {
                            let h = bounds.size.height / px(1.);
                            let visible = (h / line_h).max(1.);
                            if visible >= total_f {
                                return;
                            }
                            window.paint_quad(fill(bounds, Theme::color(track).opacity(0.25)));
                            let thumb_h = (h * visible / total_f).max(MIN_THUMB).min(h);
                            let thumb_y =
                                (h - thumb_h) * (top as f32 / (total_f - visible).max(1.)).min(1.);
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(bounds.left(), bounds.top() + px(thumb_y)),
                                    size(px(BAR), px(thumb_h)),
                                ),
                                Theme::color(thumb).opacity(0.6),
                            ));
                            let mark_y = h * cursor_line as f32 / total_f;
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(bounds.left(), bounds.top() + px(mark_y)),
                                    size(px(BAR), px(2.)),
                                ),
                                Theme::color(mark),
                            ));
                        },
                    )
                    .size_full(),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.scrollbar_drag = Some((pane, true));
                        let b = this.scrollbar_bounds.borrow().get(&(pane, true)).copied();
                        if let Some(b) = b {
                            this.scroll_pane_to(
                                pane,
                                (event.position.y - b.top()) / b.size.height,
                                cx,
                            );
                        }
                    }),
                )
                .into_any_element(),
        );

        // Horizontal, without wrapping: the visible width of the widest
        // visible line.
        if !self.vim.options().wrap {
            let char_w = styled::char_w();
            let content = widest as f32 * char_w;
            let offset = scroll_x.get() / px(1.);
            let map = self.scrollbar_bounds.clone();
            out.push(
                div()
                    .id(("hscroll", pane.0 as usize))
                    .absolute()
                    .left(px(size::GUTTER_W))
                    .right(px(BAR))
                    .bottom_0()
                    .h(px(BAR))
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                map.borrow_mut().insert((pane, false), bounds);
                            },
                            move |bounds, _, window, _| {
                                let w = bounds.size.width / px(1.);
                                if content <= w {
                                    return;
                                }
                                window.paint_quad(fill(bounds, Theme::color(track).opacity(0.25)));
                                let thumb_w = (w * w / content).max(MIN_THUMB).min(w);
                                let thumb_x =
                                    (w - thumb_w) * (offset / (content - w).max(1.)).clamp(0., 1.);
                                window.paint_quad(fill(
                                    Bounds::new(
                                        point(bounds.left() + px(thumb_x), bounds.top()),
                                        size(px(thumb_w), px(BAR)),
                                    ),
                                    Theme::color(thumb).opacity(0.6),
                                ));
                            },
                        )
                        .size_full(),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.scrollbar_drag = Some((pane, false));
                            let b = this.scrollbar_bounds.borrow().get(&(pane, false)).copied();
                            if let Some(b) = b {
                                let at = (event.position.x - b.left()) / b.size.width;
                                this.scroll_pane_sideways(
                                    pane,
                                    at,
                                    content,
                                    b.size.width / px(1.),
                                    cx,
                                );
                            }
                        }),
                    )
                    .into_any_element(),
            );
        }
        out
    }

    /// Scrolls pane `pane` so `at` (0 top … 1 bottom) of its note shows.
    fn scroll_pane_to(&mut self, pane: PaneId, at: f32, cx: &mut Context<Self>) {
        let Some(PaneView {
            lines, total, rows, ..
        }) = self.pane_view(pane)
        else {
            return;
        };
        let shown = rows.row(total.saturating_sub(1)) + 1;
        let row = ((at.clamp(0., 1.) * shown as f32) as usize).min(shown.saturating_sub(1));
        let item = rows.line(row).min(total.saturating_sub(1));
        lines.scroll_to(ListOffset {
            item_ix: item,
            offset_in_item: px(0.),
        });
        cx.notify();
    }

    /// Scrolls pane `pane` sideways so `at` (0 … 1) of the widest line
    /// shows; the focused pane's cursor comes along (the view follows it).
    fn scroll_pane_sideways(
        &mut self,
        pane: PaneId,
        at: f32,
        content: f32,
        width: f32,
        cx: &mut Context<Self>,
    ) {
        let Some(PaneView { scroll_x, .. }) = self.pane_view(pane) else {
            return;
        };
        let x = ((content - width).max(0.) * at.clamp(0., 1.)).round();
        scroll_x.set(px(x));
        if pane == self.focused {
            let rope = self.buffer.text();
            let line = mnote_vim::text::line_of(rope, self.vim.cursor());
            let start = mnote_vim::text::line_start(rope, line);
            let len = mnote_vim::text::line_len(rope, line);
            let col = ((x + width / 2.) / styled::char_w()) as usize;
            self.vim.set_cursor(start + col.min(len.saturating_sub(1)));
        }
        cx.notify();
    }

    /// The mouse moved with a scrollbar held: it keeps scrolling.
    pub(super) fn scrollbar_moved(&mut self, event: &gpui::MouseMoveEvent, cx: &mut Context<Self>) {
        let Some((pane, vertical)) = self.scrollbar_drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.scrollbar_drag = None;
            return;
        }
        let anchors = self
            .scrollbar_bounds
            .borrow()
            .get(&(pane, vertical))
            .copied();
        let Some(b) = anchors else {
            return;
        };
        if vertical {
            self.scroll_pane_to(pane, (event.position.y - b.top()) / b.size.height, cx);
        } else if let Some(PaneView { widest, .. }) = self.pane_view(pane) {
            let content = widest as f32 * styled::char_w();
            let at = (event.position.x - b.left()) / b.size.width;
            self.scroll_pane_sideways(pane, at, content, b.size.width / px(1.), cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Rows;

    #[test]
    fn a_closed_fold_is_one_row() {
        // Lines 2..=5 and 9..=10 folded: 0 1 [2] 6 7 8 [9] 11.
        let rows = Rows(vec![2..=5, 9..=10]);
        assert_eq!(rows.row(1), 1);
        assert_eq!(rows.row(2), 2);
        assert_eq!(rows.row(4), 2, "a hidden line is its fold's row");
        assert_eq!(rows.row(6), 3);
        assert_eq!(rows.row(11), 7);
        for line in [0, 1, 2, 6, 7, 8, 9, 11] {
            assert_eq!(rows.line(rows.row(line)), line, "line {line}");
        }
    }
}
