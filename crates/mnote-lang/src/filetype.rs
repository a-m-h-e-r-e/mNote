//! Filetypes: what mNote knows about a file from its extension. Its
//! language (for parsing and colours), and whether it's a note (indexed,
//! linked, styled as Markdown). Markdown is the only note filetype built in;
//! adding a file type is one [`register`] call.
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

fn table() -> &'static RwLock<Vec<Filetype>> {
    static TABLE: OnceLock<RwLock<Vec<Filetype>>> = OnceLock::new();
    TABLE.get_or_init(|| {
        RwLock::new(vec![Filetype {
            name: "markdown".into(),
            extensions: vec!["md".into(), "markdown".into()],
            language: Some("markdown".into()),
            note: true,
        }])
    })
}

/// Adds a file type, or replaces the one with its name.
pub fn register(filetype: Filetype) {
    if let Ok(mut table) = table().write() {
        table.retain(|f| f.name != filetype.name);
        table.push(filetype);
    }
}

/// The file type of a path, by its extension.
pub fn for_path(path: &Path) -> Option<Filetype> {
    let ext = path.extension()?.to_str()?;
    for_extension(ext)
}

/// The file type with this extension (no dot).
pub fn for_extension(ext: &str) -> Option<Filetype> {
    let table = table().read().ok()?;
    table
        .iter()
        .find(|f| f.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)))
        .cloned()
}

/// Extensions of note file types, without dots (`md`, `markdown`).
pub fn note_extensions() -> Vec<String> {
    table()
        .read()
        .map(|t| {
            t.iter()
                .filter(|f| f.note)
                .flat_map(|f| f.extensions.clone())
                .collect()
        })
        .unwrap_or_default()
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
        assert_eq!(for_path(Path::new("a/b.rs")), None);

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
