//! A document's outline without a language server (AMI-240): its symbols,
//! nested, from the syntax.
//!
//! - Code: the `tags` query's definitions (`@definition.function` with its
//!   `@name`), the format GitHub's code navigation uses, which the grammar
//!   crates ship; a language without one gives the functions, types and
//!   modules its `locals` query defines.
//! - Notes: their headings, each covering its section.
//! - A fence in a note adds its code's symbols under the heading it's in.
//!
//! A language server's `documentSymbol` replaces this in phase 2.

use std::collections::BTreeMap;
use std::ops::Range;

use tree_sitter::{Node, Tree};

use crate::view::SyntaxSnapshot;

/// One symbol: its name, kind (`function`, `class`, `method`, `heading`
/// …), the characters it covers and its name covers, and how deep it is
/// (0 at the top, 1 inside one, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub range: Range<usize>,
    pub name_range: Range<usize>,
    pub depth: usize,
}

/// One `tags` match: its `@name`, and its `@definition.kind` with what it
/// covers.
type TagMatch = (Option<Range<usize>>, Option<(String, Range<usize>)>);

/// `locals` definitions that make an outline (variables don't).
const LOCAL_KINDS: [&str; 6] = ["function", "method", "type", "macro", "namespace", "class"];

impl SyntaxSnapshot {
    /// The document's symbols, in order, with their depth.
    pub fn outline(&self) -> Vec<Symbol> {
        let mut out = Vec::new();
        if self.language == "markdown" {
            self.headings(&self.root, &mut out);
        } else {
            out.extend(self.definitions(&self.language));
        }
        let mut seen = std::collections::HashSet::new();
        for layer in &self.layers {
            if seen.insert(layer.language.clone()) && layer.language != "markdown_inline" {
                out.extend(self.definitions(&layer.language));
            }
        }
        out.sort_by_key(|s| (s.range.start, std::cmp::Reverse(s.range.end)));
        out.dedup_by(|b, a| a.range == b.range && a.name == b.name);
        nest(&mut out);
        out
    }

    /// The symbols around character `pos`, outermost first: where the
    /// cursor is, for breadcrumbs.
    pub fn symbols_at(outline: &[Symbol], pos: usize) -> Vec<&Symbol> {
        outline
            .iter()
            .filter(|s| s.range.start <= pos && pos < s.range.end.max(s.range.start + 1))
            .collect()
    }

    /// `language`'s definitions (in every tree of that language).
    fn definitions(&self, language: &str) -> Vec<Symbol> {
        let len = self.text().len_chars();
        let tags = crate::editing::query(language, "tags").is_some();
        let captures = self.captures(if tags { "tags" } else { "locals" }, 0..len);
        let mut out = Vec::new();
        if tags {
            // Per match: its `@name` and its `@definition.kind`.
            let mut matches: BTreeMap<usize, TagMatch> = BTreeMap::new();
            for c in captures.into_iter().filter(|c| c.language == language) {
                let entry = matches.entry(c.matched).or_default();
                if c.name == "name" {
                    entry.0 = Some(c.range);
                } else if let Some(kind) = c.name.strip_prefix("definition.") {
                    entry.1 = Some((kind.to_owned(), c.range));
                }
            }
            for (name, def) in matches.into_values() {
                if let (Some(name_range), Some((kind, range))) = (name, def) {
                    out.push(self.symbol(kind, range, name_range));
                }
            }
        } else {
            for c in captures.into_iter().filter(|c| c.language == language) {
                let Some(kind) = c.name.strip_prefix("local.definition.") else {
                    continue;
                };
                if !LOCAL_KINDS.contains(&kind) {
                    continue;
                }
                // The definition is the name's parent node.
                let range = self
                    .nodes_at(c.range.start)
                    .into_iter()
                    .find(|n| {
                        n.named && n.range.start <= c.range.start && c.range.end < n.range.end
                    })
                    .map_or(c.range.clone(), |n| n.range);
                out.push(self.symbol(kind.to_owned(), range, c.range));
            }
        }
        out
    }

    fn symbol(&self, kind: String, range: Range<usize>, name_range: Range<usize>) -> Symbol {
        let name: String = self
            .text()
            .slice(name_range.clone())
            .chars()
            .take(120)
            .collect();
        Symbol {
            name: name.trim().to_owned(),
            kind,
            range,
            name_range,
            depth: 0,
        }
    }

    /// A note's headings, each covering its section.
    fn headings(&self, tree: &Tree, out: &mut Vec<Symbol>) {
        let mut cursor = tree.walk();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if matches!(node.kind(), "atx_heading" | "setext_heading") {
                if let Some(symbol) = self.heading(node) {
                    out.push(symbol);
                }
                continue;
            }
            let children: Vec<Node> = node.children(&mut cursor).collect();
            stack.extend(children.into_iter().rev());
        }
    }

    fn heading(&self, node: Node) -> Option<Symbol> {
        let rope = self.text();
        let chars = |r: std::ops::Range<usize>| {
            let len = rope.len_bytes();
            rope.byte_to_char(r.start.min(len))..rope.byte_to_char(r.end.min(len))
        };
        let mut cursor = node.walk();
        let content = node
            .children(&mut cursor)
            .find(|c| matches!(c.kind(), "inline" | "paragraph" | "heading_content"))?;
        let name_range = chars(content.byte_range());
        // The section it heads, when the grammar gives one.
        let range = node
            .parent()
            .filter(|p| p.kind() == "section")
            .map_or_else(|| chars(node.byte_range()), |p| chars(p.byte_range()));
        let name: String = rope.slice(name_range.clone()).to_string();
        Some(Symbol {
            name: name.trim().to_owned(),
            kind: "heading".to_owned(),
            range,
            name_range,
            depth: 0,
        })
    }
}

/// Sets each symbol's depth from what it's inside (sorted by start,
/// outermost first).
fn nest(symbols: &mut [Symbol]) {
    let mut open: Vec<Range<usize>> = Vec::new();
    for s in symbols.iter_mut() {
        while open.last().is_some_and(|o| s.range.start >= o.end) {
            open.pop();
        }
        s.depth = open.len();
        open.push(s.range.clone());
    }
}

#[cfg(test)]
mod tests {
    use crate::syntax::Syntax;
    use crate::view::SyntaxSnapshot;
    use ropey::Rope;

    fn outline(text: &str, language: &str) -> Vec<(usize, String, String)> {
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
            .outline()
            .into_iter()
            .map(|s| (s.depth, s.kind, s.name))
            .collect()
    }

    fn sym(depth: usize, kind: &str, name: &str) -> (usize, String, String) {
        (depth, kind.to_owned(), name.to_owned())
    }

    #[test]
    fn rust_and_python() {
        let rust = "mod m {\n    struct S;\n    impl S {\n        fn go(&self) {}\n    }\n}\nfn main() {}\n";
        let found = outline(rust, "rust");
        assert!(found.contains(&sym(0, "module", "m")), "{found:?}");
        assert!(found.contains(&sym(1, "class", "S")), "{found:?}");
        // Rust's tags don't list `impl` blocks, so a method sits under the
        // module, not the impl.
        assert!(found.contains(&sym(1, "method", "go")), "{found:?}");
        assert!(found.contains(&sym(0, "function", "main")), "{found:?}");
        let py = "class A:\n    def f(self):\n        pass\n\ndef g():\n    pass\n";
        assert_eq!(
            outline(py, "python"),
            [
                sym(0, "class", "A"),
                sym(1, "function", "f"),
                sym(0, "function", "g")
            ]
        );
    }

    #[test]
    fn javascript_typescript_bash_and_json() {
        let js = "class A {\n  m() {}\n}\nfunction f() {}\n";
        let found = outline(js, "javascript");
        assert!(found.contains(&sym(0, "class", "A")), "{found:?}");
        assert!(found.contains(&sym(1, "method", "m")), "{found:?}");
        assert!(found.contains(&sym(0, "function", "f")), "{found:?}");
        let ts = "interface I {\n  x: number;\n}\nfunction f(): void {}\n";
        let found = outline(ts, "typescript");
        assert!(found.contains(&sym(0, "interface", "I")), "{found:?}");
        assert!(found.contains(&sym(0, "function", "f")), "{found:?}");
        let found = outline(ts, "tsx");
        assert!(found.contains(&sym(0, "function", "f")), "{found:?}");
        let sh = "x=1\nhello() {\n  echo hi\n}\n";
        assert_eq!(outline(sh, "bash"), [sym(0, "function", "hello")]);
        assert!(outline("{\"a\": 1}\n", "json").is_empty());
    }

    #[test]
    fn notes_give_their_headings_and_fences_their_code() {
        let note =
            "# Title\n\nText.\n\n## Part one\n\n```rust\nfn inside() {}\n```\n\n## Part two\n";
        assert_eq!(
            outline(note, "markdown"),
            [
                sym(0, "heading", "Title"),
                sym(1, "heading", "Part one"),
                sym(2, "function", "inside"),
                sym(1, "heading", "Part two"),
            ]
        );
    }
}
