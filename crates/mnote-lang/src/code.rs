//! Languages that can be parsed: their tree-sitter grammars (built in, or
//! loaded at run time) and their compiled highlight queries.
//!
//! A language's grammar and query are loaded once, on first use. The query
//! is the language pack's (Neovim's, the user's or a plugin's) when it fits
//! the grammar, else the grammar crate's own; see [`crate::highlight`] for
//! how it colours text.

use std::sync::{Arc, OnceLock, RwLock};

use tree_sitter::{Language, Query};

use crate::predicates::Checks;
use crate::scopes::Scope;

/// A compiled highlight query and what colouring with it needs.
pub(crate) struct Grammar {
    pub(crate) query: Query,
    pub(crate) checks: Checks,
    /// Each capture's scope.
    pub(crate) scopes: Vec<Scope>,
    /// Each pattern's `#set! priority` (100 when not set).
    pub(crate) priorities: Vec<i32>,
    /// Where patterns overlap, the later one wins (Neovim's queries) rather
    /// than the first (tree-sitter's own, as the grammar crates write them).
    pub(crate) later_wins: bool,
}

/// A query text to try, and how its patterns rank.
#[derive(PartialEq)]
struct Candidate {
    text: String,
    later_wins: bool,
    /// Where it came from, for a report when it doesn't fit the grammar.
    from: &'static str,
}

/// Compiles the first candidate that fits `language`. A candidate that
/// doesn't fit (Neovim's query for a newer version of the grammar, say) is
/// reported, and the next one is tried.
fn load(name: &str, language: &Language, candidates: Vec<Candidate>) -> Option<Grammar> {
    let count = candidates.len();
    for (i, candidate) in candidates.into_iter().enumerate() {
        if candidate.text.trim().is_empty() {
            continue;
        }
        let mut query = match Query::new(language, &candidate.text) {
            Ok(q) => q,
            Err(err) => {
                if i + 1 < count {
                    crate::languages::report_one(crate::LangError::new(
                        crate::error::Origin::Builtin,
                        Some(name),
                        format!(
                            "{}'s highlights don't fit mNote's {name} grammar ({}), so its own are used",
                            candidate.from, err.message
                        ),
                    ));
                }
                continue;
            }
        };
        let (checks, unknown) = Checks::new(&mut query);
        if !unknown.is_empty() {
            crate::languages::report_one(crate::LangError::new(
                crate::error::Origin::Builtin,
                Some(name),
                format!(
                    "its highlights use {}, which mNote doesn't know",
                    unknown.join(", ")
                ),
            ));
        }
        let scopes = query
            .capture_names()
            .iter()
            .map(|n| crate::scopes::intern(n))
            .collect();
        let priorities = (0..query.pattern_count())
            .map(|p| {
                query
                    .property_settings(p)
                    .iter()
                    .find(|s| &*s.key == "priority")
                    .and_then(|s| s.value.as_deref()?.parse().ok())
                    .unwrap_or(100)
            })
            .collect();
        return Some(Grammar {
            query,
            checks,
            scopes,
            priorities,
            later_wins: candidate.later_wins,
        });
    }
    None
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

pub(crate) struct Entry {
    def: LanguageDef,
    /// The language pack it comes from, whose highlight query it uses.
    pack: Option<String>,
    /// The grammar crate's own highlight query, used for a built-in grammar
    /// until Neovim's queries (with their own predicates and capture
    /// names) are supported for highlighting (AMI-216).
    fallback: Vec<String>,
    /// The same, for its injections query.
    fallback_injections: Vec<String>,
    /// How the grammar's own query ranks overlapping patterns.
    fallback_later_wins: bool,
    /// A grammar loaded at run time (from Neovim, say): its pack's queries
    /// from any layer fit it, Neovim's included.
    loaded: bool,
    /// The compiled query, on first use.
    grammar: OnceLock<Option<Grammar>>,
    /// Set once its query started compiling.
    started: std::sync::atomic::AtomicBool,
}

impl Entry {
    /// Whether `other` would compile to the same thing.
    fn same_as(&self, other: &Entry) -> bool {
        self.def.name == other.def.name
            && self.def.aliases == other.def.aliases
            && self.def.language == other.def.language
            && self.def.highlights == other.def.highlights
            && self.pack == other.pack
            && self.fallback == other.fallback
            && self.fallback_injections == other.fallback_injections
            && self.fallback_later_wins == other.fallback_later_wins
            && self.loaded == other.loaded
            && self.candidates() == other.candidates()
    }

    pub(crate) fn grammar(&self) -> Option<&Grammar> {
        self.started
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.grammar
            .get_or_init(|| load(&self.def.name, &self.def.language, self.candidates()))
            .as_ref()
    }

    /// The highlight queries to try, best first: the pack's from Neovim, the
    /// user or a plugin (they rank later-wins, as Neovim writes them), then
    /// the grammar crate's own (first-wins).
    fn candidates(&self) -> Vec<Candidate> {
        let mut out = Vec::new();
        if let Some(pack) = &self.pack
            && let Some(q) = crate::languages::query(pack, "highlights")
            && (self.loaded || q.layer != crate::queries::Layer::Builtin)
        {
            let from = match q.layer {
                crate::queries::Layer::Neovim => "Neovim",
                crate::queries::Layer::User => "your language pack",
                crate::queries::Layer::Builtin => "the built-in pack",
            };
            out.push(Candidate {
                text: q.text,
                later_wins: q.layer != crate::queries::Layer::Builtin,
                from,
            });
        }
        let own = if self.pack.is_some() {
            &self.fallback
        } else {
            &self.def.highlights
        };
        out.push(Candidate {
            text: own.join("\n"),
            later_wins: self.fallback_later_wins,
            from: "the grammar",
        });
        out
    }

    /// A query of `kind` for this language: for a grammar loaded at run
    /// time, its pack's (from any layer, Neovim's included); for a built-in
    /// one, the user's or a plugin's, else the grammar's own.
    fn query_text(&self, kind: &str) -> Vec<String> {
        let fallback = if kind == "highlights" {
            &self.fallback
        } else {
            &self.fallback_injections
        };
        let Some(pack) = &self.pack else {
            return if kind == "highlights" {
                self.def.highlights.clone()
            } else {
                Vec::new()
            };
        };
        match crate::languages::query(pack, kind) {
            Some(q) if self.loaded || q.layer == crate::queries::Layer::User => vec![q.text],
            _ => fallback.clone(),
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
    injections: Vec<&'static str>,
    /// Its `tags` query (definitions, for the outline), the grammar
    /// crate's own.
    tags: Vec<&'static str>,
    /// Whether its highlight query expects later patterns to win where they
    /// overlap. The grammar crates don't agree (Python's puts its catch-all
    /// `(identifier) @variable` first, JSON its specific key pattern first);
    /// the golden tests check each.
    later_wins: bool,
}

/// The grammars built in: Markdown (parsed, for notes) and the code-block
/// languages. Which file names, fences and aliases use them is in the
/// language packs (`languages.toml`).
fn builtin_grammars() -> Vec<Builtin> {
    let b = |grammar, language: Language, highlights: &[&'static str]| Builtin {
        grammar,
        language,
        highlights: highlights.to_vec(),
        injections: Vec::new(),
        tags: Vec::new(),
        later_wins: true,
    };
    vec![
        // Its code fences, HTML blocks and frontmatter become injection
        // layers (see `syntax`).
        Builtin {
            injections: vec![tree_sitter_md::INJECTION_QUERY_BLOCK],
            ..b("markdown", tree_sitter_md::LANGUAGE.into(), &[])
        },
        Builtin {
            tags: vec![tree_sitter_rust::TAGS_QUERY],
            ..b(
                "rust",
                tree_sitter_rust::LANGUAGE.into(),
                &[tree_sitter_rust::HIGHLIGHTS_QUERY],
            )
        },
        Builtin {
            tags: vec![tree_sitter_python::TAGS_QUERY],
            ..b(
                "python",
                tree_sitter_python::LANGUAGE.into(),
                &[tree_sitter_python::HIGHLIGHTS_QUERY],
            )
        },
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
        Builtin {
            later_wins: false,
            ..b(
                "json",
                tree_sitter_json::LANGUAGE.into(),
                &[tree_sitter_json::HIGHLIGHTS_QUERY],
            )
        },
    ]
}

/// The built-in grammars' highlight queries, as query files for the
/// registry (language, kind, text).
pub(crate) fn builtin_queries() -> Vec<(String, &'static str, String)> {
    let mut out = Vec::new();
    for b in builtin_grammars() {
        if !b.highlights.is_empty() {
            out.push((b.grammar.to_owned(), "highlights", b.highlights.join("\n")));
        }
        if !b.injections.is_empty() {
            out.push((b.grammar.to_owned(), "injections", b.injections.join("\n")));
        }
        // TypeScript's tags add to JavaScript's, as its highlights do.
        let tags = match b.grammar {
            "javascript" => vec![tree_sitter_javascript::TAGS_QUERY],
            "typescript" | "tsx" => vec![
                tree_sitter_javascript::TAGS_QUERY,
                tree_sitter_typescript::TAGS_QUERY,
            ],
            _ => b.tags.clone(),
        };
        if !tags.is_empty() {
            out.push((b.grammar.to_owned(), "tags", tags.join("\n")));
        }
    }
    for (language, kind, text) in crate::editing::BUNDLED {
        out.push(((*language).to_owned(), *kind, (*text).to_owned()));
    }
    out
}

/// Whether `lang`'s queries may come from Neovim's folders: only for a
/// grammar loaded at run time, which is likely the one those queries were
/// written for. Built-in grammars use mNote's own, the user's and plugins'.
pub(crate) fn takes_neovim_queries(lang: &str) -> bool {
    for_lang(lang).is_some_and(|e| e.loaded)
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
            let owned = |qs: &[&str]| qs.iter().map(|q| (*q).to_owned()).collect::<Vec<_>>();
            let (language, fallback, fallback_injections, loaded, fallback_later_wins) =
                match builtin {
                    Some(b) => (
                        b.language.clone(),
                        owned(&b.highlights),
                        owned(&b.injections),
                        false,
                        b.later_wins,
                    ),
                    None => (
                        crate::grammar::loaded_language(&pack.grammar)?,
                        Vec::new(),
                        Vec::new(),
                        true,
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
                fallback_injections,
                fallback_later_wins,
                loaded,
                grammar: OnceLock::new(),
                started: Default::default(),
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
                fallback_injections: Vec::new(),
                fallback_later_wins: true,
                loaded: false,
                grammar: OnceLock::new(),
                started: Default::default(),
            }));
        }
    }
    out
}

/// Rebuilds the registry after the language packs were reloaded. A
/// language whose grammar and queries didn't change keeps its compiled (or
/// compiling) query: packs load at start, often just after a file asked
/// for colours.
pub(crate) fn rebuild() {
    let mut fresh = entries();
    let old: Vec<Arc<Entry>> = registry()
        .read()
        .map(|e| {
            e.iter()
                .filter(|e| e.started.load(std::sync::atomic::Ordering::Relaxed))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    for entry in &mut fresh {
        if let Some(same) = old.iter().find(|o| o.same_as(entry)) {
            *entry = same.clone();
        }
    }
    if let Ok(mut entries_now) = registry().write() {
        *entries_now = fresh;
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

/// `name`'s grammar and its `injections` query, for the syntax map.
pub(crate) fn injections(name: &str) -> Option<(Language, String)> {
    let entry = for_lang(name)?;
    let text = entry.query_text("injections").join("\n");
    (!text.trim().is_empty()).then(|| (entry.def.language.clone(), text))
}

/// Whether `lang` has a grammar (it can be parsed), without compiling its
/// highlight query.
pub fn has_grammar(lang: &str) -> bool {
    for_lang(lang).is_some()
}

/// Whether `lang`'s highlight query is compiled already, so colouring
/// won't stop to compile it (compiling takes milliseconds: do it off the UI
/// thread with [`supported`] first).
pub fn ready(lang: &str) -> bool {
    for_lang(lang).is_some_and(|e| e.grammar.get().is_some())
}

/// Whether `lang` gets colours (compiling its query on first use).
pub fn supported(lang: &str) -> bool {
    for_lang(lang).is_some_and(|e| e.grammar().is_some())
}

/// The compiled highlight query of `lang` (a name or alias), if it has a
/// grammar and a query.
pub(crate) fn grammar_for(lang: &str) -> Option<Arc<Entry>> {
    let entry = for_lang(lang)?;
    entry.grammar()?;
    Some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let spans = crate::highlight::highlight_text("rrt", "fn main() {}").expect("coloured");
        assert!(
            spans.iter().any(|(_, s)| s.starts_with("keyword")),
            "{spans:?}"
        );
    }

    #[test]
    fn every_built_in_language_loads_and_colours() {
        for (lang, code) in [
            ("rust", "fn main() { let x = \"hi\"; } // done"),
            ("python", "def f(x):\n    return 'a'  # c"),
            ("js", "const f = () => 'x'; // c"),
            ("ts", "let n: number = 1;"),
            ("tsx", "const a = <b>x</b>;"),
            ("sh", "echo \"hi\" # c"),
            ("json", "{\"a\": 1, \"b\": true}"),
        ] {
            let got = crate::highlight::highlight_text(lang, code)
                .unwrap_or_else(|| panic!("{lang} has a grammar"));
            assert!(!got.is_empty(), "{lang} gets colours");
        }
        assert!(crate::highlight::highlight_text("cobol", "x").is_none());
        assert!(!supported("cobol"));
    }

    #[test]
    fn a_rebuild_keeps_compiled_queries_that_didnt_change() {
        // A language of its own: other tests reload the packs meanwhile.
        register_language(LanguageDef {
            name: "rebuild-test".into(),
            aliases: Vec::new(),
            language: tree_sitter_json::LANGUAGE.into(),
            highlights: vec!["(string) @string".into()],
        });
        assert!(supported("rebuild-test"));
        rebuild();
        assert!(ready("rebuild-test"), "recompiled for nothing");
    }
}
