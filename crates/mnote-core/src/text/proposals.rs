//! Proposals: edits held for review instead of applied (ADR-0010).
//!
//! A proposal is a set of hunks, each "this text here becomes that". A hunk
//! is anchored with a mark, so it moves with every edit as the user keeps
//! typing, and it remembers the text it expects to replace: when the text
//! under it is no longer that (the user edited inside it), it's stale and
//! won't apply. Its start stays after, and its end before, text typed
//! exactly at its edges, so typing next to a hunk leaves it valid.
//!
//! Accepting applies the chosen hunks as one undo step of the proposal's
//! group, origin and label; rejecting drops them. A proposal may span
//! several buffers: each holds its own hunks under the same [`ProposalId`],
//! and [`accept_across`] applies them as one group in all of them. Where a
//! proposal comes from (a language server's workspace edit, a plugin, later
//! an agent) doesn't matter here; drawing and reviewing it is the app's.

use std::ops::Range;
use std::sync::Arc;

use super::{Bias, Buffer, GroupId, MarkKind, Origin};

/// A proposal's id, unique in the process (shared by every buffer it spans).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProposalId(pub u64);

impl ProposalId {
    /// A new id, never handed out before.
    pub fn fresh() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        ProposalId(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

/// Where a hunk stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HunkState {
    /// Waiting for review, and still applies.
    Pending,
    /// The text under it changed: it no longer applies.
    Stale,
}

/// A hunk as the review shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// Its number in the proposal (in this buffer), from 0; it stays the
    /// same as other hunks are accepted or rejected.
    pub index: usize,
    /// Where it is now, in characters.
    pub range: Range<usize>,
    /// The text it replaces, and what it puts there.
    pub old: Arc<str>,
    pub new: Arc<str>,
    pub state: HunkState,
}

/// What resolving hunks did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Resolved {
    pub accepted: usize,
    pub rejected: usize,
    /// Asked to accept, but stale: left for the review to drop.
    pub stale: usize,
}

impl std::ops::AddAssign for Resolved {
    fn add_assign(&mut self, other: Resolved) {
        self.accepted += other.accepted;
        self.rejected += other.rejected;
        self.stale += other.stale;
    }
}

/// Why edits couldn't become a proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposeError {
    /// A range is reversed or past the end of the text.
    OutOfBounds(Range<usize>),
    /// Two ranges overlap.
    Overlapping(Range<usize>, Range<usize>),
}

impl std::fmt::Display for ProposeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProposeError::OutOfBounds(r) => write!(f, "range {r:?} is outside the text"),
            ProposeError::Overlapping(a, b) => write!(f, "ranges {a:?} and {b:?} overlap"),
        }
    }
}

impl std::error::Error for ProposeError {}

/// One buffer's share of a proposal.
#[derive(Debug, Clone)]
pub(crate) struct Held {
    id: ProposalId,
    origin: Origin,
    label: Option<Arc<str>>,
    hunks: Vec<HeldHunk>,
}

#[derive(Debug, Clone)]
struct HeldHunk {
    index: usize,
    mark: u64,
    old: Arc<str>,
    new: Arc<str>,
}

/// Marks of proposals go in this namespace (the app draws them later).
const NAMESPACE: &str = "proposal";

impl Buffer {
    /// Holds `edits` (character ranges of the current text and their new
    /// text) as proposal `id`'s hunks in this buffer, sorted by position.
    pub fn propose(
        &mut self,
        id: ProposalId,
        edits: Vec<(Range<usize>, String)>,
        origin: Origin,
        label: Option<&str>,
    ) -> Result<(), ProposeError> {
        let len = self.len_chars();
        let mut edits = edits;
        edits.sort_by_key(|(r, _)| (r.start, r.end));
        for (r, _) in &edits {
            if r.start > r.end || r.end > len {
                return Err(ProposeError::OutOfBounds(r.clone()));
            }
        }
        for pair in edits.windows(2) {
            if pair[0].0.end > pair[1].0.start {
                return Err(ProposeError::Overlapping(
                    pair[0].0.clone(),
                    pair[1].0.clone(),
                ));
            }
        }
        let mut hunks = Vec::with_capacity(edits.len());
        for (index, (range, new)) in edits.into_iter().enumerate() {
            let old: Arc<str> = self.rope.slice(range.clone()).to_string().into();
            let kind = MarkKind::Proposal {
                proposal: id.0,
                new: new.clone(),
            };
            // An insertion stays put; a replacement shrinks, not grows, when
            // the user types at its edges.
            let (start, end) = if range.is_empty() {
                (Bias::Before, Bias::Before)
            } else {
                (Bias::After, Bias::Before)
            };
            let mark = self.marks.add_with_bias(NAMESPACE, range, kind, start, end);
            hunks.push(HeldHunk {
                index,
                mark,
                old,
                new: new.into(),
            });
        }
        self.proposals.retain(|h| h.id != id);
        self.proposals.push(Held {
            id,
            origin,
            label: label.map(Arc::from),
            hunks,
        });
        Ok(())
    }

    /// The proposals this buffer holds hunks of.
    pub fn proposals(&self) -> Vec<ProposalId> {
        self.proposals.iter().map(|h| h.id).collect()
    }

    /// Proposal `id`'s hunks still waiting here, in order, each pending or
    /// stale.
    pub fn hunks(&self, id: ProposalId) -> Vec<Hunk> {
        let Some(held) = self.proposals.iter().find(|h| h.id == id) else {
            return Vec::new();
        };
        held.hunks
            .iter()
            .filter_map(|h| {
                let range = self.marks.get(h.mark)?.range.clone();
                Some(Hunk {
                    index: h.index,
                    state: self.hunk_state(&range, &h.old),
                    range,
                    old: h.old.clone(),
                    new: h.new.clone(),
                })
            })
            .collect()
    }

    fn hunk_state(&self, range: &Range<usize>, old: &str) -> HunkState {
        let applies = range.end <= self.len_chars()
            && range.len() == old.chars().count()
            && self.rope.slice(range.clone()) == old;
        if applies {
            HunkState::Pending
        } else {
            HunkState::Stale
        }
    }

    /// Applies proposal `id`'s hunks numbered `which` (all, with `None`)
    /// as one undo step of `group`, with the proposal's origin and label.
    /// Stale ones are left (and counted); applied ones leave the proposal.
    pub fn accept(&mut self, id: ProposalId, which: Option<&[usize]>, group: GroupId) -> Resolved {
        let mut done = Resolved::default();
        let Some(at) = self.proposals.iter().position(|h| h.id == id) else {
            return done;
        };
        let held = self.proposals[at].clone();
        let chosen = |index: usize| which.is_none_or(|w| w.contains(&index));
        // Back to front, so earlier hunks keep their positions.
        let mut apply: Vec<(Range<usize>, Arc<str>, usize, u64)> = Vec::new();
        for h in held.hunks.iter().filter(|h| chosen(h.index)) {
            let Some(range) = self.marks.get(h.mark).map(|m| m.range.clone()) else {
                continue;
            };
            if self.hunk_state(&range, &h.old) == HunkState::Stale {
                done.stale += 1;
                continue;
            }
            apply.push((range, h.new.clone(), h.index, h.mark));
        }
        apply.sort_by_key(|(r, ..)| std::cmp::Reverse(r.start));
        if !apply.is_empty() {
            let cursor = apply.last().map_or(0, |(r, ..)| r.start);
            self.begin_group(cursor, group, held.label.as_deref());
            for (range, new, _, mark) in &apply {
                self.marks.remove(*mark);
                let _ = self.edit(range.clone(), new, range.start, held.origin.clone());
            }
            self.end_change(cursor);
        }
        done.accepted = apply.len();
        let applied: Vec<usize> = apply.iter().map(|(_, _, index, _)| *index).collect();
        self.forget_hunks(id, &applied);
        done
    }

    /// Drops proposal `id`'s hunks numbered `which` (all, with `None`),
    /// stale ones included; the text doesn't change.
    pub fn reject(&mut self, id: ProposalId, which: Option<&[usize]>) -> Resolved {
        let Some(held) = self.proposals.iter().find(|h| h.id == id) else {
            return Resolved::default();
        };
        let dropped: Vec<(usize, u64)> = held
            .hunks
            .iter()
            .filter(|h| which.is_none_or(|w| w.contains(&h.index)))
            .map(|h| (h.index, h.mark))
            .collect();
        for (_, mark) in &dropped {
            self.marks.remove(*mark);
        }
        let indices: Vec<usize> = dropped.iter().map(|(i, _)| *i).collect();
        self.forget_hunks(id, &indices);
        Resolved {
            rejected: dropped.len(),
            ..Resolved::default()
        }
    }

    fn forget_hunks(&mut self, id: ProposalId, indices: &[usize]) {
        if let Some(held) = self.proposals.iter_mut().find(|h| h.id == id) {
            held.hunks.retain(|h| !indices.contains(&h.index));
        }
        self.proposals.retain(|h| !h.hunks.is_empty());
    }
}

/// Accepts every pending hunk of proposal `id` in each of `buffers` as one
/// edit group (one undo step across them all). Returns the group, to record
/// which documents it touched, and what was done.
pub fn accept_across<'a>(
    buffers: impl IntoIterator<Item = &'a mut Buffer>,
    id: ProposalId,
) -> (GroupId, Resolved) {
    let group = GroupId::fresh();
    let mut done = Resolved::default();
    for buffer in buffers {
        done += buffer.accept(id, None, group);
    }
    (group, done)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n.max(1) as u64) as usize
        }
    }

    const PIECES: &[&str] = &["", "a", "é", "🙂", "\n", "日本", "xyz"];

    /// A text of segments, with every other one (the odd ones) a hunk's
    /// target, and the edits that replace them.
    fn case(rng: &mut Rng) -> (Vec<String>, Vec<String>) {
        let n = 1 + rng.below(6);
        let mut segments = Vec::new();
        let mut news = Vec::new();
        for _ in 0..n {
            segments.push(PIECES[rng.below(PIECES.len())].to_owned());
            segments.push(format!("<{}>", PIECES[rng.below(PIECES.len())]));
            news.push(PIECES[rng.below(PIECES.len())].to_owned());
        }
        segments.push("end".to_owned());
        (segments, news)
    }

    /// The hunks' ranges: the odd segments.
    fn edits(segments: &[String], news: &[String]) -> Vec<(Range<usize>, String)> {
        let mut at = 0;
        let mut out = Vec::new();
        for (i, s) in segments.iter().enumerate() {
            let len = s.chars().count();
            if i % 2 == 1 {
                out.push((at..at + len, news[i / 2].clone()));
            }
            at += len;
        }
        out
    }

    fn applied(segments: &[String], news: &[String]) -> String {
        segments
            .iter()
            .enumerate()
            .map(|(i, s)| if i % 2 == 1 { news[i / 2].as_str() } else { s })
            .collect()
    }

    #[test]
    fn accepting_every_hunk_equals_applying_the_edits() {
        let mut rng = Rng(0x1234_5678_9abc_def1);
        for _ in 0..300 {
            let (segments, news) = case(&mut rng);
            let mut b = Buffer::new(&segments.concat());
            let id = ProposalId::fresh();
            b.propose(
                id,
                edits(&segments, &news),
                Origin::Lsp("ra".into()),
                Some("Fix"),
            )
            .expect("valid");
            assert_eq!(
                b.to_string(),
                segments.concat(),
                "proposing changes nothing"
            );
            let done = b.accept(id, None, GroupId::fresh());
            assert_eq!(done.accepted, news.len());
            assert_eq!(b.to_string(), applied(&segments, &news));
            assert!(b.proposals().is_empty());
            // One undo step brings it all back.
            b.undo();
            assert_eq!(b.to_string(), segments.concat());
        }
    }

    #[test]
    fn rejecting_every_hunk_leaves_the_text() {
        let mut rng = Rng(0x0bad_cafe_dead_beef);
        for _ in 0..100 {
            let (segments, news) = case(&mut rng);
            let mut b = Buffer::new(&segments.concat());
            let id = ProposalId::fresh();
            b.propose(id, edits(&segments, &news), Origin::User, None)
                .expect("valid");
            let done = b.reject(id, None);
            assert_eq!(done.rejected, news.len());
            assert_eq!(b.to_string(), segments.concat());
            assert!(b.hunks(id).is_empty() && b.marks().is_empty());
            assert_eq!(b.next_undo(), None, "nothing to undo");
        }
    }

    #[test]
    fn edits_between_hunks_rebase_them() {
        // The user types between hunks (and right at their edges); every
        // hunk still applies, where its text moved to.
        let mut rng = Rng(0x5eed_5eed_5eed_5eed);
        for _ in 0..300 {
            let (mut segments, news) = case(&mut rng);
            let mut b = Buffer::new(&segments.concat());
            let id = ProposalId::fresh();
            b.propose(id, edits(&segments, &news), Origin::User, None)
                .expect("valid");
            for _ in 0..10 {
                // Into an even (gap) segment, anywhere in it, edges included.
                let gap = 2 * rng.below(segments.len().div_ceil(2));
                let before: usize = segments[..gap].iter().map(|s| s.chars().count()).sum();
                let within = rng.below(segments[gap].chars().count() + 1);
                let piece = PIECES[1 + rng.below(PIECES.len() - 1)];
                let at = before + within;
                b.edit(at..at, piece, at, Origin::User).expect("edit");
                let byte = segments[gap]
                    .char_indices()
                    .nth(within)
                    .map_or(segments[gap].len(), |(i, _)| i);
                segments[gap].insert_str(byte, piece);
            }
            assert!(b.hunks(id).iter().all(|h| h.state == HunkState::Pending));
            b.accept(id, None, GroupId::fresh());
            assert_eq!(b.to_string(), applied(&segments, &news));
        }
    }

    #[test]
    fn an_edit_inside_a_hunk_makes_it_stale() {
        let mut b = Buffer::new("one two three");
        let id = ProposalId::fresh();
        b.propose(
            id,
            vec![(0..3, "1".into()), (4..7, "2".into()), (8..13, "3".into())],
            Origin::User,
            Some("Numbers"),
        )
        .expect("valid");
        b.edit(5..6, "W", 5, Origin::User).expect("edit");
        let states: Vec<_> = b.hunks(id).iter().map(|h| h.state).collect();
        assert_eq!(
            states,
            [HunkState::Pending, HunkState::Stale, HunkState::Pending]
        );
        let done = b.accept(id, None, GroupId::fresh());
        assert_eq!((done.accepted, done.stale), (2, 1));
        assert_eq!(b.to_string(), "1 tWo 3");
        // The stale hunk waits for the review to drop it.
        assert_eq!(b.hunks(id).len(), 1);
        assert_eq!(b.reject(id, None).rejected, 1);
        assert!(b.proposals().is_empty());
    }

    #[test]
    fn hunks_can_be_taken_one_at_a_time() {
        let mut b = Buffer::new("aa bb cc");
        let id = ProposalId::fresh();
        b.propose(
            id,
            vec![(6..8, "C".into()), (0..2, "A".into()), (3..5, "B".into())],
            Origin::Plugin("p".into()),
            Some("Caps"),
        )
        .expect("valid");
        // Numbered by position, whatever order they came in.
        let indexes: Vec<usize> = b.hunks(id).iter().map(|h| h.index).collect();
        assert_eq!(indexes, [0, 1, 2]);
        b.accept(id, Some(&[1]), GroupId::fresh());
        assert_eq!(b.to_string(), "aa B cc");
        b.reject(id, Some(&[2]));
        let left = b.hunks(id);
        assert_eq!(
            (left.len(), left[0].index, left[0].range.clone()),
            (1, 0, 0..2)
        );
        b.accept(id, None, GroupId::fresh());
        assert_eq!(b.to_string(), "A B cc");
        let step = b.next_undo().expect("a step");
        assert_eq!(
            (step.origin, step.label.as_deref()),
            (Origin::Plugin("p".into()), Some("Caps"))
        );
    }

    #[test]
    fn bad_edits_are_refused() {
        let mut b = Buffer::new("abc");
        let id = ProposalId::fresh();
        assert_eq!(
            b.propose(id, vec![(2..9, "x".into())], Origin::User, None),
            Err(ProposeError::OutOfBounds(2..9))
        );
        assert_eq!(
            b.propose(
                id,
                vec![(0..2, "x".into()), (1..3, "y".into())],
                Origin::User,
                None
            ),
            Err(ProposeError::Overlapping(0..2, 1..3))
        );
        assert!(b.proposals().is_empty());
    }

    #[test]
    fn a_proposal_across_buffers_is_one_undo_group() {
        let (mut a, mut b) = (Buffer::new("x = 1"), Buffer::new("use x;"));
        let id = ProposalId::fresh();
        a.propose(
            id,
            vec![(0..1, "y".into())],
            Origin::Lsp("ra".into()),
            Some("Rename"),
        )
        .expect("valid");
        b.propose(
            id,
            vec![(4..5, "y".into())],
            Origin::Lsp("ra".into()),
            Some("Rename"),
        )
        .expect("valid");
        let (group, done) = accept_across([&mut a, &mut b], id);
        assert_eq!(done.accepted, 2);
        assert_eq!(
            (a.to_string(), b.to_string()),
            ("y = 1".into(), "use y;".into())
        );
        assert_eq!(a.next_undo().map(|s| s.group), Some(group));
        assert_eq!(b.next_undo().map(|s| s.group), Some(group));
        a.undo();
        assert_eq!(super::super::undo_across([&mut b], group, false), (1, 0));
        assert_eq!(
            (a.to_string(), b.to_string()),
            ("x = 1".into(), "use x;".into())
        );
    }
}
