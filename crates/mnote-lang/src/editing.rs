//! Editing queries (AMI-232): the indent, fold, locals and text-object
//! queries the editing services read (code-support.md, section 5).
//!
//! The grammars built into mNote come with their own, from nvim-treesitter
//! and nvim-treesitter-textobjects (`queries/README.md` says which commit).
//! For those grammars Neovim's folders are skipped, as for highlighting:
//! they're written for nvim-treesitter's grammar versions, not mNote's. Your
//! language packs and plugins still replace or extend them. A grammar
//! loaded at run time uses every layer, Neovim's included.
//!
//! Each query compiles once per language and kind, the first time it's
//! asked for; one that doesn't fit its grammar is reported and treated as
//! missing, so the feature falls back rather than failing.

use std::sync::Arc;

use tree_sitter::Query;

use crate::error::Origin;
use crate::predicates::Checks;

/// The kinds of editing query.
pub const KINDS: [&str; 4] = ["indents", "folds", "locals", "textobjects"];

/// A compiled editing query, with Neovim's predicates it uses.
#[derive(Debug)]
pub struct EditingQuery {
    pub query: Query,
    pub checks: Checks,
}

/// `language`'s editing query of `kind` (one of [`KINDS`]), compiled, or
/// `None` when it has none or it doesn't fit the grammar.
pub fn query(language: &str, kind: &str) -> Option<Arc<EditingQuery>> {
    crate::languages::editing_query(language, kind)
}

/// Compiles `text` for `language`'s grammar, reporting what doesn't fit.
pub(crate) fn compile(language: &str, kind: &str, text: &str) -> Option<EditingQuery> {
    let grammar = crate::code::language(language)?;
    match Query::new(&grammar, text) {
        Ok(mut query) => {
            let (checks, unknown) = Checks::new(&mut query);
            if !unknown.is_empty() {
                crate::languages::report_one(crate::LangError::new(
                    Origin::Builtin,
                    Some(language),
                    format!(
                        "its {kind} query uses {}, which mNote doesn't know",
                        unknown.join(", ")
                    ),
                ));
            }
            Some(EditingQuery { query, checks })
        }
        Err(err) => {
            crate::languages::report_one(crate::LangError::new(
                Origin::Builtin,
                Some(language),
                format!(
                    "its {kind} query doesn't fit mNote's {language} grammar (line {}: {})",
                    err.row + 1,
                    err.message
                ),
            ));
            None
        }
    }
}

macro_rules! bundled {
    ($($lang:literal: [$($kind:literal),*],)*) => {
        &[$($(
            (
                $lang,
                $kind,
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"), "/queries/", $lang, "/", $kind, ".scm"
                )),
            ),
        )*)*]
    };
}

/// The bundled files: language (or a base such as `ecma` that others
/// inherit), kind and text.
pub(crate) const BUNDLED: &[(&str, &str, &str)] = bundled! {
    "bash": ["folds", "indents", "locals", "textobjects"],
    "ecma": ["folds", "indents", "locals", "textobjects"],
    "javascript": ["folds", "indents", "locals", "textobjects"],
    "json": ["folds", "indents", "locals", "textobjects"],
    "jsx": ["folds", "indents", "textobjects"],
    "markdown": ["folds", "indents", "textobjects"],
    "python": ["folds", "indents", "locals", "textobjects"],
    "rust": ["folds", "indents", "locals", "textobjects"],
    "tsx": ["folds", "indents", "locals", "textobjects"],
    "typescript": ["folds", "indents", "locals", "textobjects"],
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-in grammars, by the language names packs give them.
    const LANGUAGES: [&str; 8] = [
        "rust",
        "python",
        "javascript",
        "typescript",
        "tsx",
        "bash",
        "json",
        "markdown",
    ];

    #[test]
    fn every_bundled_query_compiles_for_its_grammar() {
        let mut failed = Vec::new();
        for language in LANGUAGES {
            for kind in KINDS {
                let Some(resolved) = crate::languages::editing_query_text(language, kind) else {
                    continue;
                };
                let grammar = crate::code::language(language).expect("a built-in grammar");
                match Query::new(&grammar, &resolved.text) {
                    Ok(mut query) => {
                        // Every predicate it uses is one mNote evaluates.
                        let (_, unknown) = Checks::new(&mut query);
                        if !unknown.is_empty() {
                            failed.push(format!("{language} {kind}: {}", unknown.join(", ")));
                        }
                    }
                    Err(err) => failed.push(format!(
                        "{language} {kind}, line {}: {}",
                        err.row + 1,
                        err.message
                    )),
                }
            }
        }
        assert!(failed.is_empty(), "{}", failed.join("\n"));
    }

    #[test]
    fn every_built_in_code_language_has_each_kind() {
        for language in LANGUAGES {
            for kind in KINDS {
                // Markdown has no variables or definitions to track.
                let missing = language == "markdown" && kind == "locals";
                assert_eq!(
                    query(language, kind).is_some(),
                    !missing,
                    "{language} {kind}"
                );
            }
        }
    }

    #[test]
    fn javascript_inherits_ecmascript_and_jsx() {
        let text = crate::languages::editing_query_text("javascript", "textobjects")
            .expect("javascript has text objects")
            .text;
        let ecma = BUNDLED
            .iter()
            .find(|(l, k, _)| *l == "ecma" && *k == "textobjects")
            .expect("ecma's")
            .2;
        assert!(text.contains(ecma.trim()), "ecma's text objects come first");
    }
}
