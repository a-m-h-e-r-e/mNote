//! The change log: every edit to a buffer, recorded once, with who made it
//! (ADR-0005, ADR-0010).
//!
//! One record feeds everything that follows the text: the syntax tree's
//! `InputEdit`s, a language server's incremental `didChange`, and moving a
//! position found at an older revision onto the current text. Consumers
//! subscribe and say which revision they've seen; changes every consumer has
//! seen are dropped. A short tail of the user's own edits is kept apart, as
//! context for features that want to know what was just typed.

use std::collections::VecDeque;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use mnote_lang::coords::{ByteOffset, CharOffset, TextPoint};
use tree_sitter::InputEdit;

/// Who made an edit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum Origin {
    /// The user, typing or running commands.
    #[default]
    User,
    /// A plugin, by name.
    Plugin(Arc<str>),
    /// A language server, by name.
    Lsp(Arc<str>),
    /// An agent, by session.
    Agent(Arc<str>),
    /// An accepted edit prediction.
    Prediction,
    /// The file changed on disk.
    External,
}

impl fmt::Display for Origin {
    /// As plugins see it: `user`, `plugin:tasks`, `lsp:rust-analyzer`,
    /// `agent:<session>`, `prediction`, `external`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::User => f.write_str("user"),
            Origin::Plugin(name) => write!(f, "plugin:{name}"),
            Origin::Lsp(name) => write!(f, "lsp:{name}"),
            Origin::Agent(session) => write!(f, "agent:{session}"),
            Origin::Prediction => f.write_str("prediction"),
            Origin::External => f.write_str("external"),
        }
    }
}

/// The undo step an edit belongs to: edits with the same id undo together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct GroupId(pub u64);

/// One recorded edit: the characters `range` (on the text before it) became
/// `new_text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub range: Range<CharOffset>,
    pub old_bytes: Range<ByteOffset>,
    pub new_text: Arc<str>,
    /// How many characters `new_text` has.
    pub new_chars: usize,
    /// Where the range started and ended before the edit, and where the new
    /// text ends after it, in every unit (LSP needs the old range in its own
    /// encoding, which can't be worked out once the old text is gone).
    pub start: TextPoint,
    pub old_end: TextPoint,
    pub new_end: TextPoint,
    /// The buffer's revision after this edit.
    pub revision: u64,
    pub origin: Origin,
    pub group: GroupId,
}

impl Change {
    /// The edit as tree-sitter takes it.
    pub fn input_edit(&self) -> InputEdit {
        InputEdit {
            start_byte: self.old_bytes.start.0,
            old_end_byte: self.old_bytes.end.0,
            new_end_byte: self.old_bytes.start.0 + self.new_text.len(),
            start_position: self.start.point().into(),
            old_end_position: self.old_end.point().into(),
            new_end_position: self.new_end.point().into(),
        }
    }

    /// Where position `at` (on the text before this edit) is after it. A
    /// position inside the replaced text, or exactly where text was
    /// inserted, goes to its start or past the new text, by `bias`.
    pub fn map(&self, at: CharOffset, bias: Bias) -> CharOffset {
        let (start, end) = (self.range.start.0, self.range.end.0);
        let after = start + self.new_chars;
        CharOffset(match at.0 {
            p if p < start => p,
            p if p > end => p - (end - start) + self.new_chars,
            // At the end of a replaced range: just past the new text.
            p if p == end && start < end => after,
            // At the start of the range, inside it, or at an insertion.
            _ => match bias {
                Bias::Before => start,
                Bias::After => after,
            },
        })
    }
}

/// Which side of an edit at exactly its position a position stays on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Bias {
    /// Before text inserted there.
    #[default]
    Before,
    /// After it.
    After,
}

/// A subscriber to a buffer's changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConsumerId(usize);

/// Changes kept for consumers that haven't seen them, at most.
const MAX_CHANGES: usize = 4096;
/// The user's own most recent edits, kept for context.
const RECENT_USER_EDITS: usize = 64;

/// A buffer's recorded edits.
#[derive(Debug, Default)]
pub struct ChangeLog {
    changes: VecDeque<Change>,
    /// The revision just before the oldest kept change.
    base: u64,
    /// The latest revision recorded.
    latest: u64,
    /// The revision each consumer has seen (`None`: unsubscribed).
    consumers: Vec<Option<u64>>,
    recent_user: VecDeque<Change>,
    last_origin: Option<Origin>,
}

impl ChangeLog {
    /// A log that starts at `revision`.
    pub fn new(revision: u64) -> Self {
        ChangeLog {
            base: revision,
            latest: revision,
            ..ChangeLog::default()
        }
    }

    /// Records a change; drops what every consumer has seen.
    pub fn push(&mut self, change: Change) {
        self.latest = change.revision;
        self.last_origin = Some(change.origin.clone());
        if change.origin == Origin::User {
            if self.recent_user.len() == RECENT_USER_EDITS {
                self.recent_user.pop_front();
            }
            self.recent_user.push_back(change.clone());
        }
        self.changes.push_back(change);
        self.trim();
    }

    /// Starts following changes from the latest revision on.
    pub fn subscribe(&mut self) -> ConsumerId {
        let seen = Some(self.latest);
        match self.consumers.iter().position(Option::is_none) {
            Some(free) => {
                self.consumers[free] = seen;
                ConsumerId(free)
            }
            None => {
                self.consumers.push(seen);
                ConsumerId(self.consumers.len() - 1)
            }
        }
    }

    /// Stops following; what only it was waiting for can go.
    pub fn unsubscribe(&mut self, id: ConsumerId) {
        if let Some(slot) = self.consumers.get_mut(id.0) {
            *slot = None;
        }
        self.trim();
    }

    /// `id` has seen every change up to `revision`.
    pub fn ack(&mut self, id: ConsumerId, revision: u64) {
        if let Some(Some(seen)) = self.consumers.get_mut(id.0) {
            *seen = (*seen).max(revision.min(self.latest));
        }
        self.trim();
    }

    /// The changes after `revision`, oldest first; `None` when some of them
    /// were already dropped (the consumer has to start over from the text).
    pub fn since(&self, revision: u64) -> Option<impl Iterator<Item = &Change>> {
        if revision < self.base {
            return None;
        }
        Some(self.changes.iter().filter(move |c| c.revision > revision))
    }

    /// Moves a position found at `revision` onto the latest text; `None` if
    /// the changes since then were dropped.
    pub fn rebase(&self, at: CharOffset, revision: u64, bias: Bias) -> Option<CharOffset> {
        Some(self.since(revision)?.fold(at, |at, c| c.map(at, bias)))
    }

    /// The user's own most recent edits, oldest first.
    pub fn recent_user_edits(&self) -> impl Iterator<Item = &Change> {
        self.recent_user.iter()
    }

    /// The origin of the latest change, if one was recorded.
    pub fn last_origin(&self) -> Option<&Origin> {
        self.last_origin.as_ref()
    }

    fn trim(&mut self) {
        let oldest_needed = self
            .consumers
            .iter()
            .flatten()
            .copied()
            .min()
            .unwrap_or(self.latest);
        while let Some(front) = self.changes.front() {
            let seen_by_all = front.revision <= oldest_needed;
            if !seen_by_all && self.changes.len() <= MAX_CHANGES {
                break;
            }
            self.base = front.revision;
            self.changes.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(start: usize, end: usize, new: &str, revision: u64, origin: Origin) -> Change {
        Change {
            range: CharOffset(start)..CharOffset(end),
            old_bytes: ByteOffset(start)..ByteOffset(end),
            new_text: new.into(),
            new_chars: new.chars().count(),
            start: TextPoint::default(),
            old_end: TextPoint::default(),
            new_end: TextPoint::default(),
            revision,
            origin,
            group: GroupId(revision),
        }
    }

    #[test]
    fn positions_move_through_an_edit() {
        // "hello world": "lo w" (3..7) becomes "X".
        let c = change(3, 7, "X", 1, Origin::User);
        let map = |p, bias| c.map(CharOffset(p), bias).0;
        assert_eq!(map(2, Bias::Before), 2);
        assert_eq!(map(9, Bias::Before), 6);
        assert_eq!(
            map(7, Bias::Before),
            4,
            "the end of the range: past the new text"
        );
        assert_eq!(map(5, Bias::Before), 3);
        assert_eq!(map(5, Bias::After), 4);
        assert_eq!(map(3, Bias::Before), 3);
        // An insertion at a position: the bias decides.
        let insert = change(3, 3, "ab", 1, Origin::User);
        assert_eq!(insert.map(CharOffset(3), Bias::Before), CharOffset(3));
        assert_eq!(insert.map(CharOffset(3), Bias::After), CharOffset(5));
    }

    #[test]
    fn consumers_see_changes_until_they_ack() {
        let mut log = ChangeLog::new(0);
        let a = log.subscribe();
        log.push(change(0, 0, "x", 1, Origin::User));
        let b = log.subscribe();
        log.push(change(1, 1, "y", 2, Origin::User));
        assert_eq!(log.since(0).map(Iterator::count), Some(2));
        assert_eq!(log.since(1).map(Iterator::count), Some(1));
        log.ack(a, 1);
        // `a` is at 1 and `b` at 1: the first change can go.
        assert_eq!(log.since(0).map(|_| ()), None, "dropped: start over");
        assert_eq!(log.since(1).map(Iterator::count), Some(1));
        log.ack(a, 2);
        log.ack(b, 2);
        assert_eq!(log.since(2).map(Iterator::count), Some(0));
        assert_eq!(log.since(1).map(|_| ()), None);
        // An unsubscribed consumer doesn't hold changes back.
        let c = log.subscribe();
        log.unsubscribe(a);
        log.unsubscribe(b);
        log.push(change(2, 2, "z", 3, Origin::External));
        assert_eq!(
            log.since(2).map(Iterator::count),
            Some(1),
            "c hasn't seen it"
        );
        log.unsubscribe(c);
        assert_eq!(log.since(2).map(|_| ()), None, "nobody waits for it now");
        assert_eq!(log.last_origin(), Some(&Origin::External));
        // Freed slots are reused.
        assert_eq!(log.subscribe(), a);
    }

    #[test]
    fn a_position_rebases_through_several_changes() {
        let mut log = ChangeLog::new(10);
        let _ = log.subscribe();
        log.push(change(0, 0, "ab", 11, Origin::User)); // 2 inserted before
        log.push(change(8, 10, "", 12, Origin::User)); // 2 deleted after it
        log.push(change(1, 2, "XYZ", 13, Origin::User)); // 2 more, before it
        assert_eq!(
            log.rebase(CharOffset(5), 10, Bias::Before),
            Some(CharOffset(9))
        );
        assert_eq!(
            log.rebase(CharOffset(5), 13, Bias::Before),
            Some(CharOffset(5))
        );
        assert_eq!(log.rebase(CharOffset(5), 9, Bias::Before), None);
    }

    #[test]
    fn the_log_is_bounded_and_keeps_recent_user_edits() {
        let mut log = ChangeLog::new(0);
        let _slow = log.subscribe();
        for r in 1..=(MAX_CHANGES as u64 + 10) {
            let origin = if r % 2 == 0 {
                Origin::User
            } else {
                Origin::External
            };
            log.push(change(0, 0, "x", r, origin));
        }
        assert!(
            log.since(0).is_none(),
            "a consumer that never acks loses the oldest"
        );
        assert_eq!(log.since(10).map(Iterator::count), Some(MAX_CHANGES));
        let recent: Vec<_> = log.recent_user_edits().collect();
        assert_eq!(recent.len(), RECENT_USER_EDITS);
        assert!(recent.iter().all(|c| c.origin == Origin::User));
        assert_eq!(
            recent.last().map(|c| c.revision),
            Some(MAX_CHANGES as u64 + 10)
        );
        assert_eq!(log.last_origin(), Some(&Origin::User));
    }

    #[test]
    fn origins_read_as_plugins_see_them() {
        let names: Vec<String> = [
            Origin::User,
            Origin::Plugin("tasks".into()),
            Origin::Lsp("rust-analyzer".into()),
            Origin::Agent("s1".into()),
            Origin::Prediction,
            Origin::External,
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(
            names,
            [
                "user",
                "plugin:tasks",
                "lsp:rust-analyzer",
                "agent:s1",
                "prediction",
                "external"
            ]
        );
    }
}
