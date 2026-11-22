//! mNote's core: the vault of plain Markdown files, the text buffer of an open
//! note, the note index and links.
//!
//! This crate has no UI dependencies, so it can be tested on its own and reused
//! by a future CLI.

pub mod blame;
pub mod capture;
pub mod code;
pub mod config;
pub mod content;
mod error;
pub mod files;
pub mod folder_edit;
pub mod frontmatter;
pub mod index;
pub mod links;
pub mod mentions;
mod note_path;
pub mod styled;
pub mod text;
mod vault;
mod watch;

use std::path::Path;

pub use error::{Result, VaultError};
pub use frontmatter::Frontmatter;
pub use index::{FolderItem, NoteEntry, NoteIndex, NoteSearch};
pub use note_path::NotePath;
pub use text::{Autosave, Buffer, ChangedLines, Reload, drop_tree_later};
/// The Markdown syntax tree a [`Buffer`] keeps, for the styled view.
pub use tree_sitter::Tree as SyntaxTree;
pub use vault::Vault;
pub use watch::{VaultEvent, VaultWatcher};

/// File extensions mNote treats as notes.
pub const NOTE_EXTENSIONS: &[&str] = &["md", "markdown"];

/// Returns true when `path` looks like a note mNote should index.
///
/// Hidden files and folders (such as `.obsidian/` or `.git/`) are skipped.
pub fn is_note(path: &Path) -> bool {
    let hidden = path
        .components()
        .any(|c| c.as_os_str().to_string_lossy().starts_with('.'));
    let has_note_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| NOTE_EXTENSIONS.iter().any(|n| n.eq_ignore_ascii_case(e)));
    has_note_ext && !hidden
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_files_are_notes() {
        assert!(is_note(Path::new("projects/ui-design.md")));
        assert!(is_note(Path::new("README.MD")));
        assert!(is_note(Path::new("journal/2026-11-21.markdown")));
    }

    #[test]
    fn other_and_hidden_files_are_not_notes() {
        assert!(!is_note(Path::new("assets/explorer.png")));
        assert!(!is_note(Path::new("notes")));
        assert!(!is_note(Path::new(".obsidian/workspace.md")));
        assert!(!is_note(Path::new("projects/.draft.md")));
    }
}
