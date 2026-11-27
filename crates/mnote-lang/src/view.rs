//! A read-only snapshot of a document's syntax (AMI-233), for the editing
//! services and Vim's syntax-aware hooks: the nodes at a position, the
//! captures of an editing query in a range, and the language at a position.
//!
//! It holds copies of the syntax map's trees (a tree copy is O(1)) and of
//! the text (a rope clone is O(1)), so it's cheap to take for one command
//! and answers without the map. Its answers are plain data in character
//! positions, so callers need no tree-sitter types. Every answer is
//! bounded by what it's asked: a position's ancestors, or the matches in a
//! range.

use std::ops::Range;

use ropey::Rope;
use tree_sitter::{Node, QueryCursor, StreamingIterator, Tree};

use crate::syntax::Layer;

/// A syntax node: its kind (`function_item`), the characters it covers,
/// whether it's named (not punctuation), and its layer's language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxNode {
    pub kind: String,
    pub range: Range<usize>,
    pub named: bool,
    pub language: String,
}

/// One capture of an editing query: its name (`indent.begin`,
/// `function.outer`), the characters it covers (after `#offset!`), the
/// `#set!` properties that apply to it (`indent.immediate`), its language,
/// and which match it came from, so captures of one match can be grouped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub name: String,
    pub range: Range<usize>,
    pub properties: Vec<(String, Option<String>)>,
    pub language: String,
    pub matched: usize,
}

/// A document's trees and text at one moment.
#[derive(Debug, Clone)]
pub struct SyntaxSnapshot {
    rope: Rope,
    language: String,
    root: Tree,
    /// Injection layers, deepest first.
    layers: Vec<Layer>,
}

impl SyntaxSnapshot {
    /// A snapshot of `root` (in `language`) and its `layers` for `rope`.
    pub fn new(rope: Rope, language: &str, root: Tree, layers: &[Layer]) -> Self {
        let mut layers = layers.to_vec();
        layers.sort_by_key(|l| std::cmp::Reverse(l.depth));
        SyntaxSnapshot {
            rope,
            language: language.to_owned(),
            root,
            layers,
        }
    }

    /// The text it was taken of.
    pub fn text(&self) -> &Rope {
        &self.rope
    }

    /// The language at character `pos`: the deepest injection covering it
    /// (a fence's), else the document's.
    pub fn language_at(&self, pos: usize) -> &str {
        let byte = self.byte(pos);
        self.layers
            .iter()
            .find(|l| l.covers(byte))
            .map_or(self.language.as_str(), |l| l.language.as_str())
    }

    /// The nodes at character `pos`, innermost first: in the deepest layer
    /// covering it up to that layer's root, then in each layer around it,
    /// and last the document's tree.
    pub fn nodes_at(&self, pos: usize) -> Vec<SyntaxNode> {
        let byte = self.byte(pos);
        let mut out = Vec::new();
        for layer in self.layers.iter().filter(|l| l.covers(byte)) {
            self.ancestors(&layer.tree, &layer.language, byte, &mut out);
        }
        self.ancestors(&self.root, &self.language, byte, &mut out);
        out
    }

    fn ancestors(&self, tree: &Tree, language: &str, byte: usize, out: &mut Vec<SyntaxNode>) {
        // A node's ancestors are few (tens), so this stays small.
        let mut node = tree.root_node().descendant_for_byte_range(byte, byte);
        while let Some(n) = node {
            out.push(SyntaxNode {
                kind: n.kind().to_owned(),
                range: self.chars(n.byte_range()),
                named: n.is_named(),
                language: language.to_owned(),
            });
            node = n.parent();
        }
    }

    /// The captures of the editing query `kind` (see [`crate::editing`])
    /// over the characters in `range`, from the document's tree and every
    /// layer reaching into it, each with its own language's query, ordered
    /// by where they start. Nothing where a language has no such query.
    pub fn captures(&self, kind: &str, range: Range<usize>) -> Vec<Capture> {
        let bytes = self.byte(range.start)..self.byte(range.end);
        let mut out = Vec::new();
        let mut matched = 0;
        self.captures_in(
            &self.root,
            &self.language,
            kind,
            bytes.clone(),
            &mut matched,
            &mut out,
        );
        for layer in &self.layers {
            let reaches = layer
                .ranges
                .iter()
                .any(|r| r.start < bytes.end.max(bytes.start + 1) && bytes.start < r.end);
            if reaches {
                self.captures_in(
                    &layer.tree,
                    &layer.language,
                    kind,
                    bytes.clone(),
                    &mut matched,
                    &mut out,
                );
            }
        }
        out.sort_by_key(|c| (c.range.start, std::cmp::Reverse(c.range.end)));
        out
    }

    fn captures_in(
        &self,
        tree: &Tree,
        language: &str,
        kind: &str,
        bytes: Range<usize>,
        matched: &mut usize,
        out: &mut Vec<Capture>,
    ) {
        let Some(editing) = crate::editing::query(language, kind) else {
            return;
        };
        let query = &editing.query;
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(bytes);
        let rope = &self.rope;
        let mut matches = cursor.matches(query, tree.root_node(), |node: Node| {
            crate::syntax::text_chunks(rope, node.byte_range())
        });
        while let Some(m) = matches.next() {
            if !editing.checks.passes(m, &self.rope) {
                continue;
            }
            let settings = query.property_settings(m.pattern_index);
            for c in m.captures() {
                let mut range = c.node.byte_range();
                for offset in editing.checks.offsets(m.pattern_index) {
                    if offset.capture == c.index {
                        range = crate::predicates::apply_offset(&self.rope, range, offset);
                    }
                }
                let properties = settings
                    .iter()
                    .filter(|p| p.capture_id.is_none_or(|id| id == c.index as usize))
                    .map(|p| (p.key.to_string(), p.value.as_deref().map(str::to_owned)))
                    .collect();
                out.push(Capture {
                    name: names[c.index as usize].to_owned(),
                    range: self.chars(range),
                    properties,
                    language: language.to_owned(),
                    matched: *matched,
                });
            }
            *matched += 1;
        }
    }

    fn byte(&self, pos: usize) -> usize {
        self.rope.char_to_byte(pos.min(self.rope.len_chars()))
    }

    fn chars(&self, bytes: Range<usize>) -> Range<usize> {
        let len = self.rope.len_bytes();
        self.rope.byte_to_char(bytes.start.min(len))..self.rope.byte_to_char(bytes.end.min(len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::Syntax;

    /// A snapshot of `text` parsed as `language`, waiting for the parse.
    fn snapshot(text: &str, language: &str) -> SyntaxSnapshot {
        let rope = Rope::from_str(text);
        let mut syntax = if language == "markdown" {
            Syntax::markdown()
        } else {
            let grammar = crate::code::language(language).expect("a built-in grammar");
            Syntax::with_injections(&grammar, Some(language))
        }
        .expect("starts parsing");
        let root = syntax.tree_now(&rope).expect("parses").clone();
        let layers = syntax.layers_now(&rope).to_vec();
        SyntaxSnapshot::new(rope, language, root, &layers)
    }

    fn named(nodes: &[SyntaxNode]) -> Vec<(&str, &str)> {
        nodes
            .iter()
            .filter(|n| n.named)
            .map(|n| (n.kind.as_str(), n.language.as_str()))
            .collect()
    }

    const RUST: &str =
        "struct S;\n\nimpl S {\n    fn go(&self, x: u8) -> u8 {\n        x + 1\n    }\n}\n";

    #[test]
    fn nodes_at_a_position_go_from_innermost_out() {
        let s = snapshot(RUST, "rust");
        let at = RUST.find("x + 1").expect("in the text");
        let all = s.nodes_at(at);
        let nodes = named(&all);
        assert_eq!(nodes.first(), Some(&("identifier", "rust")));
        let kinds: Vec<&str> = nodes.iter().map(|(k, _)| *k).collect();
        let f = kinds
            .iter()
            .position(|k| *k == "function_item")
            .expect("in a function");
        let i = kinds
            .iter()
            .position(|k| *k == "impl_item")
            .expect("in an impl");
        assert!(f < i, "{kinds:?}");
        assert_eq!(kinds.last(), Some(&"source_file"));
        let function = s
            .nodes_at(at)
            .into_iter()
            .find(|n| n.kind == "function_item")
            .expect("its function");
        assert_eq!(
            &RUST[function.range.clone()],
            "fn go(&self, x: u8) -> u8 {\n        x + 1\n    }"
        );
        assert_eq!(s.language_at(at), "rust");
    }

    #[test]
    fn captures_come_from_the_editing_query_in_a_range() {
        let s = snapshot(RUST, "rust");
        let all = s.captures("textobjects", 0..RUST.chars().count());
        let outer: Vec<&str> = all
            .iter()
            .filter(|c| c.name == "function.outer")
            .map(|c| &RUST[c.range.clone()])
            .collect();
        assert_eq!(outer, ["fn go(&self, x: u8) -> u8 {\n        x + 1\n    }"]);
        assert!(
            all.iter()
                .any(|c| c.name == "parameter.inner" && &RUST[c.range.clone()] == "x: u8")
        );
        // Only matches reaching into the range: none in `struct S;`.
        let first_line = s.captures("textobjects", 0..9);
        assert!(
            first_line.iter().all(|c| c.name != "function.outer"),
            "{first_line:?}"
        );
        // `#set!` properties travel with their captures.
        let indents = s.captures("indents", 0..RUST.chars().count());
        assert!(!indents.is_empty());
        assert!(indents.iter().all(|c| c.language == "rust"), "{indents:?}");
    }

    #[test]
    fn a_fence_in_a_note_is_its_own_language() {
        let note =
            "# Title\n\nSome text.\n\n```rust\nfn main() {\n    let x = 1;\n}\n```\n\nAfter.\n";
        let s = snapshot(note, "markdown");
        let inside = note.find("let x").expect("in the fence");
        assert_eq!(s.language_at(inside), "rust");
        assert_eq!(s.language_at(note.find("Some").expect("prose")), "markdown");
        let all = s.nodes_at(inside);
        let nodes = named(&all);
        let rust_end = nodes
            .iter()
            .rposition(|(_, l)| *l == "rust")
            .expect("rust nodes");
        let md_start = nodes
            .iter()
            .position(|(_, l)| *l == "markdown")
            .expect("then markdown");
        assert!(
            rust_end < md_start,
            "the fence's nodes come first: {nodes:?}"
        );
        assert!(nodes.iter().any(|(k, _)| *k == "function_item"));
        assert!(nodes.iter().any(|(k, _)| *k == "fenced_code_block"));
        let outer: Vec<String> = s
            .captures("textobjects", 0..note.chars().count())
            .into_iter()
            .filter(|c| c.name == "function.outer")
            .map(|c| {
                assert_eq!(c.language, "rust");
                note[c.range].to_owned()
            })
            .collect();
        assert_eq!(outer, ["fn main() {\n    let x = 1;\n}"]);
    }

    #[test]
    fn positions_are_characters_not_bytes() {
        let text = "// héllo wörld\nfn f() {}\n";
        let s = snapshot(text, "rust");
        let chars: Vec<char> = text.chars().collect();
        let at = chars.iter().position(|c| *c == 'f').expect("fn") + 3;
        let function = s
            .nodes_at(at)
            .into_iter()
            .find(|n| n.kind == "function_item")
            .expect("its function");
        let found: String = chars[function.range].iter().collect();
        assert_eq!(found, "fn f() {}");
    }
}
