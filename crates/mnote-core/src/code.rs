//! Coloring code blocks: tree-sitter grammars and their highlight queries
//! for a short list of languages (Rust, Python, JavaScript, TypeScript,
//! shell, JSON). Any other language shows plain.
//!
//! A language's grammar and query are loaded once, on first use. Callers
//! cache results per block; highlighting a block parses only that block.

use std::ops::Range;
use std::sync::OnceLock;

use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

/// What a piece of code is, for its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeStyle {
    Keyword,
    String,
    Comment,
    Function,
    Type,
    Number,
    Constant,
    Property,
    Punctuation,
}

impl CodeStyle {
    /// The style for a highlight capture name (`keyword.control` → keyword).
    fn from_capture(name: &str) -> Option<Self> {
        let head = name.split('.').next().unwrap_or(name);
        Some(match head {
            "keyword" | "conditional" | "repeat" | "include" | "exception" => CodeStyle::Keyword,
            "string" | "escape" | "character" => CodeStyle::String,
            "comment" => CodeStyle::Comment,
            "function" | "method" | "constructor" => CodeStyle::Function,
            "type" | "tag" => CodeStyle::Type,
            "number" | "float" => CodeStyle::Number,
            "constant" | "boolean" => CodeStyle::Constant,
            "property" | "attribute" | "label" => CodeStyle::Property,
            "operator" | "punctuation" | "delimiter" => CodeStyle::Punctuation,
            "variable" if name == "variable.builtin" => CodeStyle::Constant,
            _ => return None,
        })
    }
}

struct Grammar {
    language: Language,
    query: Query,
}

fn grammar(
    slot: &'static OnceLock<Option<Grammar>>,
    make: fn() -> Option<Grammar>,
) -> Option<&'static Grammar> {
    slot.get_or_init(make).as_ref()
}

fn load(language: Language, queries: &[&str]) -> Option<Grammar> {
    let source = queries.join("\n");
    let query = Query::new(&language, &source)
        .or_else(|_| Query::new(&language, queries.last().copied().unwrap_or_default()))
        .ok()?;
    Some(Grammar { language, query })
}

/// The grammar for a fence's language name, if mNote has one.
fn for_lang(lang: &str) -> Option<&'static Grammar> {
    static RUST: OnceLock<Option<Grammar>> = OnceLock::new();
    static PYTHON: OnceLock<Option<Grammar>> = OnceLock::new();
    static JS: OnceLock<Option<Grammar>> = OnceLock::new();
    static TS: OnceLock<Option<Grammar>> = OnceLock::new();
    static TSX: OnceLock<Option<Grammar>> = OnceLock::new();
    static BASH: OnceLock<Option<Grammar>> = OnceLock::new();
    static JSON: OnceLock<Option<Grammar>> = OnceLock::new();
    match lang {
        "rust" | "rs" => grammar(&RUST, || {
            load(
                tree_sitter_rust::LANGUAGE.into(),
                &[tree_sitter_rust::HIGHLIGHTS_QUERY],
            )
        }),
        "python" | "py" => grammar(&PYTHON, || {
            load(
                tree_sitter_python::LANGUAGE.into(),
                &[tree_sitter_python::HIGHLIGHTS_QUERY],
            )
        }),
        "javascript" | "js" | "jsx" | "mjs" => grammar(&JS, || {
            load(
                tree_sitter_javascript::LANGUAGE.into(),
                &[
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                ],
            )
        }),
        "typescript" | "ts" => grammar(&TS, || {
            load(
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                &[
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY,
                ],
            )
        }),
        "tsx" => grammar(&TSX, || {
            load(
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                &[
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY,
                ],
            )
        }),
        "bash" | "sh" | "shell" | "zsh" | "console" => grammar(&BASH, || {
            load(
                tree_sitter_bash::LANGUAGE.into(),
                &[tree_sitter_bash::HIGHLIGHT_QUERY],
            )
        }),
        "json" | "jsonc" => grammar(&JSON, || {
            load(
                tree_sitter_json::LANGUAGE.into(),
                &[tree_sitter_json::HIGHLIGHTS_QUERY],
            )
        }),
        _ => None,
    }
}

/// Whether `lang` gets colours.
pub fn supported(lang: &str) -> bool {
    for_lang(lang).is_some()
}

/// The coloured byte ranges of `code` in `lang`, in order and not
/// overlapping, or `None` for a language without a grammar. Where captures
/// overlap, the first pattern in the query wins (as in tree-sitter's own
/// highlighter).
pub fn highlight(lang: &str, code: &str) -> Option<Vec<(Range<usize>, CodeStyle)>> {
    let grammar = for_lang(lang)?;
    let mut parser = Parser::new();
    parser.set_language(&grammar.language).ok()?;
    let tree = parser.parse(code, None)?;
    let names = grammar.query.capture_names();
    // Per byte, the pattern index and style that painted it.
    let mut paint: Vec<Option<(usize, CodeStyle)>> = vec![None; code.len()];
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&grammar.query, tree.root_node(), code.as_bytes());
    while let Some(m) = matches.next() {
        for capture in m.captures() {
            let Some(style) = CodeStyle::from_capture(names[capture.index as usize]) else {
                continue;
            };
            for slot in &mut paint[capture.node.byte_range()] {
                if slot.is_none_or(|(pattern, _)| m.pattern_index < pattern) {
                    *slot = Some((m.pattern_index, style));
                }
            }
        }
    }
    let mut out: Vec<(Range<usize>, CodeStyle)> = Vec::new();
    for (i, slot) in paint.iter().enumerate() {
        let Some((_, style)) = slot else { continue };
        match out.last_mut() {
            Some((range, last)) if range.end == i && last == style => range.end = i + 1,
            _ => out.push((i..i + 1, *style)),
        }
    }
    // Keep ranges on character boundaries.
    out.retain(|(r, _)| code.is_char_boundary(r.start) && code.is_char_boundary(r.end));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styles(lang: &str, code: &str) -> Vec<(String, CodeStyle)> {
        highlight(lang, code)
            .expect("a grammar")
            .into_iter()
            .map(|(r, s)| (code[r].to_owned(), s))
            .collect()
    }

    #[test]
    fn every_language_loads_and_colours() {
        for (lang, code) in [
            ("rust", "fn main() { let x = \"hi\"; } // done"),
            ("python", "def f(x):\n    return 'a'  # c"),
            ("js", "const f = () => 'x'; // c"),
            ("ts", "let n: number = 1;"),
            ("tsx", "const a = <b>x</b>;"),
            ("sh", "echo \"hi\" # c"),
            ("json", "{\"a\": 1, \"b\": true}"),
        ] {
            let got = highlight(lang, code).unwrap_or_else(|| panic!("{lang} has a grammar"));
            assert!(!got.is_empty(), "{lang} gets colours");
        }
        assert!(highlight("cobol", "x").is_none());
        assert!(!supported("cobol"));
    }

    #[test]
    fn rust_keywords_strings_and_comments() {
        let got = styles("rust", "let x = \"hi\"; // c");
        assert!(got.contains(&("let".into(), CodeStyle::Keyword)), "{got:?}");
        assert!(
            got.contains(&("\"hi\"".into(), CodeStyle::String)),
            "{got:?}"
        );
        assert!(
            got.contains(&("// c".into(), CodeStyle::Comment)),
            "{got:?}"
        );
    }
}
