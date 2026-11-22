//! [`Buffer`]: the text of one open note.
//!
//! A rope (`ropey`) holds the text, so edits and line lookups stay fast on
//! notes of any size. Every change goes through [`Buffer::edit`], which keeps
//! the undo tree and the Markdown syntax tree in step. Positions are character
//! offsets, as Vim motions need; byte offsets are an internal detail.

mod autosave;
mod history;
mod syntax;

use std::fmt;
use std::ops::Range;

pub use autosave::Autosave;
pub use history::Change;
use history::{History, Step};
use ropey::Rope;
use syntax::{Syntax, point_at};
use tree_sitter::{InputEdit, Tree};

use crate::{NotePath, Vault, VaultError};

/// An edit range that doesn't fit inside the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfBounds {
    pub start: usize,
    pub end: usize,
    pub len: usize,
}

impl fmt::Display for OutOfBounds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "edit range {}..{} is outside a buffer of {} characters",
            self.start, self.end, self.len
        )
    }
}

impl std::error::Error for OutOfBounds {}

/// What [`Buffer::reload`] did with a note that changed on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reload {
    /// The disk text matched the buffer; nothing to do.
    Unchanged,
    /// The buffer had no unsaved edits, so it now shows the disk text. The
    /// reload is one undo step, so `u` brings the old text back.
    Reloaded,
    /// The buffer has unsaved edits, so they were kept. Ask before replacing.
    KeptLocalEdits,
}

/// The text of one open note, with its undo tree and syntax tree.
#[derive(Debug)]
pub struct Buffer {
    rope: Rope,
    history: History,
    /// Changes of the Vim command in progress, committed as one undo step.
    pending: Option<Step>,
    /// How many `begin_change` calls are open. Groups nest: only the
    /// outermost `end_change` commits, so an ex command that runs Vim
    /// commands (`:g`, `:normal`) is still one undo step.
    depth: usize,
    syntax: Option<Syntax>,
    revision: u64,
    /// Lines touched since the last [`take_changed_lines`](Self::take_changed_lines).
    changed: Option<ChangedLines>,
}

/// Which lines changed since a view last looked, as the number of unchanged
/// lines at the top and at the bottom of the note. Everything between them
/// may have changed. Line counts are `Rope::len_lines` (a final newline is
/// followed by one empty line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangedLines {
    pub unchanged_top: usize,
    pub unchanged_bottom: usize,
}

impl Buffer {
    /// A buffer holding `text`, parsed as Markdown.
    pub fn new(text: &str) -> Self {
        Buffer {
            rope: Rope::from_str(text),
            history: History::new(),
            pending: None,
            depth: 0,
            syntax: Syntax::markdown(),
            revision: 0,
            changed: None,
        }
    }

    /// Opens a note from the vault.
    pub fn open(vault: &Vault, note: &NotePath) -> Result<Self, VaultError> {
        Ok(Self::new(&vault.read(note)?))
    }

    /// The text. Cheap to clone if a snapshot is needed.
    pub fn text(&self) -> &Rope {
        &self.rope
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// Goes up by one on every change, including undo and redo. Views use it
    /// to know when to redraw cached lines.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether the text differs from what was last saved or opened. Undoing
    /// back to the saved state makes the buffer clean again.
    pub fn is_dirty(&self) -> bool {
        self.pending.as_ref().is_some_and(|s| !s.forward.is_empty()) || !self.history.is_saved()
    }

    /// Starts one undo step that may span many edits, such as an insert-mode
    /// session or a `:%s` substitution. `cursor` is restored on undo. Calls
    /// nest; the outermost [`end_change`](Self::end_change) commits.
    pub fn begin_change(&mut self, cursor: usize) {
        if self.depth == 0 {
            self.pending = Some(Step {
                cursor_before: cursor,
                cursor_after: cursor,
                ..Step::default()
            });
        }
        self.depth += 1;
    }

    /// Ends the current undo step. `cursor` is restored on redo.
    pub fn end_change(&mut self, cursor: usize) {
        if self.depth == 0 {
            return;
        }
        self.depth -= 1;
        if self.depth == 0 {
            self.commit_pending(cursor);
        }
    }

    /// Closes any open undo step at once, however deeply nested.
    fn close_change(&mut self, cursor: usize) {
        self.depth = 0;
        self.commit_pending(cursor);
    }

    fn commit_pending(&mut self, cursor: usize) {
        if let Some(mut step) = self.pending.take()
            && !step.forward.is_empty()
        {
            step.cursor_after = cursor;
            self.history.commit(step);
        }
    }

    /// Replaces the characters in `range` with `text`. Outside
    /// [`begin_change`](Self::begin_change) / [`end_change`](Self::end_change),
    /// each edit is its own undo step and `cursor` is restored on undo.
    pub fn edit(
        &mut self,
        range: Range<usize>,
        text: &str,
        cursor: usize,
    ) -> Result<(), OutOfBounds> {
        let len = self.rope.len_chars();
        if range.start > range.end || range.end > len {
            return Err(OutOfBounds {
                start: range.start,
                end: range.end,
                len,
            });
        }
        if range.is_empty() && text.is_empty() {
            return Ok(());
        }
        let forward = Change {
            start: range.start,
            end: range.end,
            text: text.to_owned(),
        };
        let removed = self.apply(&forward);
        let inverse = Change {
            start: range.start,
            end: range.start + text.chars().count(),
            text: removed,
        };
        match &mut self.pending {
            Some(step) => {
                step.forward.push(forward);
                step.inverse.push(inverse);
            }
            None => self.history.commit(Step {
                cursor_after: inverse.end,
                forward: vec![forward],
                inverse: vec![inverse],
                cursor_before: cursor,
            }),
        }
        Ok(())
    }

    /// The lines changed since the last call, or `None` if nothing changed.
    /// A view uses it to re-measure only those lines.
    pub fn take_changed_lines(&mut self) -> Option<ChangedLines> {
        self.changed.take()
    }

    /// Undoes the last step. Returns where the cursor should go, or `None`
    /// when there's nothing left to undo.
    pub fn undo(&mut self) -> Option<usize> {
        self.close_change(0);
        let step = self.history.undo()?.clone();
        for change in step.inverse.iter().rev() {
            self.apply(change);
        }
        Some(step.cursor_before)
    }

    /// Redoes the most recently undone step on the current branch.
    pub fn redo(&mut self) -> Option<usize> {
        self.close_change(0);
        let step = self.history.redo()?.clone();
        for change in &step.forward {
            self.apply(change);
        }
        Some(step.cursor_after)
    }

    /// Number of undo steps recorded, branches included.
    pub fn undo_steps(&self) -> usize {
        self.history.len()
    }

    /// Marks the current text as saved. A step in progress is split there
    /// (the part before the save is one undo step) and then continues, so an
    /// autosave in the middle of typing keeps grouping what follows.
    pub fn mark_saved(&mut self) {
        if let Some(cursor) = self.pending.as_ref().map(|s| s.cursor_after) {
            // Commit what's done so far and keep the group open, at the same
            // nesting depth, for what follows.
            self.commit_pending(cursor);
            self.pending = Some(Step {
                cursor_before: cursor,
                cursor_after: cursor,
                ..Step::default()
            });
        }
        self.history.mark_saved();
    }

    /// Writes the buffer to its note atomically and marks it saved.
    pub fn save(&mut self, vault: &Vault, note: &NotePath) -> Result<(), VaultError> {
        vault.write(note, &self.rope.to_string())?;
        self.mark_saved();
        Ok(())
    }

    /// Takes in the note's text after another app changed it on disk. Only
    /// the differing middle part is replaced, so the cursor and syntax tree
    /// outside it are untouched.
    pub fn reload(&mut self, disk: &str) -> Reload {
        let (prefix, suffix) = common_ends(&self.rope, disk);
        let len = self.rope.len_chars();
        let disk_chars = disk.chars().count();
        if prefix == len && prefix == disk_chars {
            self.mark_saved();
            return Reload::Unchanged;
        }
        if self.is_dirty() {
            return Reload::KeptLocalEdits;
        }
        let start_byte = disk
            .char_indices()
            .nth(prefix)
            .map_or(disk.len(), |(b, _)| b);
        let end_byte = disk
            .char_indices()
            .nth(disk_chars - suffix)
            .map_or(disk.len(), |(b, _)| b);
        // The range is computed from the buffer itself, so it always fits.
        let _ = self.edit(prefix..len - suffix, &disk[start_byte..end_byte], prefix);
        self.mark_saved();
        Reload::Reloaded
    }

    /// The newest Markdown syntax tree, without waiting. Parsing runs on a
    /// background thread; this returns at once with a tree that always lines
    /// up with the current text, though its structure may lag a few edits
    /// behind on very large notes. `None` until the first parse finishes, or
    /// if the grammar couldn't load.
    pub fn syntax_tree(&mut self) -> Option<&Tree> {
        let rope = &self.rope;
        self.syntax.as_mut()?.tree(rope)
    }

    /// The syntax tree for the current text, waiting for the parse if needed.
    /// For tests, tools and the first paint of a small note; never call it on
    /// the keystroke path.
    pub fn syntax_tree_now(&mut self) -> Option<&Tree> {
        let rope = &self.rope;
        self.syntax.as_mut()?.tree_now(rope)
    }

    /// Whether the syntax tree's structure reflects the current text.
    pub fn syntax_is_current(&self) -> bool {
        self.syntax.as_ref().is_some_and(|s| s.is_current())
    }

    /// Calls `wake` (on the parser thread) whenever a fresh syntax tree is
    /// ready, so the app can schedule a redraw.
    pub fn on_syntax_ready(&mut self, wake: impl Fn() + Send + 'static) {
        if let Some(syntax) = &mut self.syntax {
            syntax.set_waker(Box::new(wake));
        }
    }

    /// Applies one change to the rope and the syntax tree. Returns the text it
    /// replaced. The range must already be validated.
    fn apply(&mut self, change: &Change) -> String {
        let rope = &mut self.rope;
        let start_byte = rope.char_to_byte(change.start);
        let old_end_byte = rope.char_to_byte(change.end);
        let start_position = point_at(rope, start_byte);
        let old_end_position = point_at(rope, old_end_byte);

        let removed = rope.slice(change.start..change.end).to_string();
        rope.remove(change.start..change.end);
        rope.insert(change.start, &change.text);

        let new_end_byte = start_byte + change.text.len();
        let top = start_position.row;
        let bottom = rope.len_lines() - 1 - rope.byte_to_line(new_end_byte);
        self.changed = Some(match self.changed {
            Some(c) => ChangedLines {
                unchanged_top: c.unchanged_top.min(top),
                unchanged_bottom: c.unchanged_bottom.min(bottom),
            },
            None => ChangedLines {
                unchanged_top: top,
                unchanged_bottom: bottom,
            },
        });
        if let Some(syntax) = &mut self.syntax {
            syntax.edit(&InputEdit {
                start_byte,
                old_end_byte,
                new_end_byte,
                start_position,
                old_end_position,
                new_end_position: point_at(rope, new_end_byte),
            });
        }
        self.revision += 1;
        removed
    }
}

impl fmt::Display for Buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for chunk in self.rope.chunks() {
            f.write_str(chunk)?;
        }
        Ok(())
    }
}

/// Lengths in characters of the common prefix and common suffix of `rope` and
/// `text`, never overlapping.
fn common_ends(rope: &Rope, text: &str) -> (usize, usize) {
    let prefix = rope
        .chars()
        .zip(text.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let max_suffix = rope.len_chars().min(text.chars().count()) - prefix;
    let suffix = rope
        .chars_at(rope.len_chars())
        .reversed()
        .zip(text.chars().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    (prefix, suffix)
}

/// How many words `text` holds: runs of letters and digits (with `'`, `-`
/// and `_` inside them), so Markdown markers like `##` or `-` don't count.
/// Linear in the text; call it off the UI thread on big notes.
pub fn count_words(text: &Rope) -> usize {
    let mut words = 0;
    let mut in_word = false;
    for c in text.chars() {
        if c.is_alphanumeric() {
            if !in_word {
                words += 1;
                in_word = true;
            }
        } else if !(in_word && matches!(c, '\'' | '’' | '-' | '_')) {
            in_word = false;
        }
    }
    words
}

#[cfg(test)]
mod word_tests {
    use super::*;

    #[test]
    fn words_skip_markers_and_keep_joined_words_whole() {
        let count = |s: &str| count_words(&Rope::from_str(s));
        assert_eq!(count(""), 0);
        assert_eq!(count("## Plan\n- don't over-think it_now\n"), 4);
        assert_eq!(count("[[ui-design]] 2026-11-22 · ok"), 3);
        assert_eq!(count("日本 語"), 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_sexp(text: &str) -> String {
        Buffer::new(text)
            .syntax_tree_now()
            .expect("markdown grammar")
            .root_node()
            .to_sexp()
    }

    #[test]
    fn edits_replace_by_character_offset() {
        let mut b = Buffer::new("héllo wörld");
        b.edit(6..11, "there", 0).expect("in range");
        assert_eq!(b.to_string(), "héllo there");
        b.edit(0..0, "🙂 ", 0).expect("in range");
        assert_eq!(b.to_string(), "🙂 héllo there");
        assert_eq!(b.len_chars(), 13);
    }

    #[test]
    fn out_of_range_edits_are_errors_not_panics() {
        let mut b = Buffer::new("abc");
        assert_eq!(
            b.edit(2..9, "x", 0),
            Err(OutOfBounds {
                start: 2,
                end: 9,
                len: 3
            })
        );
        let reversed = Range { start: 2, end: 1 };
        assert!(b.edit(reversed, "x", 0).is_err());
        assert_eq!(b.to_string(), "abc");
        assert!(!b.is_dirty());
    }

    #[test]
    fn undo_and_redo_restore_text_and_cursor() {
        let mut b = Buffer::new("one");
        b.edit(3..3, " two", 3).expect("edit");
        b.edit(0..3, "ONE", 0).expect("edit");
        assert_eq!(b.undo(), Some(0));
        assert_eq!(b.to_string(), "one two");
        assert_eq!(b.undo(), Some(3));
        assert_eq!(b.to_string(), "one");
        assert_eq!(b.undo(), None);
        assert_eq!(b.redo(), Some(7));
        assert_eq!(b.to_string(), "one two");
    }

    #[test]
    fn a_grouped_change_is_one_undo_step() {
        let mut b = Buffer::new("note");
        b.begin_change(4);
        for (i, c) in " text".chars().enumerate() {
            b.edit(4 + i..4 + i, &c.to_string(), 4 + i).expect("edit");
        }
        b.end_change(9);
        assert_eq!(b.to_string(), "note text");
        assert_eq!(b.undo_steps(), 1);
        assert_eq!(b.undo(), Some(4));
        assert_eq!(b.to_string(), "note");
        assert_eq!(b.redo(), Some(9));
        assert_eq!(b.to_string(), "note text");
    }

    #[test]
    fn editing_after_undo_starts_a_branch_and_keeps_the_old_one() {
        let mut b = Buffer::new("a");
        b.edit(1..1, "b", 1).expect("edit");
        b.undo();
        b.edit(1..1, "c", 1).expect("edit");
        assert_eq!(b.to_string(), "ac");
        assert_eq!(b.undo_steps(), 2, "the undone branch is kept in the tree");
        b.undo();
        b.redo();
        assert_eq!(b.to_string(), "ac", "redo follows the newest branch");
    }

    #[test]
    fn dirty_tracks_the_saved_state_through_undo() {
        let mut b = Buffer::new("x");
        assert!(!b.is_dirty());
        b.edit(1..1, "y", 1).expect("edit");
        assert!(b.is_dirty());
        b.undo();
        assert!(!b.is_dirty(), "undoing back to the saved text is clean");
        b.redo();
        b.mark_saved();
        assert!(!b.is_dirty());
        b.begin_change(2);
        b.edit(2..2, "z", 2).expect("edit");
        assert!(b.is_dirty(), "a change in progress is dirty");
        b.mark_saved();
        assert!(!b.is_dirty());
    }

    #[test]
    fn syntax_tree_follows_edits_incrementally() {
        let mut b = Buffer::new("# Title\n\nSome text.\n");
        let before = b.syntax_tree_now().expect("tree").root_node().to_sexp();
        assert!(before.contains("atx_heading"));
        b.edit(9..9, "- item\n- 🙂 two\n\n", 9).expect("edit");
        b.edit(0..1, "##", 0).expect("edit");
        let after = b.syntax_tree_now().expect("tree").root_node().to_sexp();
        assert!(after.contains("list"));
        assert_eq!(after, fresh_sexp(&b.to_string()));
        b.undo();
        b.undo();
        assert_eq!(
            b.syntax_tree_now().expect("tree").root_node().to_sexp(),
            before
        );
    }

    #[test]
    fn the_non_blocking_tree_always_lines_up_with_the_text() {
        let mut b = Buffer::new("# Title\n\nSome text.\n");
        b.syntax_tree_now().expect("tree");
        b.edit(0..0, "Intro line\n\n", 0).expect("edit");
        // Without waiting, the structure may be old, but positions are shifted
        // so the heading is still found where it now is.
        let tree = b.syntax_tree().expect("tree");
        let heading = tree
            .root_node()
            .named_descendant_for_byte_range(12, 13)
            .expect("node at the heading");
        assert!(heading.kind().starts_with("atx"), "got {}", heading.kind());
    }

    #[test]
    fn reload_takes_disk_text_only_when_clean() {
        let mut b = Buffer::new("# Note\n\nfirst line\nlast line\n");
        assert_eq!(
            b.reload("# Note\n\nfirst line\nlast line\n"),
            Reload::Unchanged
        );
        assert_eq!(
            b.reload("# Note\n\nfirst line, edited elsewhere\nlast line\n"),
            Reload::Reloaded
        );
        assert_eq!(
            b.to_string(),
            "# Note\n\nfirst line, edited elsewhere\nlast line\n"
        );
        assert!(!b.is_dirty());
        assert_eq!(b.undo(), Some(18), "the reload is one undo step");
        assert_eq!(b.to_string(), "# Note\n\nfirst line\nlast line\n");

        let mut dirty = Buffer::new("mine");
        dirty.edit(4..4, "!", 4).expect("edit");
        assert_eq!(dirty.reload("theirs"), Reload::KeptLocalEdits);
        assert_eq!(dirty.to_string(), "mine!");
    }

    #[test]
    fn nested_changes_are_one_undo_step() {
        let mut b = Buffer::new("");
        b.begin_change(0);
        b.edit(0..0, "a", 0).expect("edit");
        b.begin_change(1);
        b.edit(1..1, "b", 1).expect("edit");
        b.end_change(2);
        b.edit(2..2, "c", 2).expect("edit");
        b.end_change(3);
        assert_eq!(b.undo_steps(), 1);
        b.undo();
        assert_eq!(b.to_string(), "");
        b.end_change(0); // unbalanced extra end is harmless
        b.edit(0..0, "x", 0).expect("edit");
        assert_eq!(
            b.undo_steps(),
            2,
            "edits after a balanced group stand alone"
        );
    }

    #[test]
    fn saving_mid_change_keeps_grouping_the_rest() {
        let mut b = Buffer::new("");
        b.begin_change(0);
        b.edit(0..0, "ab", 0).expect("edit");
        b.mark_saved();
        assert!(!b.is_dirty());
        b.edit(2..2, "c", 2).expect("edit");
        b.edit(3..3, "d", 3).expect("edit");
        assert!(b.is_dirty());
        b.end_change(4);
        assert_eq!(
            b.undo_steps(),
            2,
            "before and after the save, not one per key"
        );
        b.undo();
        assert_eq!(b.to_string(), "ab");
        assert!(!b.is_dirty());
    }

    #[test]
    fn reports_the_lines_that_changed() {
        let mut b = Buffer::new("a\nb\nc\nd\ne\n");
        assert_eq!(b.take_changed_lines(), None);
        b.edit(2..3, "B\nB2", 0).expect("edit"); // line 1 becomes two lines
        b.edit(9..9, "!", 0).expect("edit"); // line 4: "d" becomes "!d"
        assert_eq!(
            b.take_changed_lines(),
            Some(ChangedLines {
                unchanged_top: 1,
                unchanged_bottom: 2
            })
        );
        assert_eq!(b.take_changed_lines(), None, "taking clears it");
        b.undo();
        assert!(b.take_changed_lines().is_some(), "undo counts as a change");
    }

    #[test]
    fn common_ends_never_overlap() {
        let rope = Rope::from_str("aaa");
        assert_eq!(common_ends(&rope, "aa"), (2, 0));
        assert_eq!(common_ends(&Rope::from_str("abcxyz"), "abXyz"), (2, 2));
    }

    #[test]
    fn saves_through_the_vault() {
        let dir = crate::vault::tests::TempDir::new();
        let vault = Vault::open(&dir.0).expect("open");
        let note = NotePath::new("inbox.md").expect("valid");
        let mut b = Buffer::new("");
        b.edit(0..0, "# Inbox\n", 0).expect("edit");
        b.save(&vault, &note).expect("save");
        assert!(!b.is_dirty());
        assert_eq!(
            Buffer::open(&vault, &note).expect("open").to_string(),
            "# Inbox\n"
        );
    }
}

#[cfg(test)]
mod budget {
    use super::*;
    use std::time::{Duration, Instant};

    fn big_note() -> String {
        let block = "## Section\n\nSome **bold** text with a [[wikilink]] and `code`.\n\n- item one\n- item two\n\n```rust\nlet x = 1;\n```\n\n";
        block.repeat(1_048_576 / block.len() + 1)
    }

    /// Budgets from CLAUDE.md: a 1 MB note opens in under 50 ms (text first,
    /// syntax arrives in the background), and a keystroke (edit + taking the
    /// newest tree) stays far under the 8 ms frame budget while parses run.
    /// Run with `cargo test -p mnote-core --release -- --ignored`.
    #[test]
    #[ignore = "timing test; run with --release -- --ignored"]
    fn opens_and_edits_a_one_megabyte_note_within_budget() {
        let text = big_note();
        let start = Instant::now();
        let mut b = Buffer::new(&text);
        let _ = b.syntax_tree();
        let open = start.elapsed();

        let mut worst = Duration::ZERO;
        let middle = b.len_chars() / 2;
        for i in 0..200 {
            let t = Instant::now();
            b.edit(middle + i..middle + i, "x", middle + i)
                .expect("edit");
            let _ = b.syntax_tree();
            worst = worst.max(t.elapsed());
        }

        let t = Instant::now();
        b.syntax_tree_now().expect("tree");
        let settle = t.elapsed();
        assert!(b.syntax_is_current());
        eprintln!(
            "1 MB note: open {open:?}, worst keystroke {worst:?}, background parse settled in {settle:?}"
        );
        assert!(open < Duration::from_millis(50), "open took {open:?}");
        assert!(worst < Duration::from_millis(1), "keystroke took {worst:?}");
    }
}
