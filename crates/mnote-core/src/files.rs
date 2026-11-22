//! File operations on the vault, each with its undo: new notes, renames and
//! moves (notes or folders), and moving to the Trash.
//!
//! Deleting never deletes: [`Vault::trash`] moves the file or folder into the
//! user's Trash (`~/.Trash`), where Finder shows it, and its [`FileOp`] can
//! put it back. That's a plain rename, so it needs no Objective-C (which would
//! mean `unsafe` or another crate); the trade-off is that Finder's "Put Back"
//! doesn't know where an item came from. mNote's own undo does.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Result, VaultError};
use crate::is_note;
use crate::note_path::NotePath;
use crate::vault::Vault;

/// A vault-relative path to a note or folder that is safe to touch: not
/// empty, not absolute, never leaving the vault, not hidden.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VaultPath(PathBuf);

impl VaultPath {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let safe = !path.as_os_str().is_empty()
            && path.components().all(|c| match c {
                Component::Normal(part) => !part.to_string_lossy().starts_with('.'),
                _ => false,
            });
        if safe {
            Ok(VaultPath(path.to_path_buf()))
        } else {
            Err(VaultError::InvalidNotePath(path.to_path_buf()))
        }
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// The note this path names, if it is one.
    pub fn note(&self) -> Option<NotePath> {
        is_note(&self.0)
            .then(|| NotePath::new(&self.0).ok())
            .flatten()
    }
}

impl From<&NotePath> for VaultPath {
    fn from(note: &NotePath) -> Self {
        VaultPath(note.as_path().to_path_buf())
    }
}

impl std::fmt::Display for VaultPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

/// A file operation that happened, with what undoing it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    /// A new, empty note.
    Created(NotePath),
    /// A note or folder renamed or moved.
    Moved { from: VaultPath, to: VaultPath },
    /// A note or folder moved to the Trash, now at `trashed` (absolute).
    Trashed { from: VaultPath, trashed: PathBuf },
}

impl Vault {
    fn absolute(&self, path: &VaultPath) -> PathBuf {
        self.root().join(path.as_path())
    }

    /// Creates an empty note, and any folders it needs. Fails if something is
    /// already there.
    pub fn create_note(&self, note: &NotePath) -> Result<FileOp> {
        let path = self.path_of(note);
        if path.exists() {
            return Err(VaultError::AlreadyExists(note.as_path().to_path_buf()));
        }
        self.write(note, "")?;
        Ok(FileOp::Created(note.clone()))
    }

    /// Renames or moves a note or folder. Folders the target needs are
    /// created; an existing target is never overwritten.
    pub fn rename(&self, from: &VaultPath, to: &VaultPath) -> Result<FileOp> {
        let (src, dst) = (self.absolute(from), self.absolute(to));
        if !src.exists() {
            return Err(VaultError::io(
                src,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        // A different case of the same name is a rename on a case-insensitive
        // disk, not a clash.
        let same_file =
            src.to_string_lossy().to_lowercase() == dst.to_string_lossy().to_lowercase();
        if dst.exists() && !same_file {
            return Err(VaultError::AlreadyExists(to.as_path().to_path_buf()));
        }
        if dst.starts_with(&src) && dst != src {
            // A folder can't move into itself.
            return Err(VaultError::InvalidNotePath(to.as_path().to_path_buf()));
        }
        if let Some(dir) = dst.parent() {
            fs::create_dir_all(dir).map_err(|e| VaultError::io(dir, e))?;
        }
        fs::rename(&src, &dst).map_err(|e| VaultError::io(&src, e))?;
        Ok(FileOp::Moved {
            from: from.clone(),
            to: to.clone(),
        })
    }

    /// Moves a note or folder to the user's Trash.
    pub fn trash(&self, path: &VaultPath) -> Result<FileOp> {
        let src = self.absolute(path);
        let trash = match &self.trash {
            Some(dir) => dir.clone(),
            None => trash_dir()?,
        };
        fs::create_dir_all(&trash).map_err(|e| VaultError::io(&trash, e))?;
        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut target = trash.join(&name);
        if target.exists() {
            // Finder's way: the time is added before the extension.
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let (stem, ext) = match name.rsplit_once('.') {
                Some((stem, ext)) if !stem.is_empty() && src.is_file() => (stem, format!(".{ext}")),
                _ => (name.as_str(), String::new()),
            };
            target = trash.join(format!("{stem} {stamp}{ext}"));
        }
        move_across(&src, &target)?;
        Ok(FileOp::Trashed {
            from: path.clone(),
            trashed: target,
        })
    }

    /// Undoes `op`, returning the operation that undid it.
    pub fn undo(&self, op: &FileOp) -> Result<FileOp> {
        match op {
            FileOp::Created(note) => {
                let text = self.read(note)?;
                if !text.trim().is_empty() {
                    return Err(VaultError::NotEmpty(note.as_path().to_path_buf()));
                }
                self.trash(&VaultPath::from(note))
            }
            FileOp::Moved { from, to } => self.rename(to, from),
            FileOp::Trashed { from, trashed } => {
                let back = self.absolute(from);
                if back.exists() {
                    return Err(VaultError::AlreadyExists(from.as_path().to_path_buf()));
                }
                if let Some(dir) = back.parent() {
                    fs::create_dir_all(dir).map_err(|e| VaultError::io(dir, e))?;
                }
                move_across(trashed, &back)?;
                Ok(FileOp::Moved {
                    from: from.clone(),
                    to: from.clone(),
                })
            }
        }
    }
}

/// The user's Trash folder.
fn trash_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".Trash"))
        .ok_or_else(|| {
            VaultError::io(
                ".Trash",
                std::io::Error::new(std::io::ErrorKind::NotFound, "no home folder"),
            )
        })
}

/// Renames, or copies then removes when the two are on different disks.
fn move_across(src: &Path, dst: &Path) -> Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(18) => {
            // EXDEV: another volume.
            copy_all(src, dst)?;
            let removed = if src.is_dir() {
                fs::remove_dir_all(src)
            } else {
                fs::remove_file(src)
            };
            removed.map_err(|e| VaultError::io(src, e))
        }
        Err(e) => Err(VaultError::io(src, e)),
    }
}

fn copy_all(src: &Path, dst: &Path) -> Result<()> {
    if src.is_dir() {
        fs::create_dir_all(dst).map_err(|e| VaultError::io(dst, e))?;
        for entry in fs::read_dir(src).map_err(|e| VaultError::io(src, e))? {
            let entry = entry.map_err(|e| VaultError::io(src, e))?;
            copy_all(&entry.path(), &dst.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(src, dst)
            .map(|_| ())
            .map_err(|e| VaultError::io(src, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::tests::TempDir;

    fn vp(p: &str) -> VaultPath {
        VaultPath::new(p).expect("valid")
    }

    #[test]
    fn paths_must_stay_inside_the_vault() {
        assert!(VaultPath::new("projects/mnote").is_ok());
        assert!(VaultPath::new("../x").is_err());
        assert!(VaultPath::new("/tmp/x").is_err());
        assert!(VaultPath::new(".obsidian").is_err());
        assert!(VaultPath::new("").is_err());
        assert_eq!(vp("a/b.md").note(), NotePath::new("a/b.md").ok());
        assert_eq!(vp("a/b").note(), None);
    }

    #[test]
    fn create_rename_move_and_undo() {
        let dir = TempDir::new();
        let trash = TempDir::new();
        dir.file("a.md", "text");
        let vault = Vault::open(&dir.0).expect("vault").with_trash(&trash.0);

        let new = NotePath::new("ideas/new.md").expect("path");
        let op = vault.create_note(&new).expect("created");
        assert!(dir.0.join("ideas/new.md").is_file());
        assert!(matches!(
            vault.create_note(&new),
            Err(VaultError::AlreadyExists(_))
        ));
        vault.undo(&op).expect("trash the empty note");
        assert!(!dir.0.join("ideas/new.md").exists());

        let op = vault.rename(&vp("a.md"), &vp("b.md")).expect("renamed");
        assert!(dir.0.join("b.md").is_file() && !dir.0.join("a.md").exists());
        vault.undo(&op).expect("undone");
        assert!(dir.0.join("a.md").is_file());

        dir.file("folder/inner.md", "x");
        let op = vault
            .rename(&vp("folder"), &vp("moved/folder"))
            .expect("moved");
        assert!(dir.0.join("moved/folder/inner.md").is_file());
        assert!(
            vault
                .rename(&vp("moved"), &vp("moved/folder/inside"))
                .is_err(),
            "not into itself"
        );
        vault.undo(&op).expect("undone");
        assert!(dir.0.join("folder/inner.md").is_file());

        dir.file("taken.md", "");
        assert!(matches!(
            vault.rename(&vp("a.md"), &vp("taken.md")),
            Err(VaultError::AlreadyExists(_))
        ));
    }

    #[test]
    fn trash_and_put_back() {
        let dir = TempDir::new();
        let trash = TempDir::new();
        dir.file("old.md", "one");
        dir.file("folder/x.md", "x");
        let vault = Vault::open(&dir.0).expect("vault").with_trash(&trash.0);
        let op = vault.trash(&vp("old.md")).expect("trashed");
        assert!(!dir.0.join("old.md").exists());
        assert!(trash.0.join("old.md").is_file());
        // A second note of the same name gets a new name in the Trash.
        dir.file("old.md", "two");
        let FileOp::Trashed { trashed, .. } = vault.trash(&vp("old.md")).expect("trashed") else {
            panic!("trashed");
        };
        assert_ne!(trashed, trash.0.join("old.md"));
        vault.undo(&op).expect("put back");
        assert_eq!(
            std::fs::read_to_string(dir.0.join("old.md")).expect("back"),
            "one"
        );
        let op = vault.trash(&vp("folder")).expect("folders too");
        vault.undo(&op).expect("put back");
        assert!(dir.0.join("folder/x.md").is_file());
    }

    #[test]
    fn a_created_note_with_text_isnt_undone() {
        let dir = TempDir::new();
        let trash = TempDir::new();
        let vault = Vault::open(&dir.0).expect("vault").with_trash(&trash.0);
        let note = NotePath::new("n.md").expect("path");
        let op = vault.create_note(&note).expect("created");
        vault.write(&note, "kept").expect("write");
        assert!(matches!(vault.undo(&op), Err(VaultError::NotEmpty(_))));
        assert!(dir.0.join("n.md").is_file());
    }
}
