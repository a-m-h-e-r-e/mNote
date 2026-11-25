//! Coloring code blocks: tree-sitter grammars and their highlight queries
//! for a short list of languages (Rust, Python, JavaScript, TypeScript,
//! shell, JSON). Any other language shows plain.
//!
//! A language's grammar and query are loaded once, on first use. Callers
//! cache results per block; highlighting a block parses only that block.

use std::ops::Range;
use std::sync::{Arc, OnceLock, RwLock};

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

fn load(language: Language, queries: &[String]) -> Option<Grammar> {
    if queries.is_empty() {
        return None;
    }
    let source = queries.join("\n");
    let query = Query::new(&language, &source)
        .or_else(|_| Query::new(&language, queries.last().map_or("", String::as_str)))
        .ok()?;
    Some(Grammar { language, query })
}

/// A language mNote can parse, and colour when it has highlight queries.
#[derive(Debug, Clone)]
pub struct LanguageDef {
    /// Its name (`rust`), as filetypes and fences name it.
    pub name: String,
    /// Other names fences use (`rs`).
    pub aliases: Vec<String>,
    pub language: Language,
    /// Highlight queries, joined in order (`javascript`'s then `jsx`'s).
    /// None: parsed, but not coloured.
    pub highlights: Vec<String>,
}

struct Entry {
    def: LanguageDef,
    /// The language pack it comes from, whose highlight query it uses.
    pack: Option<String>,
    /// The grammar crate's own highlight query, used for a built-in grammar
    /// until Neovim's queries (with their own predicates and capture
    /// names) are supported for highlighting (AMI-216).
    fallback: Vec<String>,
    /// A grammar loaded at run time (from Neovim, say): its pack's queries
    /// from any layer fit it, Neovim's included.
    loaded: bool,
    /// The compiled query, on first use.
    grammar: OnceLock<Option<Grammar>>,
}

impl Entry {
    fn grammar(&self) -> Option<&Grammar> {
        self.grammar
            .get_or_init(|| load(self.def.language.clone(), &self.highlights()))
            .as_ref()
    }

    /// The highlight query: a pack's own (from the user or a plugin), or the
    /// grammar's.
    fn highlights(&self) -> Vec<String> {
        let Some(pack) = &self.pack else {
            return self.def.highlights.clone();
        };
        match crate::languages::query(pack, "highlights") {
            Some(q) if self.loaded || q.layer == crate::queries::Layer::User => vec![q.text],
            _ => self.fallback.clone(),
        }
    }

    fn named(&self, name: &str) -> bool {
        self.def.name.eq_ignore_ascii_case(name)
            || self
                .def
                .aliases
                .iter()
                .any(|a| a.eq_ignore_ascii_case(name))
    }
}

/// A language with a grammar built into mNote: its grammar and its
/// highlight query (the grammar crate's own).
struct Builtin {
    grammar: &'static str,
    language: Language,
    highlights: Vec<&'static str>,
}

/// The grammars built in: Markdown (parsed, for notes) and the code-block
/// languages. Which file names, fences and aliases use them is in the
/// language packs (`languages.toml`).
fn builtin_grammars() -> Vec<Builtin> {
    let b = |grammar, language: Language, highlights: &[&'static str]| Builtin {
        grammar,
        language,
        highlights: highlights.to_vec(),
    };
    vec![
        b("markdown", tree_sitter_md::LANGUAGE.into(), &[]),
        b(
            "rust",
            tree_sitter_rust::LANGUAGE.into(),
            &[tree_sitter_rust::HIGHLIGHTS_QUERY],
        ),
        b(
            "python",
            tree_sitter_python::LANGUAGE.into(),
            &[tree_sitter_python::HIGHLIGHTS_QUERY],
        ),
        b(
            "javascript",
            tree_sitter_javascript::LANGUAGE.into(),
            &[
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
            ],
        ),
        b(
            "typescript",
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            &[
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
            ],
        ),
        b(
            "tsx",
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            &[
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
            ],
        ),
        b(
            "bash",
            tree_sitter_bash::LANGUAGE.into(),
            &[tree_sitter_bash::HIGHLIGHT_QUERY],
        ),
        b(
            "json",
            tree_sitter_json::LANGUAGE.into(),
            &[tree_sitter_json::HIGHLIGHTS_QUERY],
        ),
    ]
}

/// The built-in grammars' highlight queries, as query files for the
/// registry (language, kind, text).
pub(crate) fn builtin_queries() -> Vec<(String, &'static str, String)> {
    builtin_grammars()
        .into_iter()
        .filter(|b| !b.highlights.is_empty())
        .map(|b| (b.grammar.to_owned(), "highlights", b.highlights.join("\n")))
        .collect()
}

/// Languages added with [`register_language`], kept across rebuilds.
fn registered() -> &'static RwLock<Vec<LanguageDef>> {
    static REGISTERED: OnceLock<RwLock<Vec<LanguageDef>>> = OnceLock::new();
    REGISTERED.get_or_init(RwLock::default)
}

/// The languages that can be parsed, shared by every thread: the language
/// packs whose grammar is built in, then those registered in code.
fn registry() -> &'static RwLock<Vec<Arc<Entry>>> {
    static REGISTRY: OnceLock<RwLock<Vec<Arc<Entry>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(entries()))
}

fn entries() -> Vec<Arc<Entry>> {
    let grammars = builtin_grammars();
    let mut out: Vec<Arc<Entry>> = crate::languages::packs()
        .into_iter()
        .filter_map(|pack| {
            let builtin = grammars.iter().find(|b| b.grammar == pack.grammar);
            let (language, fallback, loaded) = match builtin {
                Some(b) => (
                    b.language.clone(),
                    b.highlights.iter().map(|q| (*q).to_owned()).collect(),
                    false,
                ),
                None => (
                    crate::grammar::loaded_language(&pack.grammar)?,
                    Vec::new(),
                    true,
                ),
            };
            Some(Arc::new(Entry {
                def: LanguageDef {
                    name: pack.name.clone(),
                    aliases: pack.aliases.clone(),
                    language,
                    highlights: Vec::new(),
                },
                pack: Some(pack.name),
                fallback,
                loaded,
                grammar: OnceLock::new(),
            }))
        })
        .collect();
    if let Ok(defs) = registered().read() {
        for def in defs.iter() {
            out.retain(|e| e.def.name != def.name);
            out.push(Arc::new(Entry {
                def: def.clone(),
                pack: None,
                fallback: Vec::new(),
                loaded: false,
                grammar: OnceLock::new(),
            }));
        }
    }
    out
}

/// Rebuilds the registry after the language packs were reloaded.
pub(crate) fn rebuild() {
    if let Ok(mut entries_now) = registry().write() {
        *entries_now = entries();
    }
}

/// Adds a language, or replaces the one with its name: a grammar (a Cargo
/// dependency, or one loaded at run time) and its highlight queries.
pub fn register_language(def: LanguageDef) {
    if let Ok(mut defs) = registered().write() {
        defs.retain(|d| d.name != def.name);
        defs.push(def);
    }
    rebuild();
}

/// The tree-sitter language called `name` (or one of its aliases), for
/// parsing a whole file.
pub fn language(name: &str) -> Option<Language> {
    for_lang(name).map(|e| e.def.language.clone())
}

fn for_lang(lang: &str) -> Option<Arc<Entry>> {
    registry()
        .read()
        .ok()?
        .iter()
        .find(|e| e.named(lang))
        .cloned()
}

/// Whether `lang` gets colours.
pub fn supported(lang: &str) -> bool {
    for_lang(lang).is_some_and(|e| e.grammar().is_some())
}

/// The coloured byte ranges of `code` in `lang`, in order and not
/// overlapping, or `None` for a language without a grammar. Where captures
/// overlap, the first pattern in the query wins (as in tree-sitter's own
/// highlighter).
pub fn highlight(lang: &str, code: &str) -> Option<Vec<(Range<usize>, CodeStyle)>> {
    let entry = for_lang(lang)?;
    let grammar = entry.grammar()?;
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

    #[test]
    fn the_registry_names_languages_and_takes_new_ones() {
        assert!(supported("rs") && supported("rust"));
        assert!(!supported("markdown"), "parsed, not coloured");
        assert!(language("markdown").is_some());
        assert!(language("klingon").is_none());
        register_language(LanguageDef {
            name: "rust-registry-test".into(),
            aliases: vec!["rrt".into()],
            language: tree_sitter_rust::LANGUAGE.into(),
            highlights: vec![tree_sitter_rust::HIGHLIGHTS_QUERY.into()],
        });
        let spans = highlight("rrt", "fn main() {}").expect("coloured");
        assert!(spans.iter().any(|(_, s)| *s == CodeStyle::Keyword));
    }

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
