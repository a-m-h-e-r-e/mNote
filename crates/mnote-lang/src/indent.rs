//! Indent from the syntax tree (AMI-234): nvim-treesitter's `indents.scm`
//! semantics, ported from its `indent.lua` (`get_indent`), so the queries
//! bundled from it (and Neovim users' own) mean what they say there.
//!
//! The captures: `@indent.begin` (lines inside the node are one unit in),
//! `@indent.end` (after it, the next line goes back out), `@indent.align`
//! (lines line up with an open delimiter, or hang one unit in after it),
//! `@indent.dedent` (lines after the node's first are one unit out),
//! `@indent.branch` (the line the node starts on is one unit out),
//! `@indent.ignore` (lines inside are left at 0), `@indent.auto` (lines
//! inside keep the previous line's indent), `@indent.zero` (column 0).
//!
//! The tree used is the smallest one around the line: a code fence's
//! layer inside a note, then the note. A fence's lines start from the
//! indent of its first line.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::time::Duration;

use tree_sitter::{Node, Parser, Point, QueryCursor, StreamingIterator, Tree};

use crate::view::SyntaxSnapshot;

/// A line's indent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indent {
    /// This many columns.
    Columns(usize),
    /// Keep the indent it has (or, for a new line, the previous line's):
    /// `@indent.auto`, as Vim's `autoindent`.
    Keep,
}

/// Captures by node id, with their `#set!` properties.
type Marks = HashMap<usize, HashMap<String, Option<String>>>;

#[derive(Default)]
struct Captures {
    auto: Marks,
    begin: Marks,
    end: Marks,
    dedent: Marks,
    branch: Marks,
    ignore: Marks,
    align: Marks,
    zero: Marks,
}

impl Captures {
    fn slot(&mut self, name: &str) -> Option<&mut Marks> {
        Some(match name {
            "indent.auto" => &mut self.auto,
            "indent.begin" => &mut self.begin,
            "indent.end" => &mut self.end,
            "indent.dedent" => &mut self.dedent,
            "indent.branch" => &mut self.branch,
            "indent.ignore" => &mut self.ignore,
            "indent.align" => &mut self.align,
            "indent.zero" => &mut self.zero,
            _ => return None,
        })
    }
}

/// The longest the tree for a line may take to catch up with the text
/// before its indent is worked out; past it, the tree as it is is used.
const REPARSE_TIME: Duration = Duration::from_millis(4);

/// Layers that are comments, whose trees never decide indent.
const COMMENT_LANGUAGES: [&str; 5] = ["comment", "luadoc", "javadoc", "jsdoc", "phpdoc"];

impl SyntaxSnapshot {
    /// The indent line `line` (0-based) should have, `shiftwidth` columns a
    /// level, tabs `tabstop` wide; `None` when its language has no indent
    /// query (the caller keeps the previous line's indent).
    pub fn indent(&self, line: usize, shiftwidth: usize, tabstop: usize) -> Option<Indent> {
        let rope = &self.rope;
        if line >= rope.len_lines() {
            return None;
        }
        let (tree, language, ranges) = self.tree_for_line(line);
        let editing = crate::editing::query(language, "indents")?;
        // The trees follow edits by shifting; what was just typed (a `}`)
        // isn't in them until they're parsed again, as Neovim does first.
        let fresh = reparse(rope, tree, language, ranges);
        let tree = fresh.as_ref().unwrap_or(tree);
        let lines = Lines {
            rope,
            tabstop: tabstop.max(1),
        };
        let sw = shiftwidth as i64;
        let root = tree.root_node();

        // The node the line's indent is worked out from.
        let blank = lines.text(line).trim().is_empty();
        let from = if blank {
            match (0..line).rev().find(|l| !lines.text(*l).trim().is_empty()) {
                Some(prev) => prev,
                None => return Some(Indent::Columns(0)),
            }
        } else {
            line
        };
        let q = self.indent_captures(tree, &editing, from, line);
        let mut node = if blank {
            let prev = from;
            let text = lines.text(prev);
            let cols = leading_bytes(&text);
            let mut trimmed = text.trim().to_owned();
            let mut node = node_at(root, prev, (cols + trimmed.len()).saturating_sub(1));
            if let Some(n) = node
                && n.kind().contains("comment")
            {
                // A trailing comment doesn't decide: the code before it does
                // (unless the whole line is the comment).
                let first = node_at(root, prev, cols);
                if first.is_some_and(|f| f.id() != n.id()) {
                    let scol = n.start_position().column;
                    trimmed = text
                        .get(cols..scol.max(cols))
                        .unwrap_or("")
                        .trim()
                        .to_owned();
                    node = node_at(root, prev, (cols + trimmed.len()).saturating_sub(1));
                }
            }
            if node.is_some_and(|n| q.end.contains_key(&n.id())) {
                node = node_at(root, line, leading_bytes(&lines.text(line)));
            }
            node
        } else {
            node_at(root, line, leading_bytes(&lines.text(line)))
        };

        let mut indent: i64 = 0;
        if root.start_byte() != 0 {
            // An injected tree starts from its first line's indent.
            indent = lines.width(root.start_position().row) as i64;
        }
        if node.is_some_and(|n| q.zero.contains_key(&n.id())) {
            return Some(Indent::Columns(0));
        }

        let mut align = q.align.clone();
        let mut processed_rows: HashSet<usize> = HashSet::new();
        while let Some(n) = node {
            let id = n.id();
            let (srow, erow) = (n.start_position().row, n.end_position().row);
            let spans_into = srow < line && line <= erow;
            if !q.begin.contains_key(&id)
                && !align.contains_key(&id)
                && q.auto.contains_key(&id)
                && spans_into
            {
                return Some(Indent::Keep);
            }
            if !q.begin.contains_key(&id) && q.ignore.contains_key(&id) && spans_into {
                return Some(Indent::Columns(0));
            }

            let mut is_processed = false;
            if !processed_rows.contains(&srow)
                && ((q.branch.contains_key(&id) && srow == line)
                    || (q.dedent.contains_key(&id) && srow != line))
            {
                indent -= sw;
                is_processed = true;
            }

            let should_process = !processed_rows.contains(&srow);
            let in_error = should_process && n.parent().is_some_and(|p| p.has_error());
            if should_process
                && let Some(meta) = q.begin.get(&id)
                && (srow != erow || in_error || meta.contains_key("indent.immediate"))
                && (srow != line || meta.contains_key("indent.start_at_same_line"))
            {
                indent += sw;
                is_processed = true;
            }

            if in_error && !align.contains_key(&id) {
                // An error node takes its first child's alignment.
                let mut cursor = n.walk();
                if let Some(meta) = n
                    .children(&mut cursor)
                    .find_map(|c| align.get(&c.id()).cloned())
                {
                    align.insert(id, meta);
                }
            }

            if should_process
                && let Some(meta) = align.get(&id)
                && (srow != erow || in_error)
                && srow != line
            {
                let prop = |k: &str| meta.get(k).cloned().flatten();
                let (open, open_last) = match prop("indent.open_delimiter") {
                    Some(d) => delimiter(&lines, n, &d),
                    None => (Some(n), false),
                };
                let (close, close_last) = match prop("indent.close_delimiter") {
                    Some(d) => delimiter(&lines, n, &d),
                    None => (Some(n), false),
                };
                if let Some(open) = open {
                    let (o_srow, o_scol) =
                        (open.start_position().row, open.start_position().column);
                    let c_srow = close.map(|c| c.start_position().row);
                    let mut absolute = false;
                    if open_last {
                        // A hanging indent: the line before ended with the
                        // opening delimiter.
                        indent += sw;
                        if close_last && c_srow.is_some_and(|c| c < line) {
                            indent = (indent - sw).max(0);
                        }
                    } else if close_last && c_srow.is_some_and(|c| c != o_srow && c < line) {
                        indent = (indent - sw).max(0);
                    } else {
                        let increment = prop("indent.increment")
                            .and_then(|v| v.parse::<i64>().ok())
                            .unwrap_or(1);
                        indent = o_scol as i64 + increment;
                        absolute = true;
                    }
                    // The delimiters close on this line: it may need more
                    // indent so it doesn't line up with the next.
                    if c_srow.is_some_and(|c| c != o_srow && c == line)
                        && meta.contains_key("indent.avoid_last_matching_next")
                        && indent <= lines.width(o_srow) as i64 + sw
                    {
                        indent += sw;
                    }
                    is_processed = true;
                    if absolute {
                        return Some(Indent::Columns(indent.max(0) as usize));
                    }
                }
            }

            if is_processed {
                processed_rows.insert(srow);
            }
            node = n.parent();
        }
        Some(Indent::Columns(indent.max(0) as usize))
    }

    /// The smallest tree around the start of `line` that isn't a comment's,
    /// its language, and for a layer the ranges it covers.
    fn tree_for_line(&self, line: usize) -> (&Tree, &str, Option<&[Range<usize>]>) {
        let at = Point {
            row: line,
            column: 0,
        };
        let mut best = (&self.root, self.language.as_str(), None);
        let mut best_len = self.root.root_node().byte_range().len();
        for layer in &self.layers {
            if COMMENT_LANGUAGES.contains(&layer.language.as_str()) {
                continue;
            }
            let r = layer.tree.root_node();
            let inside = r.start_position() <= at && at <= r.end_position();
            if inside && r.byte_range().len() <= best_len {
                best_len = r.byte_range().len();
                best = (
                    &layer.tree,
                    layer.language.as_str(),
                    Some(layer.ranges.as_slice()),
                );
            }
        }
        best
    }

    /// The indent captures of the matches touching lines `from..=to`, by
    /// node: the nodes those lines' indent depends on are there or around
    /// them.
    fn indent_captures(
        &self,
        tree: &Tree,
        editing: &crate::editing::EditingQuery,
        from: usize,
        to: usize,
    ) -> Captures {
        let rope = &self.rope;
        let start = rope.line_to_byte(from);
        let end = rope.line_to_byte((to + 1).min(rope.len_lines()));
        let query = &editing.query;
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(start..end.max(start + 1));
        let mut matches = cursor.matches(query, tree.root_node(), |node: Node| {
            crate::syntax::text_chunks(rope, node.byte_range())
        });
        let mut out = Captures::default();
        while let Some(m) = matches.next() {
            if !editing.checks.passes(m, rope) {
                continue;
            }
            let settings = query.property_settings(m.pattern_index);
            for c in m.captures() {
                let name = names[c.index as usize];
                if name.starts_with('_') {
                    continue;
                }
                let Some(slot) = out.slot(name) else {
                    continue;
                };
                let props = slot.entry(c.node.id()).or_default();
                for p in settings
                    .iter()
                    .filter(|p| p.capture_id.is_none_or(|id| id == c.index as usize))
                {
                    props.insert(p.key.to_string(), p.value.as_deref().map(str::to_owned));
                }
            }
        }
        out
    }
}

/// `tree` parsed again for `rope` (in `ranges` for a layer), reusing it:
/// quick after a small edit. `None` if that can't be done in time.
pub(crate) fn reparse(
    rope: &ropey::Rope,
    tree: &Tree,
    language: &str,
    ranges: Option<&[Range<usize>]>,
) -> Option<Tree> {
    let grammar = crate::code::language(language)?;
    let mut parser = Parser::new();
    parser.set_language(&grammar).ok()?;
    if let Some(ranges) = ranges {
        let ts: Vec<tree_sitter::Range> = ranges
            .iter()
            .map(|r| crate::syntax::ts_range(rope, r))
            .collect();
        parser.set_included_ranges(&ts).ok()?;
    }
    crate::syntax::parse(&mut parser, rope, Some(tree), REPARSE_TIME)
}

/// The node at byte column `col` of `row`.
fn node_at(root: Node, row: usize, col: usize) -> Option<Node> {
    root.descendant_for_point_range(
        Point { row, column: col },
        Point {
            row,
            column: col + 1,
        },
    )
}

/// `node`'s first child of kind `delimiter`, and whether nothing but
/// spaces and delimiters follows it on its line.
fn delimiter<'t>(lines: &Lines, node: Node<'t>, delimiter: &str) -> (Option<Node<'t>>, bool) {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| c.kind() == delimiter);
    let Some(child) = found else {
        return (None, false);
    };
    let text = lines.text(child.start_position().row);
    let after = text.get(child.end_position().column..).unwrap_or("");
    let is_last = after
        .chars()
        .all(|c| c.is_whitespace() || delimiter.contains(c));
    (Some(child), is_last)
}

/// Line text and widths, with tabs `tabstop` wide.
struct Lines<'a> {
    rope: &'a ropey::Rope,
    tabstop: usize,
}

impl Lines<'_> {
    /// Line `line` without its line break.
    fn text(&self, line: usize) -> String {
        if line >= self.rope.len_lines() {
            return String::new();
        }
        let mut s = self.rope.line(line).to_string();
        while s.ends_with(['\n', '\r']) {
            s.pop();
        }
        s
    }

    /// The width of line `line`'s indent, in columns.
    fn width(&self, line: usize) -> usize {
        let mut cols = 0;
        for c in self.text(line).chars() {
            match c {
                ' ' => cols += 1,
                '\t' => cols += self.tabstop - cols % self.tabstop,
                _ => break,
            }
        }
        cols
    }
}

/// The bytes of `text`'s leading whitespace.
fn leading_bytes(text: &str) -> usize {
    text.len() - text.trim_start().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::Syntax;
    use ropey::Rope;

    /// (name, language, shiftwidth, text, line, indent), the indents from
    /// Neovim 0.11 with nvim-treesitter's own `get_indent` on the same cases.
    const GOLDEN: &[(&str, &str, usize, &str, usize, usize)] = &[
        ("rust_fn_body", "rust", 4, "fn f() {\n\n}\n", 1, 4),
        (
            "rust_after_stmt",
            "rust",
            4,
            "fn f() {\n    let x = 1;\n\n}\n",
            2,
            4,
        ),
        (
            "rust_nested_if",
            "rust",
            4,
            "fn f() {\n    if x {\n\n    }\n}\n",
            2,
            8,
        ),
        (
            "rust_closing_brace",
            "rust",
            4,
            "fn f() {\n    x\n        }\n",
            2,
            0,
        ),
        (
            "rust_after_inner_block",
            "rust",
            4,
            "fn f() {\n    if x {\n        y\n    }\n\n}\n",
            4,
            4,
        ),
        ("rust_struct_field", "rust", 4, "struct S {\n\n}\n", 1, 4),
        (
            "rust_call_args",
            "rust",
            4,
            "fn f() {\n    foo(\n\n    );\n}\n",
            2,
            8,
        ),
        (
            "rust_match_arm",
            "rust",
            4,
            "fn f() {\n    match x {\n\n    }\n}\n",
            2,
            8,
        ),
        (
            "rust_impl_fn",
            "rust",
            4,
            "impl S {\n    fn g(&self) {\n\n    }\n}\n",
            2,
            8,
        ),
        ("rust_top_level", "rust", 4, "fn f() {}\n\n", 1, 0),
        (
            "rust_trailing_comment",
            "rust",
            4,
            "fn f() { // hi\n\n}\n",
            1,
            4,
        ),
        (
            "rust_method_chain",
            "rust",
            4,
            "fn f() {\n    let x = a\n\n}\n",
            2,
            4,
        ),
        ("python_def", "python", 4, "def f():\n\n", 1, 4),
        (
            "python_after_stmt",
            "python",
            4,
            "def f():\n    x = 1\n\n",
            2,
            4,
        ),
        (
            "python_after_return",
            "python",
            4,
            "def f():\n    return 1\n\n",
            2,
            0,
        ),
        ("python_if", "python", 4, "def f():\n    if x:\n\n", 2, 8),
        (
            "python_else",
            "python",
            4,
            "def f():\n    if x:\n        y\n        else:\n",
            3,
            4,
        ),
        ("python_list", "python", 4, "x = [\n\n]\n", 1, 4),
        ("python_call_aligned", "python", 4, "foo(a,\n\n", 1, 4),
        (
            "python_class",
            "python",
            4,
            "class A:\n    def f(self):\n        pass\n\n",
            3,
            8,
        ),
        ("js_fn", "javascript", 2, "function f() {\n\n}\n", 1, 2),
        (
            "js_object",
            "javascript",
            2,
            "const o = {\n  a: 1,\n\n};\n",
            2,
            2,
        ),
        (
            "js_closing",
            "javascript",
            2,
            "function f() {\n  if (x) {\n    y();\n    }\n}\n",
            3,
            2,
        ),
        (
            "js_arrow",
            "javascript",
            2,
            "const f = () => {\n\n};\n",
            1,
            2,
        ),
        (
            "ts_interface",
            "typescript",
            2,
            "interface A {\n\n}\n",
            1,
            2,
        ),
        (
            "ts_class_method",
            "typescript",
            2,
            "class A {\n  m(): void {\n\n  }\n}\n",
            2,
            4,
        ),
        (
            "tsx_jsx",
            "tsx",
            2,
            "const a = (\n  <div>\n\n  </div>\n);\n",
            2,
            4,
        ),
        ("bash_if", "bash", 2, "if true; then\n\nfi\n", 1, 2),
        ("bash_function", "bash", 2, "f() {\n\n}\n", 1, 2),
        ("bash_fi", "bash", 2, "if true; then\n  x\n  fi\n", 2, 0),
        ("json_object", "json", 2, "{\n\n}\n", 1, 2),
        ("json_nested", "json", 2, "{\n  \"a\": [\n\n  ]\n}\n", 2, 4),
        ("json_closing", "json", 2, "{\n  \"a\": 1\n  }\n", 2, 0),
        // An unclosed block: Rust's grammar loses it, so Neovim and mNote
        // both give the outer level (auto-pairs close it as it's typed).
        (
            "rust_unclosed_if",
            "rust",
            4,
            "fn f() {\n    let x = 1;\n    if x {\n\n}\n",
            3,
            4,
        ),
        (
            "js_unclosed_if",
            "javascript",
            2,
            "function f() {\n  if (x) {\n\n}\n",
            2,
            4,
        ),
    ];

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

    /// Where mNote's pinned grammar parses a case differently from
    /// Neovim's, and so the same query gives another indent: (name, ours).
    const GRAMMAR_DIFFERS: &[(&str, usize)] = &[
        // tree-sitter-python 0.25.0 puts a half-typed `else:` in an ERROR
        // inside the `if`'s consequence, where Neovim's newer parser gives
        // `(ERROR "else" ":")` beside it, which the `^else` branch matches.
        ("python_else", 8),
    ];

    #[test]
    fn indents_match_neovims() {
        let mut wrong = Vec::new();
        for (name, language, sw, text, line, want) in GOLDEN {
            let want = GRAMMAR_DIFFERS
                .iter()
                .find(|(n, _)| n == name)
                .map_or(want, |(_, ours)| ours);
            let got = snapshot(text, language).indent(*line, *sw, *sw);
            if got != Some(Indent::Columns(*want)) {
                wrong.push(format!("{name}: {got:?}, Neovim says {want}"));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }
}
