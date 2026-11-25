//! mNote's core: the vault of plain Markdown files, the text buffer of an open
//! note, the note index and links.
//!
//! This crate has no UI dependencies, so it can be tested on its own and reused
//! by a future CLI.

pub mod blame;
pub mod capture;
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
pub mod permissions;
pub mod styled;
pub mod text;
mod vault;
mod watch;

use std::path::Path;

// The language layer moved to `mnote-lang`; these keep the old paths
// (`mnote_core::code`, `mnote_core::filetype`) working.
pub use mnote_lang::{code, filetype};

pub use error::{Result, VaultError};
pub use frontmatter::Frontmatter;
pub use index::{FolderItem, NoteEntry, NoteIndex, NoteSearch};
pub use note_path::NotePath;
/// The Markdown syntax tree a [`Buffer`] keeps, for the styled view.
pub use ropey::Rope;
pub use text::{Autosave, Buffer, ChangedLines, Mark, MarkKind, Reload, drop_tree_later};
pub use tree_sitter::Tree as SyntaxTree;
pub use vault::Vault;
pub use watch::{DirWatcher, VaultEvent, VaultWatcher, watch_dir};

/// Returns true when `path` looks like a note mNote should index: its
/// file type ([`filetype`]) is a note one (Markdown).
///
/// Hidden files and folders (such as `.obsidian/` or `.git/`) are skipped.
pub fn is_note(path: &Path) -> bool {
    !is_hidden(path) && filetype::for_path(path).is_some_and(|f| f.note)
}

/// Returns true when `path` is a file of a known type (a note, or another
/// registered with [`filetype::register`]), not hidden.
pub fn is_known_file(path: &Path) -> bool {
    !is_hidden(path) && filetype::for_path(path).is_some()
}

fn is_hidden(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
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
