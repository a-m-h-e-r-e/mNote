//! Block renderers: lines the styled view draws as something other than
//! text, such as a table, an image or collapsed frontmatter. Each renderer
//! looks at a line (its kind, from the Markdown model, and its text) and
//! either draws it or passes. The first that draws wins.
//!
//! The built-in ones are registered here the way any other would be (a
//! Mermaid or math fence: a renderer that claims `LineKind::Code` with that
//! language). Adding a block kind is one [`Editor::register_block`] call.

use std::rc::Rc;

use gpui::{AnyElement, Context, IntoElement, SharedString, Styled, div, px};
use mnote_core::styled::{LineKind, line_kind};

use super::Editor;
use crate::theme::Palette;

/// What a renderer knows about the line being drawn.
pub struct BlockCtx<'a> {
    /// The line (0-based).
    pub ix: usize,
    pub kind: &'a LineKind,
    /// Its text, without the newline.
    pub content: &'a str,
    /// It's the cursor line (in the focused pane).
    pub current: bool,
    pub cursor_line: usize,
    /// It shows as typed: the cursor is on it, or a `:s` preview changes it.
    /// Most blocks step aside then, so the line can be edited.
    pub raw: bool,
    /// Its line number, as the gutter shows it.
    pub gutter: SharedString,
    pub palette: &'a Palette,
}

/// Draws a line, or passes (`None`).
pub type BlockRenderer =
    Rc<dyn Fn(&mut Editor, &BlockCtx, &mut Context<Editor>) -> Option<AnyElement>>;

impl Editor {
    /// Adds a block renderer, tried after those before it.
    pub fn register_block(&mut self, name: &str, render: BlockRenderer) {
        let mut blocks = (*self.blocks).clone();
        blocks.retain(|(n, _)| n != name);
        blocks.push((name.to_owned(), render));
        self.blocks = Rc::new(blocks);
    }

    /// Frontmatter, images and tables.
    pub(super) fn register_builtin_blocks(&mut self) {
        // Frontmatter collapses to its tags unless the cursor is inside it.
        self.register_block(
            "frontmatter",
            Rc::new(|e, line, _| {
                let LineKind::Frontmatter { first } = line.kind else {
                    return None;
                };
                let rope = e.buffer.text();
                let cursor_inside = matches!(
                    line_kind(e.tree.as_ref(), rope, line.cursor_line),
                    LineKind::Frontmatter { .. }
                );
                if cursor_inside {
                    return None;
                }
                if !first {
                    return Some(div().h(px(0.)).into_any_element());
                }
                let head: String = rope.slice(..rope.len_chars().min(8192)).to_string();
                let fm = mnote_core::Frontmatter::parse(&head);
                Some(e.frontmatter_row(line.gutter.clone(), line.current, &fm.tags, line.palette))
            }),
        );
        self.register_block(
            "image",
            Rc::new(|e, line, _| {
                let LineKind::Image { src, alt } = line.kind else {
                    return None;
                };
                if line.raw {
                    return None;
                }
                Some(e.image_row(line.gutter.clone(), line.current, src, alt, line.palette))
            }),
        );
        self.register_block(
            "table",
            Rc::new(|e, line, _| {
                let LineKind::Table { block, delimiter } = line.kind else {
                    return None;
                };
                if line.raw {
                    return None;
                }
                let block = e.table_block(block.clone(), line.ix);
                Some(e.table_row(
                    line.ix,
                    block,
                    *delimiter,
                    line.content,
                    line.gutter.clone(),
                    line.current,
                    line.palette,
                ))
            }),
        );
    }
}
