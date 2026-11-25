//! Grammars: the tree-sitter parsers languages are parsed with (ADR-0002).
//!
//! Eight are built into mNote (see [`crate::code`]). Others are parser
//! libraries loaded at run time, found by a pack's `grammar` name in:
//!
//! - mNote's own folder, `~/.config/mnote/grammars/`;
//! - Neovim's parser folders (`site/parser`, nvim-treesitter's `parser`),
//!   when `reuse_neovim` is on.
//!
//! A library is loaded only from those folders, only once the user allowed
//! it (the caller asks the permission store, by fingerprint), and only if
//! its ABI version is one this tree-sitter reads. Loading happens through a
//! [`GrammarEngine`], so another engine (WASM) could be added without
//! changing anything that uses grammars.

pub mod native;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use tree_sitter::Language;

use crate::error::{LangError, Origin};

/// Something that turns a grammar library into a language.
pub trait GrammarEngine: Send + Sync {
    /// Loads `grammar` from `path`. The caller has checked the folder and
    /// the user's OK; the ABI is checked after.
    fn load(&self, grammar: &str, path: &Path) -> Result<Language, LangError>;
}

/// The folders grammar libraries are loaded from: mNote's own, then
/// Neovim's (`data` is Neovim's data folder, when it's reused).
pub fn folders(config_dir: Option<&Path>, neovim_data: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(config) = config_dir {
        out.push(config.join("grammars"));
    }
    if let Some(data) = neovim_data {
        out.push(data.join("site/parser"));
        out.push(data.join("lazy/nvim-treesitter/parser"));
    }
    out
}

fn folder_list() -> &'static RwLock<Vec<PathBuf>> {
    static FOLDERS: OnceLock<RwLock<Vec<PathBuf>>> = OnceLock::new();
    FOLDERS.get_or_init(RwLock::default)
}

/// Sets the folders grammars are looked for in (when settings are applied).
pub fn set_folders(folders: Vec<PathBuf>) {
    if let Ok(mut list) = folder_list().write() {
        *list = folders;
    }
}

/// The folders grammars are looked for in now.
pub fn current_folders() -> Vec<PathBuf> {
    folder_list().read().map(|f| f.clone()).unwrap_or_default()
}

/// The grammars loaded so far, by name.
fn loaded() -> &'static RwLock<HashMap<String, Language>> {
    static LOADED: OnceLock<RwLock<HashMap<String, Language>>> = OnceLock::new();
    LOADED.get_or_init(RwLock::default)
}

/// The loaded grammar called `grammar`, if it is.
pub fn loaded_language(grammar: &str) -> Option<Language> {
    loaded().read().ok()?.get(grammar).cloned()
}

/// The library for `grammar` in `folders` (first found): `<grammar>.so` or
/// `<grammar>.dylib`. Names with a path in them find nothing.
pub fn find(grammar: &str, folders: &[PathBuf]) -> Option<PathBuf> {
    if grammar.is_empty() || grammar.contains(['/', '\\', '.']) {
        return None;
    }
    folders.iter().find_map(|folder| {
        ["so", "dylib"]
            .iter()
            .map(|ext| folder.join(format!("{grammar}.{ext}")))
            .find(|p| p.is_file())
    })
}

/// A grammar that could be loaded for `language` (a name or alias): its
/// name and library. `None` when the language has a grammar already, has
/// no pack, or no library was found.
pub fn available(language: &str) -> Option<(String, PathBuf)> {
    let pack = crate::languages::pack(language)?;
    if crate::code::language(&pack.name).is_some() {
        return None;
    }
    let folders = folder_list().read().ok()?.clone();
    let path = find(&pack.grammar, &folders)?;
    Some((pack.grammar, path))
}

/// Loads `grammar` from `path` with `engine`, if `path` is inside one of
/// `folders` and the language's ABI is one this tree-sitter reads; then
/// languages using it can be parsed and coloured. Slow (it opens a
/// library): call it off the UI thread.
pub fn load(
    engine: &dyn GrammarEngine,
    grammar: &str,
    path: &Path,
    folders: &[PathBuf],
) -> Result<Language, LangError> {
    let fail =
        |message: String| LangError::new(Origin::File(path.to_path_buf()), Some(grammar), message);
    let real = path
        .canonicalize()
        .map_err(|e| fail(format!("can't read it: {e}")))?;
    let allowed = folders
        .iter()
        .filter_map(|f| f.canonicalize().ok())
        .any(|folder| real.starts_with(&folder) && real.parent() == Some(folder.as_path()));
    if !allowed {
        return Err(fail(
            "grammars load only from mNote's grammars folder or Neovim's parser folders".into(),
        ));
    }
    let language = engine.load(grammar, &real)?;
    let abi = language.abi_version();
    let supported = tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION..=tree_sitter::LANGUAGE_VERSION;
    if !supported.contains(&abi) {
        return Err(fail(format!(
            "it was made for tree-sitter ABI {abi}; mNote reads {} to {}",
            supported.start(),
            supported.end()
        )));
    }
    if let Ok(mut map) = loaded().write() {
        map.insert(grammar.to_owned(), language.clone());
    }
    crate::code::rebuild();
    Ok(language)
}

#[cfg(test)]
mod tests;
