//! Golden tests in nvim-treesitter's format: each file in
//! `test-data/highlight/` is parsed as its language (fences become layers),
//! highlighted whole, and checked against the assertions in its comments:
//!
//! ```text
//! let name = "x";
//! // ^ @keyword        the character above the caret is a keyword
//! // <- @keyword       the character above the comment's start is too
//! ```
//!
//! A character passes when its scope is the one named or a longer one
//! under it (`@function` passes `function.method`). Assertion lines are the
//! language's own comments, so they parse as part of the file.

use std::path::PathBuf;

use ropey::Rope;

use crate::syntax::Syntax;

struct Assertion {
    line: usize,
    row: usize,
    column: usize,
    scope: String,
}

/// The assertions in `text`, where comments start with `comment`.
fn assertions(text: &str, comment: &str) -> Vec<Assertion> {
    let mut out = Vec::new();
    let mut code_row = None;
    for (line_no, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let at = line.len() - trimmed.len();
        let rest = trimmed.strip_prefix(comment).map(str::trim_start);
        let parsed = rest.and_then(|rest| {
            if let Some(scope) = rest.strip_prefix("<-") {
                Some((vec![at], scope.trim()))
            } else if rest.starts_with('^') {
                let start = line.find('^')?;
                let carets = line[start..].chars().take_while(|c| *c == '^').count();
                let scope = line[start + carets..].trim();
                Some(((start..start + carets).collect(), scope))
            } else {
                None
            }
        });
        match (parsed, code_row) {
            (Some((columns, scope)), Some(row)) if scope.starts_with('@') => {
                for column in columns {
                    out.push(Assertion {
                        line: line_no + 1,
                        row,
                        column,
                        scope: scope.trim_start_matches('@').to_owned(),
                    });
                }
            }
            (Some(_), _) => {}
            (None, _) => code_row = Some(line_no),
        }
    }
    out
}

/// Checks one file; returns its failures.
fn check(file: &str, language: &str, comment: &str) -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test-data/highlight")
        .join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{file}: {e}"));
    let grammar = crate::code::language(language).unwrap_or_else(|| panic!("{language} grammar"));
    let mut syntax = Syntax::with_injections(&grammar, Some(language)).expect("a syntax map");
    let rope = Rope::from_str(&text);
    let tree = syntax.tree_now(&rope).cloned().expect("parses");
    let layers = syntax.layers_now(&rope).to_vec();
    let spans =
        super::highlight_layers(Some((language, &tree)), &layers, &rope, 0..rope.len_bytes());
    let found = assertions(&text, comment);
    assert!(!found.is_empty(), "{file} has no assertions");
    let mut failures = Vec::new();
    for a in found {
        let byte = rope.line_to_byte(a.row) + a.column;
        let got: Option<String> = spans
            .iter()
            .find(|s| s.range.contains(&byte))
            .map(|s| crate::scopes::name(s.scope).to_string());
        let ok = got
            .as_deref()
            .is_some_and(|g| g == a.scope || g.starts_with(&format!("{}.", a.scope)));
        if !ok {
            let line = text.lines().nth(a.row).unwrap_or("");
            let ch = line
                .get(a.column..)
                .and_then(|s| s.chars().next())
                .unwrap_or(' ');
            failures.push(format!(
                "{file}:{}: {:?} (row {}, col {}) is {got:?}, wanted @{}",
                a.line, ch, a.row, a.column, a.scope
            ));
        }
    }
    failures
}

#[test]
fn golden_files_highlight_as_marked() {
    let mut failures = Vec::new();
    for (file, language, comment) in [
        ("sample.rs", "rust", "//"),
        ("sample.py", "python", "#"),
        ("sample.js", "javascript", "//"),
        ("sample.ts", "typescript", "//"),
        ("sample.tsx", "tsx", "//"),
        ("sample.sh", "bash", "#"),
        ("sample.json", "json", "//"),
        // A Rust fence in a note: its assertions are Rust comments, checked
        // through the fence's injection layer.
        ("sample.md", "markdown", "//"),
    ] {
        failures.extend(check(file, language, comment));
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn assertions_are_read_like_nvim_treesitter() {
    let found = assertions(
        "let x = 1;\n// ^^ @keyword\n//    ^ @variable\n// <- @keyword\n",
        "//",
    );
    let cols: Vec<(usize, usize, &str)> = found
        .iter()
        .map(|a| (a.row, a.column, a.scope.as_str()))
        .collect();
    assert_eq!(
        cols,
        [
            (0, 3, "keyword"),
            (0, 4, "keyword"),
            (0, 6, "variable"),
            (0, 0, "keyword")
        ]
    );
}

#[test]
fn deeper_layers_and_priorities_win() {
    use crate::syntax::Layer;
    // A Rust fence's tokens are painted over its Markdown (which has no
    // highlights here) and stay within the fence.
    let text = "# T\n\n```rust\nfn f() {}\n```\nfn not_code\n";
    let rope = Rope::from_str(text);
    let md = crate::code::language("markdown").expect("markdown");
    let mut syntax = Syntax::with_injections(&md, Some("markdown")).expect("map");
    let tree = syntax.tree_now(&rope).cloned().expect("tree");
    let layers: Vec<Layer> = syntax.layers_now(&rope).to_vec();
    let spans = super::highlight_layers(
        Some(("markdown", &tree)),
        &layers,
        &rope,
        0..rope.len_bytes(),
    );
    let inside = text.find("fn f").expect("there");
    let outside = text.find("fn not").expect("there");
    assert!(spans.iter().any(|s| s.range.contains(&inside)));
    assert!(
        !spans.iter().any(|s| s.range.contains(&outside)),
        "{spans:?}"
    );
}

/// By hand: how long highlighting the visible rows of a large Rust file
/// takes (`cargo test -p mnote-lang --release visible_rows_timing --
/// --ignored --nocapture`).
#[test]
#[ignore]
fn visible_rows_timing() {
    let line = "    pub fn example(&self, value: u32) -> Option<String> { let s = format!(\"{value}\"); Some(s) } // note\n";
    let text: String = std::iter::repeat_n(line, 10_000).collect();
    let rope = Rope::from_str(&text);
    let rust = crate::code::language("rust").expect("rust");
    let mut syntax = Syntax::with_injections(&rust, Some("rust")).expect("map");
    let started = std::time::Instant::now();
    let tree = syntax.tree_now(&rope).cloned().expect("tree");
    println!("parse {} KB: {:?}", text.len() / 1024, started.elapsed());
    for first in [0, 5_000, 9_940] {
        let range = rope.line_to_byte(first)..rope.line_to_byte(first + 60);
        let started = std::time::Instant::now();
        let spans = super::highlight_layers(Some(("rust", &tree)), &[], &rope, range);
        println!(
            "rows {first}..+60: {} spans in {:?}",
            spans.len(),
            started.elapsed()
        );
    }
}
