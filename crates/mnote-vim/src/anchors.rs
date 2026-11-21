//! Line anchors for `:g` and `:normal`: the start of each line a command
//! still has to visit, kept correct while the command edits the note.
//!
//! Anchors stay sorted. An edit shifts every anchor after it by the same
//! amount, so the shifts live in a Fenwick tree (range-add, point-query):
//! each edit costs O(log n) plus the anchors inside the edited range, instead
//! of touching every anchor. That keeps `:g/pat/d` on a 30 000-match note
//! linear rather than quadratic.

use std::ops::Range;

use ropey::Rope;

#[derive(Debug, Default, Clone)]
pub(crate) struct Anchors {
    base: Vec<usize>,
    /// Fenwick tree of position deltas; the delta of anchor `i` is the prefix
    /// sum up to `i`.
    tree: Vec<i64>,
    gone: Vec<bool>,
}

impl Anchors {
    /// Anchors at `positions`, which must be sorted.
    pub fn new(positions: Vec<usize>) -> Self {
        let n = positions.len();
        Anchors {
            base: positions,
            tree: vec![0; n + 1],
            gone: vec![false; n],
        }
    }

    pub fn len(&self) -> usize {
        self.base.len()
    }

    pub fn is_empty(&self) -> bool {
        self.base.is_empty()
    }

    /// The current position of anchor `i`, or `None` if its line was deleted.
    pub fn get(&self, i: usize) -> Option<usize> {
        (!self.gone[i]).then(|| self.pos(i))
    }

    fn pos(&self, i: usize) -> usize {
        let mut sum = 0i64;
        let mut k = i + 1;
        while k > 0 {
            sum += self.tree[k];
            k &= k - 1;
        }
        (self.base[i] as i64 + sum).max(0) as usize
    }

    /// Adds `delta` to anchors `from..`.
    fn add_suffix(&mut self, from: usize, delta: i64) {
        let mut k = from + 1;
        while k < self.tree.len() {
            self.tree[k] += delta;
            k += k.isolate_lowest_one();
        }
    }

    /// Index of the first anchor at or after `p`.
    fn first_at_or_after(&self, p: usize) -> usize {
        let (mut lo, mut hi) = (0, self.len());
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.pos(mid) < p {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// Updates anchors for replacing `range` with `added` characters. `before`
    /// is the text before the edit. A line whose start and ending newline are
    /// both removed is gone; text inserted at a line's start stays with it.
    pub fn apply_edit(&mut self, before: &Rope, range: Range<usize>, added: usize) {
        if self.is_empty() {
            return;
        }
        let removed = range.len();
        let end = range.end.min(before.len_chars());
        let mut i = self.first_at_or_after(range.start);
        while i < self.len() {
            let p = self.pos(i);
            if p == range.start && removed == 0 {
                i += 1;
                continue;
            }
            if p >= range.end {
                break;
            }
            if p < end && before.slice(p..end).chars().any(|c| c == '\n') {
                self.gone[i] = true;
            }
            // Pin anchors inside the removed text to its start.
            let pin = range.start as i64 - p as i64;
            self.add_suffix(i, pin);
            self.add_suffix(i + 1, -pin);
            i += 1;
        }
        self.add_suffix(i, added as i64 - removed as i64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifts_pins_and_drops() {
        let text = Rope::from_str("aa\nbb\ncc\ndd\n");
        let mut a = Anchors::new(vec![0, 3, 6, 9]);
        // Delete line "bb\n": its anchor goes, later ones move up by 3.
        a.apply_edit(&text, 3..6, 0);
        assert_eq!(
            (0..4).map(|i| a.get(i)).collect::<Vec<_>>(),
            [Some(0), None, Some(3), Some(6)]
        );
        // Insert at the start of the first line: it stays.
        let text = Rope::from_str("aa\ncc\ndd\n");
        a.apply_edit(&text, 0..0, 2);
        assert_eq!(a.get(0), Some(0));
        assert_eq!(a.get(2), Some(5));
        // Delete part of a line (no newline): its anchor is pinned, not gone.
        let text = Rope::from_str("xxaa\ncc\ndd\n");
        a.apply_edit(&text, 5..6, 0);
        assert_eq!(a.get(2), Some(5));
    }
}
