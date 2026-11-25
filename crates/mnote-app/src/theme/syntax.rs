//! Syntax colours (ADR-0004): highlight capture names to colours.
//!
//! Every built-in theme maps nvim-treesitter's standard captures to its
//! colour tokens ([`DEFAULTS`]); a theme file's `[syntax]` table adds or
//! overrides any of them, with a token's name or a colour:
//!
//! ```toml
//! [syntax]
//! "@keyword.return" = "danger"
//! "@function" = "#61afef"
//! ```
//!
//! A capture without an entry falls back by dropping its last part
//! (`@keyword.function` → `@keyword`), then draws plain. Lookups are cached
//! per scope, so each name is resolved once.

use std::collections::HashMap;
use std::sync::RwLock;

use mnote_core::config::SyntaxColor;
use mnote_core::lang::scopes::{self, Scope};

use super::tokens::Palette;

/// nvim-treesitter's standard captures (and the older names the grammar
/// crates use), and the colour token each is drawn in.
pub const DEFAULTS: &[(&str, &str)] = &[
    ("keyword", "syn_keyword"),
    ("conditional", "syn_keyword"),
    ("repeat", "syn_keyword"),
    ("include", "syn_keyword"),
    ("exception", "syn_keyword"),
    ("storageclass", "syn_keyword"),
    ("string", "syn_code"),
    ("character", "syn_code"),
    ("escape", "syn_code"),
    ("comment", "ink_3"),
    ("comment.error", "danger"),
    ("function", "syn_link"),
    ("method", "syn_link"),
    ("constructor", "syn_link"),
    ("type", "syn_strong"),
    ("tag", "syn_strong"),
    ("module", "syn_strong"),
    ("namespace", "syn_strong"),
    ("number", "syn_strong"),
    ("float", "syn_strong"),
    ("constant", "syn_url"),
    ("boolean", "syn_url"),
    ("variable.builtin", "syn_url"),
    ("property", "ink"),
    ("attribute", "ink"),
    ("label", "ink"),
    ("field", "ink"),
    ("variable.member", "ink"),
    ("tag.attribute", "ink"),
    ("operator", "ink_2"),
    ("punctuation", "ink_2"),
    ("delimiter", "ink_2"),
    ("tag.delimiter", "ink_2"),
    ("markup.heading", "syn_heading"),
    ("markup.strong", "syn_strong"),
    ("markup.link", "syn_link"),
    ("markup.link.url", "syn_url"),
    ("markup.raw", "syn_code"),
    ("markup.list", "syn_marker"),
    ("markup.quote", "ink_2"),
    ("diff.plus", "insert"),
    ("diff.minus", "danger"),
];

/// A theme's syntax colours.
pub struct SyntaxColors {
    by_name: HashMap<String, u32>,
    /// Resolved colours by scope id (`None` inside: draws plain).
    cache: RwLock<Vec<Option<Option<u32>>>>,
}

impl std::fmt::Debug for SyntaxColors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyntaxColors")
            .field("entries", &self.by_name.len())
            .finish()
    }
}

impl PartialEq for SyntaxColors {
    fn eq(&self, other: &Self) -> bool {
        self.by_name == other.by_name
    }
}

impl SyntaxColors {
    /// The built-in mapping for `palette`, with `overrides` (a theme file's
    /// `[syntax]`) over it. A token that doesn't exist is an error.
    pub fn new(palette: &Palette, overrides: &[(String, SyntaxColor)]) -> Result<Self, String> {
        let mut by_name = HashMap::new();
        for (capture, token) in DEFAULTS {
            if let Some(value) = palette.token(token) {
                by_name.insert((*capture).to_owned(), value);
            }
        }
        for (capture, color) in overrides {
            let value = match color {
                SyntaxColor::Color(value) => *value,
                SyntaxColor::Token(token) => palette.token(token).ok_or_else(|| {
                    format!("[syntax] {capture}: there's no colour called {token}")
                })?,
            };
            by_name.insert(capture.trim_start_matches('@').to_owned(), value);
        }
        Ok(SyntaxColors {
            by_name,
            cache: RwLock::new(Vec::new()),
        })
    }

    /// No colours at all (everything plain).
    pub fn empty() -> Self {
        SyntaxColors {
            by_name: HashMap::new(),
            cache: RwLock::new(Vec::new()),
        }
    }

    /// The colour `scope` is drawn in, falling back by prefix; `None` draws
    /// plain.
    pub fn color(&self, scope: Scope) -> Option<u32> {
        let id = scope.0 as usize;
        if let Ok(cache) = self.cache.read()
            && let Some(Some(hit)) = cache.get(id)
        {
            return *hit;
        }
        let name = scopes::name(scope);
        let found = scopes::fallbacks(&name).find_map(|n| self.by_name.get(n).copied());
        if let Ok(mut cache) = self.cache.write() {
            if cache.len() <= id {
                cache.resize(id + 1, None);
            }
            cache[id] = Some(found);
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::tokens;

    #[test]
    fn captures_fall_back_by_prefix_and_files_override() {
        let p = &tokens::DARK;
        let colors = SyntaxColors::new(p, &[]).expect("defaults");
        let keyword = colors.color(scopes::intern("@keyword.function"));
        assert_eq!(keyword, Some(p.syn_keyword), "falls back to @keyword");
        assert_eq!(
            colors.color(scopes::intern("variable.builtin")),
            Some(p.syn_url)
        );
        assert_eq!(colors.color(scopes::intern("variable")), None, "plain");
        assert_eq!(
            colors.color(scopes::intern("comment.error")),
            Some(p.danger)
        );
        let file = [
            (
                "keyword.return".to_owned(),
                SyntaxColor::Token("danger".into()),
            ),
            ("@function".to_owned(), SyntaxColor::Color(0x61afefff)),
        ];
        let colors = SyntaxColors::new(p, &file).expect("valid");
        assert_eq!(
            colors.color(scopes::intern("keyword.return")),
            Some(p.danger)
        );
        assert_eq!(colors.color(scopes::intern("keyword")), Some(p.syn_keyword));
        assert_eq!(
            colors.color(scopes::intern("function.method.call")),
            Some(0x61afefff)
        );
        let bad = [("x".to_owned(), SyntaxColor::Token("nope".into()))];
        assert!(SyntaxColors::new(p, &bad).is_err());
    }
}
