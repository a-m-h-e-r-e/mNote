//! [`Vault`]: a folder of plain Markdown notes. mNote never keeps notes
//! anywhere else, so any existing folder (an Obsidian vault included) works.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use crate::error::{Result, VaultError};
use crate::note_path::NotePath;

/// Size and modification time of a file mNote just wrote, so the watcher can
/// tell mNote's own saves from edits made by other apps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stamp {
    len: u64,
    modified: SystemTime,
}

impl Stamp {
    pub(crate) fn of(path: &Path) -> Option<Self> {
        let meta = fs::metadata(path).ok()?;
        Some(Stamp {
            len: meta.len(),
            modified: meta.modified().ok()?,
        })
    }
}

/// Paths mNote wrote, with the stamp each write left on disk.
pub(crate) type OwnWrites = Arc<Mutex<HashMap<NotePath, Stamp>>>;

/// An open vault. Cheap to clone; clones share the record of mNote's own
/// writes, so a watcher started from any clone ignores saves made by another.
#[derive(Debug, Clone)]
pub struct Vault {
    root: PathBuf,
    pub(crate) own_writes: OwnWrites,
    /// Where [`Vault::trash`] moves things; `None` is the user's Trash.
    pub(crate) trash: Option<PathBuf>,
}

impl Vault {
    /// Opens a folder as a vault. The folder must already exist.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let root = root
            .canonicalize()
            .map_err(|_| VaultError::NotADirectory(root.to_path_buf()))?;
        if !root.is_dir() {
            return Err(VaultError::NotADirectory(root));
        }
        Ok(Vault {
            root,
            own_writes: OwnWrites::default(),
            trash: None,
        })
    }

    /// Uses `dir` instead of the user's Trash (for tests).
    pub fn with_trash(mut self, dir: impl Into<PathBuf>) -> Self {
        self.trash = Some(dir.into());
        self
    }

    /// The vault folder, as an absolute path with symlinks resolved.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The absolute path of a note on disk.
    pub fn path_of(&self, note: &NotePath) -> PathBuf {
        self.root.join(note)
    }

    /// The vault-relative note path for an absolute path, if it's a note
    /// inside this vault.
    pub fn note_at(&self, absolute: &Path) -> Option<NotePath> {
        let relative = absolute.strip_prefix(&self.root).ok()?;
        NotePath::new(relative).ok()
    }

    /// Every file of a known type in the vault (notes and the others
    /// registered with [`crate::filetype::register`]), sorted by path.
    /// Hidden files and folders are skipped, and symlinked folders aren't
    /// followed, as for [`Vault::notes`].
    pub fn files(&self) -> Result<Vec<crate::files::VaultPath>> {
        let mut files = Vec::new();
        let mut pending = vec![self.root.clone()];
        while let Some(dir) = pending.pop() {
            let entries = fs::read_dir(&dir).map_err(|e| VaultError::io(&dir, e))?;
            for entry in entries {
                let entry = entry.map_err(|e| VaultError::io(&dir, e))?;
                if entry.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                let path = entry.path();
                let kind = entry.file_type().map_err(|e| VaultError::io(&path, e))?;
                if kind.is_dir() {
                    pending.push(path);
                } else if (kind.is_file() || (kind.is_symlink() && path.is_file()))
                    && let Ok(relative) = path.strip_prefix(&self.root)
                    && crate::is_known_file(relative)
                    && let Ok(file) = crate::files::VaultPath::new(relative)
                {
                    files.push(file);
                }
            }
        }
        files.sort_unstable();
        Ok(files)
    }

    /// Every note in the vault, sorted by path. Hidden files and folders (such
    /// as `.obsidian/` or `.git/`) are skipped, and symlinked folders aren't
    /// followed, so a link loop can't hang the scan.
    pub fn notes(&self) -> Result<Vec<NotePath>> {
        let mut notes = Vec::new();
        let mut pending = vec![self.root.clone()];
        while let Some(dir) = pending.pop() {
            let entries = fs::read_dir(&dir).map_err(|e| VaultError::io(&dir, e))?;
            for entry in entries {
                let entry = entry.map_err(|e| VaultError::io(&dir, e))?;
                if entry.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                let path = entry.path();
                let kind = entry.file_type().map_err(|e| VaultError::io(&path, e))?;
                if kind.is_dir() {
                    pending.push(path);
                } else if (kind.is_file() || (kind.is_symlink() && path.is_file()))
                    // Checked on the vault-relative path, so a vault that lives
                    // inside a hidden folder (such as ~/.notes) still works.
                    && let Some(note) = self.note_at(&path)
                {
                    notes.push(note);
                }
            }
        }
        notes.sort_unstable();
        Ok(notes)
    }

    /// Reads a note as UTF-8 text.
    pub fn read(&self, note: &NotePath) -> Result<String> {
        let path = self.path_of(note);
        fs::read_to_string(&path).map_err(|e| VaultError::io(path, e))
    }

    /// Writes a note atomically: the text goes to a hidden temporary file in
    /// the same folder, is flushed to disk, then renamed over the note. A
    /// crash or power cut leaves either the old note or the new one, never a
    /// half-written file. Missing folders are created.
    pub fn write(&self, note: &NotePath, text: &str) -> Result<()> {
        let path = self.path_of(note);
        let dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.clone());
        fs::create_dir_all(&dir).map_err(|e| VaultError::io(&dir, e))?;

        let tmp = dir.join(format!(
            ".{}.mnote-tmp-{}",
            note.file_name(),
            std::process::id()
        ));
        let written = (|| {
            let mut file = File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            fs::rename(&tmp, &path)?;
            // Make the rename itself durable.
            File::open(&dir)?.sync_all()
        })();
        if let Err(err) = written {
            let _ = fs::remove_file(&tmp);
            return Err(VaultError::io(path, err));
        }

        if let Some(stamp) = Stamp::of(&path) {
            self.own_writes().insert(note.clone(), stamp);
        }
        Ok(())
    }

    pub(crate) fn own_writes(&self) -> MutexGuard<'_, HashMap<NotePath, Stamp>> {
        // A panic while holding this lock can't leave the map inconsistent, so
        // recover the guard rather than propagate the poison.
        self.own_writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
pub(crate) mod tests {

    #[test]
    fn files_lists_every_known_type_and_notes_only_notes() {
        crate::filetype::register(crate::filetype::Filetype {
            name: "vault-files-test".into(),
            extensions: vec!["vftest".into()],
            language: None,
            note: false,
        });
        let dir = TempDir::new();
        dir.file("a.md", "# A");
        dir.file("src/b.vftest", "b");
        dir.file("c.unknown", "c");
        dir.file(".hidden/d.md", "d");
        let vault = Vault::open(&dir.0).expect("vault");
        let files: Vec<String> = vault
            .files()
            .expect("files")
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(files, ["a.md", "src/b.vftest"]);
        let notes: Vec<String> = vault
            .notes()
            .expect("notes")
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(notes, ["a.md"]);
    }

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A temporary folder removed when dropped.
    pub(crate) struct TempDir(pub PathBuf);

    impl TempDir {
        pub(crate) fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "mnote-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            TempDir(path)
        }

        pub(crate) fn file(&self, rel: &str, text: &str) {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().expect("has parent")).expect("create dirs");
            fs::write(path, text).expect("write file");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn note(p: &str) -> NotePath {
        NotePath::new(p).expect("valid note path")
    }

    #[test]
    fn opening_requires_an_existing_folder() {
        let dir = TempDir::new();
        dir.file("note.md", "hi");
        assert!(Vault::open(&dir.0).is_ok());
        assert!(matches!(
            Vault::open(dir.0.join("missing")),
            Err(VaultError::NotADirectory(_))
        ));
        assert!(matches!(
            Vault::open(dir.0.join("note.md")),
            Err(VaultError::NotADirectory(_))
        ));
    }

    #[test]
    fn lists_notes_and_skips_hidden_and_other_files() {
        let dir = TempDir::new();
        dir.file("inbox.md", "");
        dir.file("projects/mnote/ui-design.md", "");
        dir.file("projects/roadmap.markdown", "");
        dir.file("assets/explorer.png", "");
        dir.file(".obsidian/workspace.md", "");
        dir.file("projects/.draft.md", "");
        let vault = Vault::open(&dir.0).expect("open");
        assert_eq!(
            vault.notes().expect("list"),
            [
                note("inbox.md"),
                note("projects/mnote/ui-design.md"),
                note("projects/roadmap.markdown"),
            ]
        );
    }

    #[test]
    fn a_vault_inside_a_hidden_folder_still_lists_its_notes() {
        let dir = TempDir::new();
        dir.file(".notes/inbox.md", "");
        let vault = Vault::open(dir.0.join(".notes")).expect("open");
        assert_eq!(vault.notes().expect("list"), [note("inbox.md")]);
    }

    #[test]
    fn writes_atomically_and_reads_back() {
        let dir = TempDir::new();
        let vault = Vault::open(&dir.0).expect("open");
        let n = note("journal/2026-11-21.md");
        vault.write(&n, "# Today\n").expect("first write");
        vault
            .write(&n, "# Today\n\nSecond draft.\n")
            .expect("overwrite");
        assert_eq!(vault.read(&n).expect("read"), "# Today\n\nSecond draft.\n");
        // No temporary files are left behind.
        let leftovers: Vec<_> = fs::read_dir(dir.0.join("journal"))
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("mnote-tmp"))
            .collect();
        assert!(leftovers.is_empty());
        assert!(vault.own_writes().contains_key(&n));
    }

    #[test]
    fn reading_a_missing_note_is_an_error_not_a_panic() {
        let dir = TempDir::new();
        let vault = Vault::open(&dir.0).expect("open");
        assert!(matches!(
            vault.read(&note("missing.md")),
            Err(VaultError::Io { .. })
        ));
    }

    #[test]
    fn maps_absolute_paths_back_to_notes() {
        let dir = TempDir::new();
        let vault = Vault::open(&dir.0).expect("open");
        let abs = vault.root().join("projects/ui-design.md");
        assert_eq!(vault.note_at(&abs), Some(note("projects/ui-design.md")));
        assert_eq!(vault.note_at(Path::new("/elsewhere/note.md")), None);
        assert_eq!(vault.note_at(&vault.root().join(".hidden.md")), None);
    }
}

#[cfg(test)]
mod budget {
    use super::tests::TempDir;
    use super::*;
    use std::time::{Duration, Instant};

    /// Performance budget from CLAUDE.md: a 10 000-note vault is listed well
    /// inside the 1 s indexing budget. Run with
    /// `cargo test -p mnote-core --release -- --ignored`.
    #[test]
    #[ignore = "timing test; run with --release -- --ignored"]
    fn lists_ten_thousand_notes_within_budget() {
        let dir = TempDir::new();
        for folder in 0..100 {
            for n in 0..100 {
                dir.file(&format!("f{folder}/note-{n}.md"), "# Note\n");
            }
        }
        let vault = Vault::open(&dir.0).expect("open");
        let start = Instant::now();
        let notes = vault.notes().expect("list");
        let took = start.elapsed();
        assert_eq!(notes.len(), 10_000);
        eprintln!("listed 10 000 notes in {took:?}");
        assert!(took < Duration::from_millis(250), "listing took {took:?}");
    }
}
