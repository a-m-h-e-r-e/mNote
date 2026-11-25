//! Edit groups that span several buffers (ADR-0010).
//!
//! An edit to many files (a rename, a code action, later an agent's turn)
//! is one group: every buffer it touched records a step with the same
//! [`GroupId`]. Undoing it in one buffer undoes it in the others whose next
//! step is that group; a buffer edited since keeps its newer text, and is
//! reported as skipped.

use std::collections::{HashMap, VecDeque};

use super::{Buffer, GroupId};

/// Groups remembered, at most; older ones undo in their own buffer only.
const GROUPS_KEPT: usize = 256;

/// Which documents each multi-document group touched. `K` is however the
/// caller names a document.
#[derive(Debug)]
pub struct GroupDocs<K> {
    docs: HashMap<GroupId, Vec<K>>,
    order: VecDeque<GroupId>,
}

impl<K> Default for GroupDocs<K> {
    fn default() -> Self {
        GroupDocs {
            docs: HashMap::new(),
            order: VecDeque::new(),
        }
    }
}

impl<K: Clone + PartialEq> GroupDocs<K> {
    /// Remembers that `group` touched `docs` (only worth it for two or more).
    pub fn record(&mut self, group: GroupId, docs: Vec<K>) {
        if docs.len() < 2 {
            return;
        }
        if self.order.len() == GROUPS_KEPT
            && let Some(oldest) = self.order.pop_front()
        {
            self.docs.remove(&oldest);
        }
        self.order.push_back(group);
        self.docs.insert(group, docs);
    }

    /// Whether a remembered group touched `doc` (its buffer should stay
    /// open so the group can still be undone there).
    pub fn contains(&self, doc: &K) -> bool {
        self.docs.values().any(|docs| docs.contains(doc))
    }

    /// The documents `group` touched besides `except`; empty for a group in
    /// one document.
    pub fn others(&self, group: GroupId, except: &K) -> Vec<K> {
        self.docs
            .get(&group)
            .map(|docs| docs.iter().filter(|d| *d != except).cloned().collect())
            .unwrap_or_default()
    }
}

impl Buffer {
    /// Undoes (or with `redo`, redoes) the next step if it belongs to
    /// `group`; returns where the cursor goes, or `None` when this buffer's
    /// next step is something else (it changed since).
    pub fn undo_group(&mut self, group: GroupId, redo: bool) -> Option<usize> {
        let next = if redo {
            self.next_redo()
        } else {
            self.next_undo()
        };
        if next?.group != group {
            return None;
        }
        let cursor = if redo { self.redo() } else { self.undo() };
        // The caller is already carrying this group to the other buffers.
        self.undone = None;
        cursor
    }
}

/// Undoes (or redoes) `group` in each of `buffers` whose next step is that
/// group. Returns how many did, and how many were skipped because they
/// changed since.
pub fn undo_across<'a>(
    buffers: impl IntoIterator<Item = &'a mut Buffer>,
    group: GroupId,
    redo: bool,
) -> (usize, usize) {
    let (mut done, mut skipped) = (0, 0);
    for buffer in buffers {
        match buffer.undo_group(group, redo) {
            Some(_) => done += 1,
            None => skipped += 1,
        }
    }
    (done, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Origin;

    /// One group's edits in each buffer, as a multi-file edit makes them.
    fn edit_all(buffers: &mut [&mut Buffer], group: GroupId, text: &str) {
        for b in buffers.iter_mut() {
            b.begin_group(0, group, Some("Rename"));
            b.edit(0..0, text, 0, Origin::Lsp("ra".into()))
                .expect("edit");
            b.end_change(0);
        }
    }

    #[test]
    fn a_group_undoes_and_redoes_in_every_buffer() {
        let (mut a, mut b, mut c) = (Buffer::new("a"), Buffer::new("b"), Buffer::new("c"));
        let group = GroupId::fresh();
        edit_all(&mut [&mut a, &mut b, &mut c], group, "x");
        let info = a.next_undo().expect("a step");
        assert_eq!(info.group, group);
        assert_eq!(info.origin, Origin::Lsp("ra".into()));
        assert_eq!(info.label.as_deref(), Some("Rename"));

        // `u` in `a`, then the app carries it to the others.
        a.undo();
        let undone = a.take_undone().expect("undone");
        assert_eq!((undone.group, undone.redo), (group, false));
        assert_eq!(a.take_undone(), None, "once");
        assert_eq!(undo_across([&mut b, &mut c], group, false), (2, 0));
        assert_eq!(
            (a.to_string(), b.to_string(), c.to_string()),
            ("a".into(), "b".into(), "c".into())
        );
        assert_eq!(b.take_undone(), None, "carried, not reported again");

        a.redo();
        assert!(a.take_undone().is_some_and(|u| u.redo));
        assert_eq!(undo_across([&mut b, &mut c], group, true), (2, 0));
        assert_eq!(
            (a.to_string(), b.to_string(), c.to_string()),
            ("xa".into(), "xb".into(), "xc".into())
        );
    }

    #[test]
    fn a_buffer_edited_since_is_skipped() {
        let (mut a, mut b) = (Buffer::new("a"), Buffer::new("b"));
        let group = GroupId::fresh();
        edit_all(&mut [&mut a, &mut b], group, "x");
        b.edit(2..2, "!", 2, Origin::User).expect("edit");
        a.undo();
        assert_eq!(undo_across([&mut b], group, false), (0, 1));
        assert_eq!(b.to_string(), "xb!", "its newer text stays");
    }

    #[test]
    fn single_buffer_undo_is_unchanged() {
        let mut a = Buffer::new("");
        a.edit(0..0, "1", 0, Origin::User).expect("edit");
        a.begin_change(1);
        a.edit(1..1, "2", 1, Origin::User).expect("edit");
        a.edit(2..2, "3", 2, Origin::User).expect("edit");
        a.end_change(3);
        assert_ne!(a.next_undo().map(|s| s.group), None);
        a.undo();
        assert_eq!(a.to_string(), "1");
        a.undo();
        assert_eq!(a.to_string(), "");
        assert_eq!(a.next_undo(), None);
        a.redo();
        assert_eq!(a.to_string(), "1");
    }

    #[test]
    fn groups_remember_their_documents() {
        let mut docs = GroupDocs::default();
        let g = GroupId::fresh();
        docs.record(g, vec!["a.md", "b.md", "c.md"]);
        assert_eq!(docs.others(g, &"b.md"), ["a.md", "c.md"]);
        assert!(docs.contains(&"c.md") && !docs.contains(&"z.md"));
        let alone = GroupId::fresh();
        docs.record(alone, vec!["a.md"]);
        assert!(docs.others(alone, &"a.md").is_empty());
        for _ in 0..GROUPS_KEPT {
            docs.record(GroupId::fresh(), vec!["x", "y"]);
        }
        assert!(
            docs.others(g, &"a.md").is_empty(),
            "the oldest are forgotten"
        );
        assert_ne!(GroupId::fresh(), GroupId::fresh());
    }
}
