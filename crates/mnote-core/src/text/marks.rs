//! Decorations on a buffer's text (like Neovim's extmarks): ranges that move
//! with every edit, undo, redo and reload, each with what to draw. The app
//! draws them; plugins (later) and built-in features add them, each in a
//! namespace it can clear at once.
//!
//! Colours are named after the theme's tokens (`"match"`, `"danger_wash"`,
//! `"ink_3"`), so decorations follow the theme.

use std::ops::Range;

use super::Bias;

/// What a decoration draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkKind {
    /// Colours the text in the range: a text colour, a background, or both.
    Highlight {
        color: Option<String>,
        background: Option<String>,
    },
    /// A background behind the whole of each line the range touches.
    LineBackground { background: String },
    /// Text drawn after the line (at its right edge with `right`), not part
    /// of the note: a blame label, a word goal, a diagnostic.
    VirtualText {
        text: String,
        color: String,
        right: bool,
    },
    /// A short sign in the gutter, before the line number.
    Sign { text: String, color: String },
    /// Text drawn inside the line at the mark's start, not part of the
    /// note: an inlay hint, later an inline suggestion. The cursor and the
    /// selection skip over it.
    Ghost { text: String, color: String },
}

/// A decoration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub id: u64,
    pub namespace: String,
    /// Characters; for line decorations, any position on the line.
    pub range: Range<usize>,
    pub kind: MarkKind,
    /// Which side of text inserted exactly at its start, and at its end,
    /// the mark stays on.
    pub start_bias: Bias,
    pub end_bias: Bias,
}

/// A buffer's decorations.
#[derive(Debug, Clone, Default)]
pub struct Marks {
    marks: Vec<Mark>,
    next_id: u64,
}

impl Marks {
    /// Adds a decoration that stays before text inserted exactly at either
    /// end (typing at its end doesn't extend it).
    pub fn add(&mut self, namespace: &str, range: Range<usize>, kind: MarkKind) -> u64 {
        self.add_with_bias(namespace, range, kind, Bias::Before, Bias::Before)
    }

    /// Adds a decoration whose ends stay before or after text inserted
    /// exactly at them: a ghost text at the cursor uses `After` for both,
    /// so it moves along as the user types.
    pub fn add_with_bias(
        &mut self,
        namespace: &str,
        range: Range<usize>,
        kind: MarkKind,
        start_bias: Bias,
        end_bias: Bias,
    ) -> u64 {
        self.next_id += 1;
        self.marks.push(Mark {
            id: self.next_id,
            namespace: namespace.to_owned(),
            range,
            kind,
            start_bias,
            end_bias,
        });
        self.next_id
    }

    pub fn remove(&mut self, id: u64) {
        self.marks.retain(|m| m.id != id);
    }

    /// Removes every decoration in `namespace`.
    pub fn clear(&mut self, namespace: &str) {
        self.marks.retain(|m| m.namespace != namespace);
    }

    /// The decorations touching characters `range` (a line, for the
    /// renderer), in the order they were added.
    pub fn touching(&self, range: Range<usize>) -> impl Iterator<Item = &Mark> {
        self.marks.iter().filter(move |m| {
            m.range.start < range.end.max(range.start + 1)
                && m.range.end.max(m.range.start + 1) > range.start
        })
    }

    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }

    /// Moves every decoration for an edit replacing `removed` (characters)
    /// with `added` characters. Text after the edit shifts; a decoration
    /// whose text went keeps its place at the edit.
    pub(crate) fn edited(&mut self, removed: Range<usize>, added: usize) {
        let shift = |pos: usize, end: bool, bias: Bias| {
            if removed.is_empty() && pos == removed.start && bias == Bias::After {
                // Text inserted exactly here goes before the mark.
                pos + added
            } else if pos <= removed.start {
                pos
            } else if pos >= removed.end {
                pos - removed.len() + added
            } else if end {
                removed.start + added
            } else {
                removed.start
            }
        };
        for m in &mut self.marks {
            let start = shift(m.range.start, false, m.start_bias);
            let end = shift(m.range.end, true, m.end_bias).max(start);
            m.range = start..end;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign() -> MarkKind {
        MarkKind::Sign {
            text: "●".into(),
            color: "danger".into(),
        }
    }

    #[test]
    fn marks_move_with_edits() {
        let mut marks = Marks::default();
        let a = marks.add("lint", 10..15, sign());
        marks.edited(0..0, 3);
        assert_eq!(marks.marks[0].range, 13..18, "text before: shifts");
        marks.edited(20..25, 0);
        assert_eq!(marks.marks[0].range, 13..18, "text after: stays");
        marks.edited(14..16, 5);
        assert_eq!(marks.marks[0].range, 13..21, "inside: grows with it");
        marks.edited(10..30, 0);
        assert_eq!(
            marks.marks[0].range,
            10..10,
            "its text gone: stays at the edit"
        );
        marks.remove(a);
        assert!(marks.is_empty());
    }

    #[test]
    fn the_bias_decides_at_an_insertion_point() {
        let mut marks = Marks::default();
        marks.add("a", 5..8, sign());
        marks.add_with_bias("b", 5..8, sign(), Bias::After, Bias::After);
        let ghost = MarkKind::Ghost {
            text: ": i32".into(),
            color: "ink_3".into(),
        };
        marks.add_with_bias("c", 8..8, ghost, Bias::After, Bias::After);
        // Typing at the start of both ranges.
        marks.edited(5..5, 2);
        assert_eq!(
            marks.marks[0].range,
            5..10,
            "before: the new text is inside"
        );
        assert_eq!(marks.marks[1].range, 7..10, "after: the mark moves past it");
        // Typing at their ends, where the ghost text sits.
        marks.edited(10..10, 1);
        assert_eq!(marks.marks[0].range, 5..10, "before: not extended");
        assert_eq!(marks.marks[1].range, 7..11, "after: extended");
        assert_eq!(
            marks.marks[2].range,
            11..11,
            "the ghost stays after what's typed"
        );
        // A replacement (not an insertion) isn't decided by the bias.
        marks.edited(4..6, 2);
        assert_eq!(marks.marks[0].range, 4..10);
    }

    #[test]
    fn namespaces_clear_together_and_lines_find_theirs() {
        let mut marks = Marks::default();
        marks.add("lint", 0..2, sign());
        marks.add("lint", 10..12, sign());
        marks.add("blame", 10..10, sign());
        assert_eq!(
            marks.touching(9..14).count(),
            2,
            "an empty mark on the line counts"
        );
        assert_eq!(marks.touching(3..9).count(), 0);
        marks.clear("lint");
        assert_eq!(marks.touching(0..20).count(), 1);
    }
}
