//! The native grammar engine: opens a tree-sitter parser library (`.so` /
//! `.dylib`) and gets its language (ADR-0002).
//!
//! This is the one module of `mnote-lang` allowed `unsafe` (CLAUDE.md):
//! loading a library runs its code, and its language function is a C
//! function. [`super::load`] has already checked the library is in an
//! allowed folder and that the user allowed it, and it checks the ABI
//! version before the language is used for anything else. A loaded library
//! is never unloaded, since a `Language` points into it.

#![allow(unsafe_code)]

use std::path::Path;

use tree_sitter::Language;

use super::GrammarEngine;
use crate::error::{LangError, Origin};

/// Loads grammars from native libraries.
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeEngine;

impl GrammarEngine for NativeEngine {
    fn load(&self, grammar: &str, path: &Path) -> Result<Language, LangError> {
        let fail = |message: String| {
            LangError::new(Origin::File(path.to_path_buf()), Some(grammar), message)
        };
        // SAFETY: opening the library runs its initialisers. It's a
        // tree-sitter parser from a folder the user's grammars live in, and
        // the user allowed this exact file (by fingerprint) before this call.
        let library = unsafe { libloading::Library::new(path) }
            .map_err(|e| fail(format!("couldn't open the library: {e}")))?;
        let symbol = format!("tree_sitter_{}", grammar.replace('-', "_"));
        // SAFETY: tree-sitter parsers export `tree_sitter_<name>` as
        // `const TSLanguage *tree_sitter_<name>(void)`, which this type
        // matches. The symbol is only used while `library` is alive, and the
        // library is never unloaded (below).
        let function: unsafe extern "C" fn() -> *const () = unsafe {
            *library
                .get::<unsafe extern "C" fn() -> *const ()>(symbol.as_bytes())
                .map_err(|_| fail(format!("it has no {symbol} function")))?
        };
        // SAFETY: `function` is the parser's language function (above); it
        // takes no arguments and returns a pointer to static data.
        let language =
            Language::new(unsafe { tree_sitter_language::LanguageFn::from_raw(function) });
        // Never unloaded: the language (and every tree made with it) points
        // into the library.
        std::mem::forget(library);
        Ok(language)
    }
}
