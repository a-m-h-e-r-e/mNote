//! [`LineText`]: one line of the note, shaped and wrapped by gpui's text
//! system, with its selection and caret painted in.
//!
//! The block caret, the selection and search or substitute highlights are
//! drawn as text-run backgrounds, so they follow the glyphs across wrapped
//! rows and the character under the block caret takes the `on-cursor` colour.
//! The bar caret, highlight outlines and the "newline is selected" cell are
//! quads at positions from the shaped line.
//!
//! With [`LineText::nowrap`] the line is one row however long, drawn
//! shifted by an offset every row shares and clipped to its box; the line
//! with the caret moves the offset as it's laid out, so the caret stays in
//! view and every row paints at the new offset in the same frame.
//!
//! With [`LineText::on_mouse`], a press or drag over the text reports the
//! byte under the pointer (in the shown text), for the editor to map back
//! to the note.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, AvailableSpace, Bounds, ContentMask, DispatchPhase, Element, ElementId, Font,
    GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, Pixels, SharedString, Style, TextAlign, TextRun, Window, WrappedLine, fill,
    outline, point, px, relative, size,
};
use mnote_vim::CaretShape;

/// Where and how to draw the caret on this line.
#[derive(Debug, Clone, Copy)]
pub struct Caret {
    /// Byte offset in the line.
    pub byte: usize,
    pub shape: CaretShape,
    pub color: Hsla,
    /// Colour of the character under a block caret.
    pub on_color: Hsla,
}

/// Coloured text on the line: a search match, or a `:s` replacement.
#[derive(Debug, Clone)]
pub struct Highlight {
    /// Byte range in the line.
    pub bytes: Range<usize>,
    pub background: Hsla,
    pub color: Hsla,
    /// A 1px outline around it (the `:s` match that runs first).
    pub outline: Option<Hsla>,
}

/// Inline formatting of a run of the line (bold, a link, inline code), under
/// any [`Highlight`].
#[derive(Debug, Clone, Default)]
pub struct Styling {
    /// Byte range in the line.
    pub bytes: Range<usize>,
    pub color: Option<Hsla>,
    pub background: Option<Hsla>,
    pub weight: Option<gpui::FontWeight>,
    pub underline: bool,
    pub strike: bool,
}

/// What the mouse did over a line's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseHit {
    /// Pressed: how many clicks in a row, and Shift and ⌘.
    Down {
        clicks: usize,
        shift: bool,
        command: bool,
    },
    /// Moved with the button held.
    Drag,
}

/// Called with the byte under the pointer (in the shown text) and what the
/// mouse did.
pub type MouseHandler = Rc<dyn Fn(usize, MouseHit, &mut Window, &mut App)>;

/// Called as the line is painted with the bytes (in the shown text) where
/// each screen row after the first starts.
pub type WrapHandler = Rc<dyn Fn(Vec<usize>)>;

/// One rendered line of the note.
pub struct LineText {
    text: SharedString,
    font: Font,
    font_size: Pixels,
    line_height: Pixels,
    color: Hsla,
    /// Selected bytes. An end past the text means the newline is selected
    /// too, shown as a small cell after the last character.
    selection: Option<(Range<usize>, Hsla)>,
    caret: Option<Caret>,
    highlights: Vec<Highlight>,
    styling: Vec<Styling>,
    shaped: Rc<RefCell<Option<Shaped>>>,
    on_mouse: Option<MouseHandler>,
    on_wraps: Option<WrapHandler>,
    /// `nowrap`: the sideways offset every row shares.
    scroll_x: Option<Rc<Cell<Pixels>>>,
}

/// Columns kept visible beside the caret when the note scrolls sideways.
const SCROLL_MARGIN_CELLS: f32 = 4.;

/// The sideways offset that shows the caret at `x` (a cell wide) in `width`,
/// moving `offset` as little as possible and keeping a margin beside it.
pub fn follow_caret(offset: Pixels, x: Pixels, cell: Pixels, width: Pixels) -> Pixels {
    let margin = (cell * SCROLL_MARGIN_CELLS).min(width / 4.);
    if x - margin < offset {
        (x - margin).max(px(0.))
    } else if x + cell + margin > offset + width {
        x + cell + margin - width
    } else {
        offset
    }
}

struct Shaped {
    wrap_width: Option<Pixels>,
    line: WrappedLine,
}

impl LineText {
    pub fn new(
        text: SharedString,
        font: Font,
        font_size: Pixels,
        line_height: Pixels,
        color: Hsla,
    ) -> Self {
        LineText {
            text,
            font,
            font_size,
            line_height,
            color,
            selection: None,
            caret: None,
            highlights: Vec::new(),
            styling: Vec::new(),
            shaped: Rc::default(),
            on_mouse: None,
            on_wraps: None,
            scroll_x: None,
        }
    }

    /// One row however long, scrolled sideways by `offset` (shared by every
    /// row); the line with the caret moves it to keep the caret in view.
    pub fn nowrap(mut self, offset: Rc<Cell<Pixels>>) -> Self {
        self.scroll_x = Some(offset);
        self
    }

    /// Reports where the line wraps to `handler` each time it's painted.
    pub fn on_wraps(mut self, handler: WrapHandler) -> Self {
        self.on_wraps = Some(handler);
        self
    }

    /// Reports presses and drags over the text to `handler`.
    pub fn on_mouse(mut self, handler: MouseHandler) -> Self {
        self.on_mouse = Some(handler);
        self
    }

    pub fn selection(mut self, bytes: Range<usize>, color: Hsla) -> Self {
        self.selection = Some((bytes, color));
        self
    }

    pub fn caret(mut self, caret: Option<Caret>) -> Self {
        self.caret = caret;
        self
    }

    /// Inline formatting, in order and not overlapping.
    pub fn styling(mut self, styling: Vec<Styling>) -> Self {
        self.styling = styling;
        self
    }

    /// Highlights, in order and not overlapping. Empty ones are skipped.
    pub fn highlights(mut self, highlights: Vec<Highlight>) -> Self {
        self.highlights = highlights;
        self
    }

    /// Text runs: plain text, highlights, selected text on its background,
    /// and the character under a block caret, which wins over the rest.
    fn runs(&self) -> Vec<TextRun> {
        let len = self.text.len();
        let block = self
            .caret
            .filter(|c| c.shape == CaretShape::Block && c.byte < len)
            .map(|c| {
                let end = self.text[c.byte..]
                    .chars()
                    .next()
                    .map_or(c.byte, |ch| c.byte + ch.len_utf8());
                (c.byte..end, c)
            });
        let selection = self
            .selection
            .as_ref()
            .map(|(r, color)| (r.start.min(len)..r.end.min(len), *color));

        let mut cuts = vec![0, len];
        for r in [
            block.as_ref().map(|b| b.0.clone()),
            selection.as_ref().map(|s| s.0.clone()),
        ]
        .into_iter()
        .flatten()
        {
            cuts.extend([r.start, r.end]);
        }
        for h in &self.highlights {
            cuts.extend([h.bytes.start.min(len), h.bytes.end.min(len)]);
        }
        for st in &self.styling {
            cuts.extend([st.bytes.start.min(len), st.bytes.end.min(len)]);
        }
        cuts.sort_unstable();
        cuts.dedup();

        cuts.windows(2)
            .filter(|w| w[1] > w[0])
            .map(|w| {
                let at = w[0];
                let in_block = block.as_ref().filter(|(r, _)| r.contains(&at));
                let in_selection = selection.as_ref().filter(|(r, _)| r.contains(&at));
                let in_highlight = self.highlights.iter().find(|h| h.bytes.contains(&at));
                let style = self.styling.iter().find(|st| st.bytes.contains(&at));
                let mut font = self.font.clone();
                if let Some(weight) = style.and_then(|st| st.weight) {
                    font.weight = weight;
                }
                let base = style.and_then(|st| st.color).unwrap_or(self.color);
                TextRun {
                    len: w[1] - w[0],
                    font,
                    color: in_block.map_or_else(
                        || in_highlight.map_or(base, |h| h.color),
                        |(_, c)| c.on_color,
                    ),
                    background_color: in_block
                        .map(|(_, c)| c.color)
                        .or(in_highlight.map(|h| h.background))
                        .or(in_selection.map(|(_, color)| *color))
                        .or(style.and_then(|st| st.background)),
                    underline: style
                        .filter(|st| st.underline)
                        .map(|_| gpui::UnderlineStyle {
                            thickness: px(1.),
                            color: None,
                            wavy: false,
                        }),
                    strikethrough: style
                        .filter(|st| st.strike)
                        .map(|_| gpui::StrikethroughStyle {
                            thickness: px(1.),
                            color: None,
                        }),
                }
            })
            .collect()
    }
}

/// Shapes `text` at `wrap_width`, reusing the last result when the width
/// hasn't changed. gpui caches shaped lines across frames too, so unchanged
/// lines cost almost nothing to lay out again.
fn shape(
    shaped: &RefCell<Option<Shaped>>,
    text: &SharedString,
    runs: &[TextRun],
    font_size: Pixels,
    wrap_width: Option<Pixels>,
    window: &mut Window,
) {
    if shaped
        .borrow()
        .as_ref()
        .is_some_and(|s| s.wrap_width == wrap_width)
    {
        return;
    }
    let line = window
        .text_system()
        .shape_text(text.clone(), font_size, runs, wrap_width, None)
        .ok()
        .and_then(|lines| lines.into_iter().next())
        .unwrap_or_default();
    *shaped.borrow_mut() = Some(Shaped { wrap_width, line });
}

impl IntoElement for LineText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for LineText {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let runs = self.runs();
        let text = self.text.clone();
        let shaped = self.shaped.clone();
        let (font_size, line_height) = (self.font_size, self.line_height);
        let wraps = self.scroll_x.is_none();
        let layout_id =
            window.request_measured_layout(style, move |known, available, window, _cx| {
                let width = known.width.or(match available.width {
                    AvailableSpace::Definite(width) => Some(width),
                    _ => None,
                });
                let wrap_width = width.filter(|_| wraps);
                shape(&shaped, &text, &runs, font_size, wrap_width, window);
                let height = shaped.borrow().as_ref().map_or(line_height, |s| {
                    s.line.size(line_height).height.max(line_height)
                });
                size(width.unwrap_or(px(0.)), height)
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _cx: &mut App,
    ) {
        // The caret's line moves the shared offset before any row paints.
        let (Some(offset), Some(caret)) = (&self.scroll_x, self.caret) else {
            return;
        };
        let runs = self.runs();
        shape(
            &self.shaped,
            &self.text,
            &runs,
            self.font_size,
            None,
            window,
        );
        let shaped = self.shaped.borrow();
        let Some(Shaped { line, .. }) = shaped.as_ref() else {
            return;
        };
        let x = line
            .position_for_index(caret.byte.min(self.text.len()), self.line_height)
            .unwrap_or_default()
            .x;
        let cell = self.font_size * 0.55;
        offset.set(follow_caret(offset.get(), x, cell, bounds.size.width));
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let runs = self.runs();
        let wrap_width = self.scroll_x.is_none().then_some(bounds.size.width);
        shape(
            &self.shaped,
            &self.text,
            &runs,
            self.font_size,
            wrap_width,
            window,
        );
        let shaped = self.shaped.borrow();
        let Some(Shaped { line, .. }) = shaped.as_ref() else {
            return;
        };
        // `nowrap`: drawn shifted left by the shared offset, clipped to the
        // line's box.
        let shift = self.scroll_x.as_ref().map_or(px(0.), |o| o.get());
        let (origin, lh) = (bounds.origin - point(shift, px(0.)), self.line_height);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            self.paint_shaped(line, bounds, origin, lh, window, cx);
        });
    }
}

impl LineText {
    fn paint_shaped(
        &self,
        line: &WrappedLine,
        bounds: Bounds<Pixels>,
        origin: gpui::Point<Pixels>,
        lh: Pixels,
        window: &mut Window,
        cx: &mut App,
    ) {
        let len = self.text.len();
        if let Some(on_wraps) = &self.on_wraps {
            let starts = line
                .wrap_boundaries()
                .iter()
                .filter_map(|b| line.runs().get(b.run_ix)?.glyphs.get(b.glyph_ix))
                .map(|glyph| glyph.index)
                .collect();
            on_wraps(starts);
        }
        if let Some(handler) = self.on_mouse.clone() {
            // The byte nearest the pointer; past the last row, the end.
            let hit_line = line.clone();
            let hit = move |position: gpui::Point<Pixels>| {
                let offset = position - origin;
                hit_line
                    .closest_index_for_position(offset, lh)
                    .unwrap_or_else(|end| end)
                    .min(len)
            };
            let down = handler.clone();
            let down_hit = hit.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble
                    && event.button == MouseButton::Left
                    && bounds.contains(&event.position)
                {
                    let mods = event.modifiers;
                    down(
                        down_hit(event.position),
                        MouseHit::Down {
                            clicks: event.click_count,
                            shift: mods.shift,
                            command: mods.platform,
                        },
                        window,
                        cx,
                    );
                }
            });
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble
                    && event.pressed_button == Some(MouseButton::Left)
                    && bounds.contains(&event.position)
                {
                    handler(hit(event.position), MouseHit::Drag, window, cx);
                }
            });
        }
        let cell = self.font_size * 0.55;
        let at = |byte: usize| {
            line.position_for_index(byte.min(len), lh)
                .unwrap_or_default()
        };

        // The newline (or an empty line) inside a selection: one cell.
        if let Some((range, color)) = &self.selection
            && range.end > len
        {
            let p = at(len);
            window.paint_quad(fill(
                Bounds::new(origin + point(p.x, p.y), size(cell, lh)),
                *color,
            ));
        }

        let _ = line.paint_background(origin, lh, TextAlign::Left, Some(bounds), window, cx);
        let _ = line.paint(origin, lh, TextAlign::Left, Some(bounds), window, cx);

        // Outlines sit 1px outside the highlight, on its first row.
        for h in &self.highlights {
            let Some(color) = h.outline else { continue };
            let (from, to) = (at(h.bytes.start), at(h.bytes.end));
            let right = if to.y == from.y {
                to.x
            } else {
                bounds.size.width
            };
            window.paint_quad(outline(
                Bounds::new(
                    origin + point(from.x - px(2.), from.y),
                    size(right - from.x + px(4.), lh),
                ),
                color,
                gpui::BorderStyle::Solid,
            ));
        }

        if let Some(caret) = self.caret {
            let p = at(caret.byte);
            let width = match caret.shape {
                CaretShape::Bar => Some(px(2.)),
                // A block on a character is drawn by its text run; past the
                // end (an empty line) it needs a quad.
                CaretShape::Block if caret.byte >= len => Some(cell),
                CaretShape::Block => None,
            };
            if let Some(width) = width {
                window.paint_quad(fill(
                    Bounds::new(origin + point(p.x, p.y), size(width, lh)),
                    caret.color,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_offset_follows_the_caret_with_a_margin() {
        let (cell, width) = (px(10.), px(400.));
        // In view: nothing moves.
        assert_eq!(follow_caret(px(0.), px(100.), cell, width), px(0.));
        // Past the right edge: just far enough to show it and 4 cells more.
        assert_eq!(follow_caret(px(0.), px(500.), cell, width), px(150.));
        // Back left of the view: 4 cells before it, never below 0.
        assert_eq!(follow_caret(px(300.), px(200.), cell, width), px(160.));
        assert_eq!(follow_caret(px(300.), px(20.), cell, width), px(0.));
        // A narrow pane keeps a quarter of its width as the margin.
        assert_eq!(follow_caret(px(0.), px(100.), cell, px(80.)), px(50.));
    }
}
