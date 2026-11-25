//! Live reload: watching the vault for notes changed by other apps (a sync
//! client, git, another editor).

use std::path::Path;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::error::{Result, VaultError};
use crate::note_path::NotePath;
use crate::vault::{Stamp, Vault};

/// Something changed on disk outside mNote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultEvent {
    /// A note was created or its contents changed. Re-read it.
    Changed(NotePath),
    /// A note was deleted or moved away.
    Removed(NotePath),
}

/// Keeps the watch running. Dropping it stops watching.
pub struct VaultWatcher {
    _watcher: RecommendedWatcher,
}

impl std::fmt::Debug for VaultWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaultWatcher")
    }
}

impl Vault {
    /// Watches the whole vault and calls `on_event` for each note changed by
    /// another app. mNote's own saves through [`Vault::write`] are not
    /// reported. `on_event` runs on the watcher's thread, so it should only
    /// hand the event over (for example on a channel) and return.
    pub fn watch(&self, on_event: impl Fn(VaultEvent) + Send + 'static) -> Result<VaultWatcher> {
        let vault = self.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            let mut seen: Vec<&Path> = Vec::with_capacity(event.paths.len());
            for path in &event.paths {
                if seen.contains(&path.as_path()) {
                    continue;
                }
                seen.push(path);
                if let Some(change) = vault.classify(path) {
                    on_event(change);
                }
            }
        })
        .map_err(VaultError::Watch)?;
        watcher
            .watch(self.root(), RecursiveMode::Recursive)
            .map_err(VaultError::Watch)?;
        Ok(VaultWatcher { _watcher: watcher })
    }

    /// Turns a changed path into an event, or `None` for files that aren't
    /// notes and for mNote's own writes.
    fn classify(&self, path: &Path) -> Option<VaultEvent> {
        let note = self.note_at(path).or_else(|| self.file_at(path))?;
        match Stamp::of(path) {
            Some(stamp) => {
                if self.own_writes().get(&note) == Some(&stamp) {
                    None
                } else {
                    Some(VaultEvent::Changed(note))
                }
            }
            None => {
                self.own_writes().remove(&note);
                Some(VaultEvent::Removed(note))
            }
        }
    }
}

/// Keeps a folder watch running ([`watch_dir`]). Dropping it stops watching.
pub struct DirWatcher {
    _watcher: RecommendedWatcher,
}

impl std::fmt::Debug for DirWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DirWatcher")
    }
}

/// Watches `dir` and everything under it, and calls `on_change` with each
/// file created, changed or removed (for theme files saved in another
/// editor). It runs on the watcher's thread: hand the path over and return.
pub fn watch_dir(
    dir: &Path,
    on_change: impl Fn(std::path::PathBuf) + Send + 'static,
) -> Result<DirWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        for path in event.paths {
            on_change(path);
        }
    })
    .map_err(|e| VaultError::io(dir, std::io::Error::other(e.to_string())))?;
    watcher
        .watch(dir, RecursiveMode::Recursive)
        .map_err(|e| VaultError::io(dir, std::io::Error::other(e.to_string())))?;
    Ok(DirWatcher { _watcher: watcher })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::tests::TempDir;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn note(p: &str) -> NotePath {
        NotePath::new(p).expect("valid note path")
    }

    /// Collects events until `want` arrives or a generous timeout passes
    /// (FSEvents batches changes, so delivery can take a moment).
    fn wait_for(rx: &mpsc::Receiver<VaultEvent>, want: &VaultEvent) -> Vec<VaultEvent> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut got = Vec::new();
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(event) => {
                    let done = &event == want;
                    got.push(event);
                    if done {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        got
    }

    #[test]
    fn classifies_external_changes_own_writes_and_removals() {
        let dir = TempDir::new();
        let vault = Vault::open(&dir.0).expect("open");
        let n = note("inbox.md");

        vault.write(&n, "mine").expect("write");
        assert_eq!(
            vault.classify(&vault.path_of(&n)),
            None,
            "own write is ignored"
        );

        std::fs::write(vault.path_of(&n), "edited elsewhere, longer").expect("external write");
        assert_eq!(
            vault.classify(&vault.path_of(&n)),
            Some(VaultEvent::Changed(n.clone()))
        );

        std::fs::remove_file(vault.path_of(&n)).expect("remove");
        assert_eq!(
            vault.classify(&vault.path_of(&n)),
            Some(VaultEvent::Removed(n.clone()))
        );

        assert_eq!(vault.classify(&vault.root().join("image.png")), None);
        assert_eq!(
            vault.classify(&vault.root().join(".inbox.md.mnote-tmp-1")),
            None
        );
    }

    #[test]
    fn reports_a_note_written_by_another_app() {
        let dir = TempDir::new();
        let vault = Vault::open(&dir.0).expect("open");
        let (tx, rx) = mpsc::channel();
        let _watch = vault
            .watch(move |e| {
                let _ = tx.send(e);
            })
            .expect("watch");
        // Give FSEvents a moment to start before the first change.
        std::thread::sleep(Duration::from_millis(300));

        std::fs::create_dir_all(vault.root().join("projects")).expect("mkdir");
        std::fs::write(vault.root().join("projects/roadmap.md"), "# Roadmap").expect("write");

        let want = VaultEvent::Changed(note("projects/roadmap.md"));
        let got = wait_for(&rx, &want);
        assert!(got.contains(&want), "expected {want:?}, got {got:?}");
    }
}
