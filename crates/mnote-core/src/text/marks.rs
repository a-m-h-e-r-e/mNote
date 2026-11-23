//! Decorations on a buffer's text (like Neovim's extmarks): ranges that move
//! with every edit, undo, redo and reload, each with what to draw. The app
//! draws them; plugins (later) and built-in features add them, each in a
//! namespace it can clear at once.
//!
//! Colours are named after the theme's tokens (`"match"`, `"danger_wash"`,
//! `"ink_3"`), so decorations follow the theme.

use std::ops::Range;

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
}

/// A decoration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub id: u64,
    pub namespace: String,
    /// Characters; for line decorations, any position on the line.
    pub range: Range<usize>,
    pub kind: MarkKind,
}

/// A buffer's decorations.
#[derive(Debug, Clone, Default)]
pub struct Marks {
    marks: Vec<Mark>,
    next_id: u64,
}

impl Marks {
    pub fn add(&mut self, namespace: &str, range: Range<usize>, kind: MarkKind) -> u64 {
        self.next_id += 1;
        self.marks.push(Mark {
            id: self.next_id,
            namespace: namespace.to_owned(),
            range,
            kind,
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
        let shift = |pos: usize, end: bool| {
            if pos <= removed.start {
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
            let start = shift(m.range.start, false);
            let end = shift(m.range.end, true).max(start);
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
