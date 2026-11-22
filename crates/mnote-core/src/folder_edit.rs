//! Editing a folder as text (oil.nvim style): the folder becomes one line per
//! entry (`design/` for a subfolder, `ui-design.md` for a note), and edits to
//! those lines become file operations, shown for review before anything
//! touches the disk.
//!
//! Reading the edited text back:
//! - a line kept as it was changes nothing (order doesn't matter);
//! - a changed line renames or moves its entry: `a.md` → `b.md`,
//!   `a.md` → `archive/a.md`, `old/` → `new/`;
//! - a deleted line moves its entry to the Trash;
//! - a new line creates a note (`ideas` → `ideas.md`) or a folder (`ideas/`).
//!
//! Lines carry no hidden ids, so a changed line is paired with a removed one:
//! first one with the same file name (a move), then in order (a rename).

use std::collections::HashSet;
use std::fmt;

use crate::error::{Result, VaultError};
use crate::files::{FileOp, VaultPath};
use crate::note_path::NotePath;
use crate::vault::Vault;

/// One entry of the folder being edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderEntry {
    pub name: String,
    pub is_dir: bool,
}

impl FolderEntry {
    /// The entry as a line: folders end with `/`.
    pub fn line(&self) -> String {
        if self.is_dir {
            format!("{}/", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// The folder as text, folders first.
pub fn folder_text(entries: &[FolderEntry]) -> String {
    let mut text: String = entries.iter().map(|e| e.line() + "\n").collect();
    if text.is_empty() {
        text.push('\n');
    }
    text
}

/// One change, with paths relative to the folder being edited. Folders end
/// with `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A rename (same folder) or a move (another folder).
    Move {
        from: String,
        to: String,
    },
    Trash(String),
    New(String),
}

impl Change {
    /// The word the review shows: move, rename, trash or new.
    pub fn verb(&self) -> &'static str {
        match self {
            Change::Move { from, to } if parent(from) == parent(to) => "rename",
            Change::Move { .. } => "move",
            Change::Trash(_) => "trash",
            Change::New(_) => "new",
        }
    }
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Change::Move { from, to } => write!(f, "{from} → {to}"),
            Change::Trash(path) | Change::New(path) => f.write_str(path),
        }
    }
}

fn parent(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map_or("", |(p, _)| p)
}

fn file_name(path: &str) -> &str {
    let path = path.trim_end_matches('/');
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// Why edited text can't become changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// A line that leaves the folder, is hidden or empty after cleaning.
    BadLine(String),
    /// Two lines name the same entry.
    Duplicate(String),
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::BadLine(line) => write!(f, "\"{line}\" isn't a name inside this folder"),
            PlanError::Duplicate(line) => write!(f, "\"{line}\" is on two lines"),
        }
    }
}

/// Cleans one line: trims, drops a leading `./`, gives a bare note name its
/// `.md`. `None` for a blank line.
fn clean(line: &str) -> std::result::Result<Option<String>, PlanError> {
    let line = line.trim();
    let line = line.strip_prefix("./").unwrap_or(line);
    if line.is_empty() {
        return Ok(None);
    }
    let is_dir = line.ends_with('/');
    let bare = line.trim_end_matches('/');
    if bare.is_empty()
        || bare.starts_with('/')
        || bare
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == ".." || part.starts_with('.'))
    {
        return Err(PlanError::BadLine(line.to_owned()));
    }
    Ok(Some(if is_dir {
        format!("{bare}/")
    } else if bare.ends_with(".md") || bare.ends_with(".markdown") || bare.contains('.') {
        bare.to_owned()
    } else {
        format!("{bare}.md")
    }))
}

/// The changes that turn `entries` into the edited `text`, in the order
/// they should run: trash, then moves, then new entries.
///
/// The two texts are aligned line by line (longest common subsequence), so
/// an edited line pairs with the line it replaced. A line removed in one place
/// and added elsewhere with the same text changes nothing; one added with the
/// same file name under another folder is a move.
pub fn plan(entries: &[FolderEntry], text: &str) -> std::result::Result<Vec<Change>, PlanError> {
    let mut new = Vec::new();
    let mut seen = HashSet::new();
    for line in text.lines() {
        if let Some(line) = clean(line)? {
            if !seen.insert(line.to_lowercase()) {
                return Err(PlanError::Duplicate(line));
            }
            new.push(line);
        }
    }
    let old: Vec<String> = entries.iter().map(FolderEntry::line).collect();

    // Hunks of (removed, added) lines between aligned ones.
    let (n, m) = (old.len(), new.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut hunks: Vec<(Vec<&str>, Vec<&str>)> = vec![(Vec::new(), Vec::new())];
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && old[i] == new[j] {
            hunks.push((Vec::new(), Vec::new()));
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            hunks.last_mut().expect("a hunk").1.push(&new[j]);
            j += 1;
        } else {
            hunks.last_mut().expect("a hunk").0.push(&old[i]);
            i += 1;
        }
    }

    // A line moved within the text, unchanged, is no change.
    let added_all: HashSet<&str> = hunks.iter().flat_map(|h| h.1.iter().copied()).collect();
    let removed_all: HashSet<&str> = hunks.iter().flat_map(|h| h.0.iter().copied()).collect();
    for hunk in &mut hunks {
        hunk.0.retain(|o| !added_all.contains(o));
        hunk.1.retain(|a| !removed_all.contains(a));
    }

    let is_dir = |p: &str| p.ends_with('/');
    let mut moves = Vec::new();
    // Same file name, another folder: a move, wherever the lines are.
    for h in 0..hunks.len() {
        let mut k = 0;
        while k < hunks[h].1.len() {
            let added = hunks[h].1[k];
            let found = hunks.iter().enumerate().find_map(|(hh, hunk)| {
                hunk.0
                    .iter()
                    .position(|&o| is_dir(o) == is_dir(added) && file_name(o) == file_name(added))
                    .map(|pos| (hh, pos))
            });
            match found {
                Some((hh, pos)) => {
                    let from = hunks[hh].0.remove(pos);
                    hunks[h].1.remove(k);
                    moves.push(Change::Move {
                        from: from.to_owned(),
                        to: added.to_owned(),
                    });
                }
                None => k += 1,
            }
        }
    }
    // An edited line in place: a rename, in order within its hunk.
    let mut created = Vec::new();
    let mut trashed = Vec::new();
    for (mut removed, added) in hunks {
        for line in added {
            match removed.iter().position(|&o| is_dir(o) == is_dir(line)) {
                Some(pos) => moves.push(Change::Move {
                    from: removed.remove(pos).to_owned(),
                    to: line.to_owned(),
                }),
                None => created.push(Change::New(line.to_owned())),
            }
        }
        trashed.extend(removed.into_iter().map(|r| Change::Trash(r.to_owned())));
    }
    let mut changes = trashed;
    changes.extend(moves);
    changes.extend(created);
    Ok(changes)
}

/// Applies `changes` to `folder` (vault-relative, `""` for the root): trash,
/// then moves (through temporary names when one move's target is another's
/// source, so swaps work), then new notes and folders. Stops at the first
/// failure, returning what was done so far alongside the error.
pub fn apply(vault: &Vault, folder: &str, changes: &[Change]) -> (Vec<FileOp>, Option<VaultError>) {
    let path = |rel: &str| -> Result<VaultPath> {
        let rel = rel.trim_end_matches('/');
        let full = if folder.is_empty() {
            rel.to_owned()
        } else {
            format!("{folder}/{rel}")
        };
        VaultPath::new(full)
    };
    let mut done = Vec::new();
    let mut run = || -> Result<()> {
        for change in changes {
            if let Change::Trash(rel) = change {
                done.push(vault.trash(&path(rel)?)?);
            }
        }
        let moves: Vec<(VaultPath, VaultPath)> = changes
            .iter()
            .filter_map(|c| match c {
                Change::Move { from, to } => Some((path(from), path(to))),
                _ => None,
            })
            .map(|(a, b)| Ok((a?, b?)))
            .collect::<Result<_>>()?;
        let sources: HashSet<&VaultPath> = moves.iter().map(|(from, _)| from).collect();
        let clash = moves.iter().any(|(_, to)| sources.contains(to));
        if clash {
            // Every source steps aside first, then moves to its target.
            let mut parked = Vec::new();
            for (i, (from, to)) in moves.iter().enumerate() {
                let temp = VaultPath::new(format!("{from}.mnote-move-{i}"))?;
                vault.rename(from, &temp)?;
                parked.push((temp, from, to));
            }
            for (temp, from, to) in parked {
                vault.rename(&temp, to)?;
                done.push(FileOp::Moved {
                    from: from.clone(),
                    to: to.clone(),
                });
            }
        } else {
            for (from, to) in &moves {
                done.push(vault.rename(from, to)?);
            }
        }
        for change in changes {
            if let Change::New(rel) = change {
                if rel.ends_with('/') {
                    let dir = vault.root().join(path(rel)?.as_path());
                    std::fs::create_dir_all(&dir).map_err(|e| VaultError::io(&dir, e))?;
                } else {
                    let note = NotePath::new(path(rel)?.as_path())?;
                    done.push(vault.create_note(&note)?);
                }
            }
        }
        Ok(())
    };
    let result = run();
    (done, result.err())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::tests::TempDir;

    fn entries() -> Vec<FolderEntry> {
        let e = |name: &str, is_dir| FolderEntry {
            name: name.into(),
            is_dir,
        };
        vec![
            e("assets", true),
            e("design", true),
            e("hotkeys.md", false),
            e("ideas.md", false),
            e("ui-design.md", false),
            e("vim-keys.md", false),
        ]
    }

    fn changes(text: &str) -> Vec<String> {
        plan(&entries(), text)
            .expect("plan")
            .iter()
            .map(|c| format!("{} {c}", c.verb()))
            .collect()
    }

    #[test]
    fn text_round_trips_with_no_changes() {
        let text = folder_text(&entries());
        assert_eq!(
            text,
            "assets/\ndesign/\nhotkeys.md\nideas.md\nui-design.md\nvim-keys.md\n"
        );
        assert!(changes(&text).is_empty());
        assert!(
            changes("vim-keys.md\nideas.md\n./hotkeys.md\nassets/\ndesign/\nui-design.md\n\n")
                .is_empty(),
            "order and blank lines don't matter"
        );
    }

    #[test]
    fn moves_renames_trash_and_new() {
        // New note added at the top, ui-design moved into design/, ideas
        // renamed in place, vim-keys deleted, a folder added.
        let text =
            "new-note\nassets/\ndesign/\ndesign/ui-design.md\nhotkeys.md\nkeymap.md\ndrafts/\n";
        assert_eq!(
            changes(text),
            [
                "trash vim-keys.md",
                "move ui-design.md → design/ui-design.md",
                "rename ideas.md → keymap.md",
                "new new-note.md",
                "new drafts/",
            ]
        );
        assert_eq!(
            changes("assets/\ndesigns/\nhotkeys.md\nideas.md\nui-design.md\nvim-keys.md\n"),
            ["rename design/ → designs/"]
        );
    }

    #[test]
    fn bad_lines_are_refused() {
        assert_eq!(
            plan(&entries(), "../out.md"),
            Err(PlanError::BadLine("../out.md".into()))
        );
        assert_eq!(
            plan(&entries(), ".hidden.md"),
            Err(PlanError::BadLine(".hidden.md".into()))
        );
        assert_eq!(
            plan(&entries(), "a.md\nA.md"),
            Err(PlanError::Duplicate("A.md".into()))
        );
    }

    #[test]
    fn applies_in_a_safe_order_including_swaps() {
        let dir = TempDir::new();
        let trash = TempDir::new();
        dir.file("notes/a.md", "A");
        dir.file("notes/b.md", "B");
        dir.file("notes/gone.md", "x");
        let vault = Vault::open(&dir.0).expect("vault").with_trash(&trash.0);
        let entries = vec![
            FolderEntry {
                name: "a.md".into(),
                is_dir: false,
            },
            FolderEntry {
                name: "b.md".into(),
                is_dir: false,
            },
            FolderEntry {
                name: "gone.md".into(),
                is_dir: false,
            },
        ];
        // Swap a and b, trash gone, add a folder and a note.
        let changes = vec![
            Change::Trash("gone.md".into()),
            Change::Move {
                from: "a.md".into(),
                to: "b.md".into(),
            },
            Change::Move {
                from: "b.md".into(),
                to: "a.md".into(),
            },
            Change::New("sub/".into()),
            Change::New("sub/c.md".into()),
        ];
        let _ = entries;
        let (done, err) = apply(&vault, "notes", &changes);
        assert!(err.is_none(), "{err:?}");
        assert_eq!(done.len(), 4, "trash, two moves, the note");
        assert_eq!(
            std::fs::read_to_string(dir.0.join("notes/a.md")).expect("a"),
            "B"
        );
        assert_eq!(
            std::fs::read_to_string(dir.0.join("notes/b.md")).expect("b"),
            "A"
        );
        assert!(dir.0.join("notes/sub/c.md").is_file());
        assert!(!dir.0.join("notes/gone.md").exists());
    }
}
