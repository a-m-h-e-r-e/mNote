//! Filetypes: what mNote knows about a file from its name, extension or
//! first line. Its language (for parsing and colours), and whether it's a
//! note (indexed, linked, styled as Markdown). They're a view of the
//! language packs ([`crate::languages`]), Markdown the one note language;
//! [`register`] adds one directly.
//!
//! The table is shared by every thread: indexing and the file watcher run in
//! the background.

use std::path::Path;
use std::sync::{OnceLock, RwLock};

/// A kind of file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filetype {
    /// Its name (`markdown`, `rust`).
    pub name: String,
    /// Extensions without the dot, matched case-insensitively. The first is
    /// the one new files get.
    pub extensions: Vec<String>,
    /// Its language in the language registry ([`crate::code`]), if any.
    pub language: Option<String>,
    /// It's a note: listed and indexed with its titles, tags and links, and
    /// shown in the styled view.
    pub note: bool,
}

/// File types registered directly (by a plugin or a test); they come
/// before the language packs'.
fn table() -> &'static RwLock<Vec<Filetype>> {
    static TABLE: OnceLock<RwLock<Vec<Filetype>>> = OnceLock::new();
    TABLE.get_or_init(RwLock::default)
}

/// A language pack as a file type: Markdown is the one note language.
fn from_pack(pack: crate::pack::Pack) -> Filetype {
    Filetype {
        note: pack.name == "markdown",
        language: Some(pack.name.clone()),
        extensions: pack.extensions,
        name: pack.name,
    }
}

/// Adds a file type, or replaces the one with its name.
pub fn register(filetype: Filetype) {
    if let Ok(mut table) = table().write() {
        table.retain(|f| f.name != filetype.name);
        table.push(filetype);
    }
}

/// The file type of a path: by its exact name, then its extension.
pub fn for_path(path: &Path) -> Option<Filetype> {
    detect(path, None)
}

/// The file type of a file: by its exact name, then its extension, then
/// its first line (a `#!` line, or a pack's `first_line` pattern).
pub fn detect(path: &Path, first_line: Option<&str>) -> Option<Filetype> {
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && let Some(registered) = registered_for(ext)
    {
        return Some(registered);
    }
    crate::languages::detect(path, first_line).map(from_pack)
}

fn registered_for(ext: &str) -> Option<Filetype> {
    let table = table().read().ok()?;
    table
        .iter()
        .find(|f| f.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)))
        .cloned()
}

/// The file type with this extension (no dot).
pub fn for_extension(ext: &str) -> Option<Filetype> {
    registered_for(ext).or_else(|| {
        crate::languages::packs()
            .into_iter()
            .find(|p| p.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)))
            .map(from_pack)
    })
}

/// Extensions of note file types, without dots (`md`, `markdown`).
pub fn note_extensions() -> Vec<String> {
    let mut exts: Vec<String> = crate::languages::pack("markdown")
        .map(|p| p.extensions)
        .unwrap_or_else(|| vec!["md".into()]);
    if let Ok(table) = table().read() {
        for f in table.iter().filter(|f| f.note) {
            exts.extend(f.extensions.iter().cloned());
        }
    }
    exts
}

/// The extension a new note gets (`md`).
pub fn new_note_extension() -> String {
    note_extensions()
        .into_iter()
        .next()
        .unwrap_or_else(|| "md".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_is_a_note_and_others_can_be_added() {
        let md = for_path(Path::new("a/B.MD")).expect("markdown");
        assert!(md.note);
        assert_eq!(md.language.as_deref(), Some("markdown"));
        assert_eq!(new_note_extension(), "md");
        assert!(note_extensions().contains(&"markdown".to_owned()));
        // Code languages are file types too, but not notes.
        let rust = for_path(Path::new("a/b.rs")).expect("rust");
        assert!(!rust.note);
        assert_eq!(rust.language.as_deref(), Some("rust"));
        assert_eq!(for_path(Path::new("a/b.unknownext")), None);
        assert_eq!(
            detect(Path::new("bin/tool"), Some("#!/usr/bin/env python3")).map(|f| f.name),
            Some("python".into())
        );

        register(Filetype {
            name: "rust-test".into(),
            extensions: vec!["rstest".into()],
            language: Some("rust".into()),
            note: false,
        });
        let rs = for_path(Path::new("src/main.rstest")).expect("registered");
        assert!(!rs.note);
        assert!(!note_extensions().contains(&"rstest".to_owned()));
    }
}
