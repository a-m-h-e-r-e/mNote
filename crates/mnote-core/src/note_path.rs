//! [`NotePath`]: a checked, vault-relative path to a note.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::{Result, VaultError};
use crate::is_note;

/// A path to a note, relative to the vault root: a safe
/// [`VaultPath`](crate::files::VaultPath) (not absolute, never leaving the
/// vault, not hidden) whose file type is a note one (`.md`).
///
/// Every vault operation takes a `NotePath`, so a bad path can't reach the
/// file system.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NotePath(PathBuf);

impl NotePath {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let safe = crate::files::VaultPath::new(path).is_ok() && is_note(path);
        if safe {
            Ok(NotePath(path.to_path_buf()))
        } else {
            Err(VaultError::InvalidNotePath(path.to_path_buf()))
        }
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// The file name with its extension, as shown in the status line:
    /// `ui-design.md`.
    pub fn file_name(&self) -> &str {
        self.0
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
    }

    /// The note's name without the extension: `ui-design`.
    pub fn name(&self) -> &str {
        self.0
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
    }

    /// The folders from the vault root down to the note, then the note's name,
    /// as shown in the top bar: `["projects", "mnote", "ui-design"]`.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        let folders = self
            .0
            .parent()
            .into_iter()
            .flat_map(|p| p.components())
            .filter_map(|c| c.as_os_str().to_str());
        folders.chain(std::iter::once(self.name()))
    }
}

impl fmt::Display for NotePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

impl AsRef<Path> for NotePath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_notes_inside_the_vault() {
        let p = NotePath::new("projects/mnote/ui-design.md").expect("valid note path");
        assert_eq!(p.file_name(), "ui-design.md");
        assert_eq!(p.name(), "ui-design");
        assert_eq!(
            p.segments().collect::<Vec<_>>(),
            ["projects", "mnote", "ui-design"]
        );
        assert_eq!(
            NotePath::new("inbox.md")
                .expect("valid")
                .segments()
                .collect::<Vec<_>>(),
            ["inbox"]
        );
    }

    #[test]
    fn rejects_paths_that_escape_or_hide() {
        for bad in [
            "",
            "/etc/passwd.md",
            "../outside.md",
            "projects/../../outside.md",
            "./note.md",
            ".obsidian/workspace.md",
            "projects/.draft.md",
            "image.png",
        ] {
            assert!(NotePath::new(bad).is_err(), "{bad:?} should be rejected");
        }
    }
}
