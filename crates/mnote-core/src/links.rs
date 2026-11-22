//! Wikilinks (`[[name]]`, `[[folder/name|label]]`, `[[name#heading]]`) and
//! keeping them pointing at the right note when notes are renamed or moved.
//!
//! A link names a note by its name (`[[ui-design]]`) or by its path without
//! `.md` (`[[projects/ui-design]]`), in any letter case. When a note's name
//! changes, links by name follow it; when its folder changes, links by path
//! follow it. Labels (`|…`) and headings (`#…`) are kept.

use std::collections::HashMap;

use crate::note_path::NotePath;
use crate::vault::Vault;

/// A note that moved from one path to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkMove {
    pub from: NotePath,
    pub to: NotePath,
}

/// A note path without its extension, the way links write it.
fn link_path(note: &NotePath) -> String {
    let text = note.to_string();
    let ext = note.as_path().extension().map_or(0, |e| e.len() + 1);
    text[..text.len() - ext].to_owned()
}

/// What to replace each link target with, by lowercase target.
struct Rewrites {
    by_path: HashMap<String, String>,
    by_name: HashMap<String, String>,
}

impl Rewrites {
    fn new(moves: &[LinkMove]) -> Self {
        let mut by_path = HashMap::new();
        let mut by_name = HashMap::new();
        for m in moves {
            by_path.insert(link_path(&m.from).to_lowercase(), link_path(&m.to));
            if m.from.name() != m.to.name() {
                by_name.insert(m.from.name().to_lowercase(), m.to.name().to_owned());
            }
        }
        Rewrites { by_path, by_name }
    }

    fn target(&self, target: &str) -> Option<&str> {
        let (bare, _) = split_ext(target);
        let key = bare.to_lowercase();
        if bare.contains('/') {
            self.by_path.get(&key).map(String::as_str)
        } else {
            self.by_name
                .get(&key)
                .or_else(|| self.by_path.get(&key))
                .map(String::as_str)
        }
    }
}

/// `name.md` → (`name`, `.md`).
fn split_ext(target: &str) -> (&str, &str) {
    for ext in [".md", ".markdown"] {
        if target.len() > ext.len() && target[target.len() - ext.len()..].eq_ignore_ascii_case(ext)
        {
            return target.split_at(target.len() - ext.len());
        }
    }
    (target, "")
}

/// Rewrites the wikilinks in `text` that point at a moved note. Returns the
/// new text and how many links changed, or `None` if none did.
pub fn rewrite_wikilinks(text: &str, moves: &[LinkMove]) -> Option<(String, usize)> {
    if moves.is_empty() || !text.contains("[[") {
        return None;
    }
    let rewrites = Rewrites::new(moves);
    let mut out = String::with_capacity(text.len());
    let mut changed = 0;
    let mut rest = text;
    while let Some(open) = rest.find("[[") {
        let after = &rest[open + 2..];
        // A link ends at `]]` on the same line.
        let Some(close) = after.find("]]").filter(|&c| !after[..c].contains('\n')) else {
            out.push_str(&rest[..open + 2]);
            rest = after;
            continue;
        };
        let inner = &after[..close];
        let target_end = inner.find(['|', '#']).unwrap_or(inner.len());
        let target = inner[..target_end].trim();
        out.push_str(&rest[..open + 2]);
        match rewrites.target(target) {
            Some(new) => {
                let (_, ext) = split_ext(target);
                out.push_str(new);
                out.push_str(ext);
                out.push_str(&inner[target_end..]);
                changed += 1;
            }
            None => out.push_str(inner),
        }
        out.push_str("]]");
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    (changed > 0).then_some((out, changed))
}

/// How many links in `notes` point at notes in `moves` (what
/// [`relink_vault`] would rewrite), without changing anything. Call it off
/// the UI thread.
pub fn count_relinks(vault: &Vault, notes: &[NotePath], moves: &[LinkMove]) -> usize {
    if moves.is_empty() {
        return 0;
    }
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(16);
    let chunk = notes.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let workers: Vec<_> = notes
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .filter_map(|note| {
                            let text = vault.read(note).ok()?;
                            rewrite_wikilinks(&text, moves).map(|(_, n)| n)
                        })
                        .sum::<usize>()
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().unwrap_or(0)).sum()
    })
}

/// A note whose links were rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relinked {
    pub note: NotePath,
    /// The note's new text, as saved.
    pub text: String,
    pub links: usize,
}

/// Rewrites links to moved notes across `notes` (read in parallel) and saves
/// each changed note atomically. Notes that can't be read or written are
/// skipped. Call it off the UI thread.
pub fn relink_vault(vault: &Vault, notes: &[NotePath], moves: &[LinkMove]) -> Vec<Relinked> {
    if moves.is_empty() {
        return Vec::new();
    }
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(16);
    let chunk = notes.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let workers: Vec<_> = notes
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .filter_map(|note| {
                            let text = vault.read(note).ok()?;
                            let (text, links) = rewrite_wikilinks(&text, moves)?;
                            vault.write(note, &text).ok()?;
                            Some(Relinked {
                                note: note.clone(),
                                text,
                                links,
                            })
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_default())
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mv(from: &str, to: &str) -> LinkMove {
        LinkMove {
            from: NotePath::new(from).expect("path"),
            to: NotePath::new(to).expect("path"),
        }
    }

    #[test]
    fn renames_follow_links_by_name_and_path() {
        let text = "See [[ui-design]], [[UI-Design|the design]], [[projects/ui-design#Principles]] and [[ui-design.md]].\n[[other]] stays.";
        let (out, n) = rewrite_wikilinks(text, &[mv("projects/ui-design.md", "projects/ui.md")])
            .expect("changed");
        assert_eq!(
            out,
            "See [[ui]], [[ui|the design]], [[projects/ui#Principles]] and [[ui.md]].\n[[other]] stays."
        );
        assert_eq!(n, 4);
    }

    #[test]
    fn moves_keep_links_by_name_and_fix_paths() {
        let text = "[[ui-design]] and [[projects/ui-design]]";
        let (out, n) =
            rewrite_wikilinks(text, &[mv("projects/ui-design.md", "archive/ui-design.md")])
                .expect("changed");
        assert_eq!(out, "[[ui-design]] and [[archive/ui-design]]");
        assert_eq!(n, 1);
    }

    #[test]
    fn relinks_a_vault() {
        let dir = crate::vault::tests::TempDir::new();
        dir.file("a.md", "links to [[b]]");
        dir.file("c.md", "nothing");
        let vault = Vault::open(&dir.0).expect("vault");
        let notes = vault.notes().expect("notes");
        let done = relink_vault(&vault, &notes, &[mv("b.md", "bee.md")]);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].links, 1);
        assert_eq!(
            std::fs::read_to_string(dir.0.join("a.md")).expect("read"),
            "links to [[bee]]"
        );
    }

    #[test]
    fn leaves_everything_else_alone() {
        let moves = [mv("a.md", "b.md")];
        assert_eq!(rewrite_wikilinks("no links", &moves), None);
        assert_eq!(rewrite_wikilinks("[[c]] [[ab]]", &moves), None);
        assert_eq!(rewrite_wikilinks("broken [[a\n]] link", &moves), None);
        assert_eq!(
            rewrite_wikilinks("[[a]] [[a", &moves).map(|r| r.0),
            Some("[[b]] [[a".into())
        );
    }
}
