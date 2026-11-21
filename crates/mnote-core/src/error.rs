//! The one error type for everything the vault does.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// What went wrong in the vault. Messages are written for the person using
/// mNote, so they can be shown as they are.
#[derive(Debug)]
pub enum VaultError {
    /// The folder chosen as the vault doesn't exist or isn't a folder.
    NotADirectory(PathBuf),
    /// A note path that is absolute, leaves the vault (`..`), is hidden, or
    /// isn't a Markdown file.
    InvalidNotePath(PathBuf),
    /// Reading or writing a file failed.
    Io { path: PathBuf, source: io::Error },
    /// The file watcher couldn't start.
    Watch(notify::Error),
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VaultError::NotADirectory(path) => {
                write!(
                    f,
                    "{} isn't a folder. Choose a folder of notes.",
                    path.display()
                )
            }
            VaultError::InvalidNotePath(path) => write!(
                f,
                "{} isn't a note in this vault. Notes are .md files inside the vault folder.",
                path.display()
            ),
            VaultError::Io { path, source } => write!(f, "{}: {source}", path.display()),
            VaultError::Watch(source) => {
                write!(f, "Couldn't watch the vault for changes: {source}")
            }
        }
    }
}

impl std::error::Error for VaultError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            VaultError::Io { source, .. } => Some(source),
            VaultError::Watch(source) => Some(source),
            VaultError::NotADirectory(_) | VaultError::InvalidNotePath(_) => None,
        }
    }
}

impl VaultError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        VaultError::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T, E = VaultError> = std::result::Result<T, E>;
