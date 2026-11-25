//! Highlighting (ADR-0004): which scope each byte of a range has, from the
//! highlight queries of every layer of a syntax map that covers it.
//!
//! Only the range asked for is queried (the visible rows), so the cost
//! follows the screen, not the file. Where captures overlap, the winner is,
//! in order: the deeper layer (a fence's Rust over its Markdown), the
//! pattern's `#set! priority` (100 by default), then the pattern's place in
//! its query (later wins in Neovim's queries, first in the grammar crates').
//! Captures Neovim uses for itself (`@_name`, `@spell`, `@conceal`) aren't
//! painted.

use std::ops::Range;
use std::sync::Arc;

use ropey::Rope;
use tree_sitter::{Node, Parser, QueryCursor, StreamingIterator, Tree};

use crate::predicates::apply_offset;
use crate::scopes::Scope;
use crate::syntax::Layer;

/// A run of bytes with one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub scope: Scope,
}

/// Who wins a byte: bigger wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    depth: usize,
    priority: i32,
    order: i64,
}

type Paint = Vec<Option<(Key, Scope)>>;

/// Paints `range` with `lang`'s highlights over `tree`, at `depth`.
fn paint(
    lang: &str,
    tree: &Tree,
    rope: &Rope,
    range: Range<usize>,
    depth: usize,
    base: usize,
    out: &mut Paint,
) {
    let Some(entry) = crate::code::grammar_for(lang) else {
        return;
    };
    let Some(g) = entry.grammar() else {
        return;
    };
    let names = g.query.capture_names();
    let skip: Vec<bool> = names
        .iter()
        .map(|n| n.starts_with('_') || matches!(*n, "spell" | "nospell" | "conceal"))
        .collect();
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(range.clone());
    let mut matches = cursor.matches(&g.query, tree.root_node(), |node: Node| {
        crate::syntax::text_chunks(rope, node.byte_range())
    });
    while let Some(m) = matches.next() {
        if !g.checks.passes(m, rope) {
            continue;
        }
        let pattern = m.pattern_index;
        let key = Key {
            depth,
            priority: g.priorities.get(pattern).copied().unwrap_or(100),
            order: if g.later_wins {
                pattern as i64
            } else {
                -(pattern as i64)
            },
        };
        let offsets = g.checks.offsets(pattern);
        for capture in m.captures() {
            let index = capture.index as usize;
            if skip.get(index).copied().unwrap_or(true) {
                continue;
            }
            let mut bytes = capture.node.byte_range();
            if let Some(offset) = offsets.iter().find(|o| o.capture == capture.index) {
                bytes = apply_offset(rope, bytes, offset);
            }
            let from = bytes.start.max(range.start);
            let to = bytes.end.min(range.end);
            if from >= to {
                continue;
            }
            let scope = g.scopes[index];
            for slot in &mut out[from - base..to - base] {
                if slot.is_none_or(|(k, _)| key >= k) {
                    *slot = Some((key, scope));
                }
            }
        }
    }
}

/// The spans of `range` (bytes of `rope`): the root tree's highlights in
/// `root_lang`, under each layer's. In order, not overlapping, on character
/// boundaries.
pub fn highlight_layers(
    root: Option<(&str, &Tree)>,
    layers: &[Layer],
    rope: &Rope,
    range: Range<usize>,
) -> Vec<Span> {
    let range = range.start.min(rope.len_bytes())..range.end.min(rope.len_bytes());
    if range.is_empty() {
        return Vec::new();
    }
    let mut out: Paint = vec![None; range.len()];
    if let Some((lang, tree)) = root {
        paint(lang, tree, rope, range.clone(), 0, range.start, &mut out);
    }
    for layer in layers {
        for covered in &layer.ranges {
            let from = covered.start.max(range.start);
            let to = covered.end.min(range.end);
            if from < to {
                paint(
                    &layer.language,
                    &layer.tree,
                    rope,
                    from..to,
                    layer.depth,
                    range.start,
                    &mut out,
                );
            }
        }
    }
    spans(&out, range.start, rope)
}

/// The spans of `range` from one tree (a layer's) in `lang`.
pub fn highlight_tree(
    lang: &str,
    tree: &Tree,
    rope: &Rope,
    range: Range<usize>,
) -> Option<Vec<Span>> {
    crate::code::grammar_for(lang)?;
    Some(highlight_layers(Some((lang, tree)), &[], rope, range))
}

/// `code` in `lang`, parsed and highlighted whole, as spans of `code`.
pub fn highlight_text_spans(lang: &str, code: &str) -> Option<Vec<Span>> {
    crate::code::grammar_for(lang)?;
    let language = crate::code::language(lang)?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(code, None)?;
    let rope = Rope::from_str(code);
    highlight_tree(lang, &tree, &rope, 0..code.len())
}

/// `code` in `lang`, parsed and highlighted whole, with scope names: for
/// tests and one-off text. `None` without a grammar and query.
pub fn highlight_text(lang: &str, code: &str) -> Option<Vec<(Range<usize>, Arc<str>)>> {
    crate::code::grammar_for(lang)?;
    let language = crate::code::language(lang)?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(code, None)?;
    let rope = Rope::from_str(code);
    let spans = highlight_tree(lang, &tree, &rope, 0..code.len())?;
    Some(
        spans
            .into_iter()
            .map(|s| (s.range, crate::scopes::name(s.scope)))
            .collect(),
    )
}

fn spans(paint: &Paint, base: usize, rope: &Rope) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for (i, slot) in paint.iter().enumerate() {
        let Some((_, scope)) = slot else { continue };
        let at = base + i;
        match out.last_mut() {
            Some(span) if span.range.end == at && span.scope == *scope => span.range.end = at + 1,
            _ => out.push(Span {
                range: at..at + 1,
                scope: *scope,
            }),
        }
    }
    let on_boundary = |b: usize| {
        rope.try_byte_to_char(b)
            .is_ok_and(|c| rope.char_to_byte(c) == b)
    };
    out.retain(|s| on_boundary(s.range.start) && on_boundary(s.range.end));
    out
}

#[cfg(test)]
mod tests;
