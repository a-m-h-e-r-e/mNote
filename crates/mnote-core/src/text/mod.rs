//! [`Buffer`]: the text of one open note.
//!
//! A rope (`ropey`) holds the text, so edits and line lookups stay fast on
//! notes of any size. Every change goes through [`Buffer::edit`], which keeps
//! the undo tree and the Markdown syntax tree in step. Positions are character
//! offsets, as Vim motions need; byte offsets are an internal detail.

mod autosave;
mod changes;
mod groups;
mod history;
mod marks;
mod proposals;

use std::fmt;
use std::ops::Range;

pub use autosave::Autosave;
use changes::ChangeLog;
pub use changes::{Bias, Change, ConsumerId, GroupId, Origin};
pub use groups::{GroupDocs, undo_across};
use history::{History, Replace, Step};
pub use marks::{Mark, MarkKind, Marks};
use mnote_lang::coords::{ByteOffset, CharOffset, RopeCoords};
use mnote_lang::syntax::Syntax;
pub use mnote_lang::syntax::drop_tree_later;
pub use proposals::{Hunk, HunkState, ProposalId, ProposeError, Resolved, accept_across};
use ropey::Rope;
use std::sync::Arc;
use tree_sitter::Tree;

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
    /// Decorations, moved by every edit.
    marks: Marks,
    /// Every edit, with who made it, for those following the text.
    log: ChangeLog,
    /// The undo step edits are going into now.
    group: GroupId,
    /// The group the last undo or redo reversed or re-applied, for the app
    /// to carry to the other buffers it spans.
    undone: Option<Undone>,
    /// Its language's name (`markdown`), if it has one.
    language: Option<Arc<str>>,
    /// Edits held for review, as anchored hunks.
    proposals: Vec<proposals::Held>,
}

/// What the last undo or redo did, from [`Buffer::take_undone`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undone {
    pub group: GroupId,
    /// Redo, not undo.
    pub redo: bool,
    pub label: Option<Arc<str>>,
}

/// The undo step `u` (or `⌃r`) would take next, from
/// [`Buffer::next_undo`] and [`Buffer::next_redo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepInfo {
    pub group: GroupId,
    pub origin: Origin,
    pub label: Option<Arc<str>>,
}

/// A buffer's text at one revision: cheap to take (a rope clone is O(1)) and
/// safe to read on another thread while the buffer moves on.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub rope: Rope,
    pub revision: u64,
    pub language: Option<Arc<str>>,
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
        Self::with_syntax(text, Syntax::markdown(), Some("markdown"))
    }

    /// A buffer holding `text`, parsed as `language` (a name or alias in
    /// [`crate::code`]'s registry); without that language, as plain text.
    pub fn with_language(text: &str, language: &str) -> Self {
        if language == "markdown" {
            return Self::new(text);
        }
        let syntax = crate::code::language(language)
            .and_then(|l| Syntax::with_injections(&l, Some(language)));
        let name = syntax.is_some().then_some(language);
        Self::with_syntax(text, syntax, name)
    }

    fn with_syntax(text: &str, syntax: Option<Syntax>, language: Option<&str>) -> Self {
        Buffer {
            rope: Rope::from_str(text),
            history: History::new(),
            pending: None,
            depth: 0,
            syntax,
            revision: 0,
            changed: None,
            marks: Marks::default(),
            log: ChangeLog::new(0),
            group: GroupId::fresh(),
            undone: None,
            language: language.map(Arc::from),
            proposals: Vec::new(),
        }
    }

    /// The buffer's decorations.
    pub fn marks(&self) -> &Marks {
        &self.marks
    }

    /// Changes the decorations (add, remove, clear a namespace).
    pub fn marks_mut(&mut self) -> &mut Marks {
        &mut self.marks
    }

    /// Opens a note from the vault.
    pub fn open(vault: &Vault, note: &NotePath) -> Result<Self, VaultError> {
        let text = vault.read(note)?;
        let first_line = text.lines().next();
        let language = crate::filetype::detect(note.as_path(), first_line).and_then(|f| f.language);
        Ok(match language {
            Some(language) => Self::with_language(&text, &language),
            None => Self::with_syntax(&text, None, None),
        })
    }

    /// The text and its revision, for work on another thread.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            rope: self.rope.clone(),
            revision: self.revision,
            language: self.language.clone(),
        }
    }

    /// The language's name (`markdown`, `rust`), if the buffer has one.
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// Starts following edits from now on (see [`Self::changes_since`]).
    pub fn subscribe_changes(&mut self) -> ConsumerId {
        self.log.subscribe()
    }

    /// Stops following edits.
    pub fn unsubscribe_changes(&mut self, id: ConsumerId) {
        self.log.unsubscribe(id);
    }

    /// `id` has seen the edits up to `revision`; they can be dropped once
    /// every follower has.
    pub fn ack_changes(&mut self, id: ConsumerId, revision: u64) {
        self.log.ack(id, revision);
    }

    /// The edits after `revision`, oldest first; `None` if some were
    /// dropped (a follower that fell that far behind starts again from the
    /// text).
    pub fn changes_since(&self, revision: u64) -> Option<impl Iterator<Item = &Change>> {
        self.log.since(revision)
    }

    /// Moves a position found at `revision` onto the current text.
    pub fn rebase(&self, at: CharOffset, revision: u64, bias: Bias) -> Option<CharOffset> {
        self.log.rebase(at, revision, bias)
    }

    /// The user's own most recent edits, oldest first.
    pub fn recent_user_edits(&self) -> impl Iterator<Item = &Change> {
        self.log.recent_user_edits()
    }

    /// Who made the latest edit.
    pub fn last_origin(&self) -> Option<&Origin> {
        self.log.last_origin()
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
        self.begin_group(cursor, GroupId::fresh(), None);
    }

    /// Like [`begin_change`](Self::begin_change), with the step joining
    /// edit group `group` (shared by every buffer one multi-file edit
    /// touches) and carrying a label. Nested, the outer step's group stands.
    pub fn begin_group(&mut self, cursor: usize, group: GroupId, label: Option<&str>) {
        if self.depth == 0 {
            self.group = group;
            self.pending = Some(Step {
                cursor_before: cursor,
                cursor_after: cursor,
                group,
                label: label.map(Arc::from),
                ..Step::default()
            });
        }
        self.depth += 1;
    }

    /// The step undo would reverse next.
    pub fn next_undo(&self) -> Option<StepInfo> {
        self.history.peek_undo().map(Step::info)
    }

    /// The step redo would re-apply next.
    pub fn next_redo(&self) -> Option<StepInfo> {
        self.history.peek_redo().map(Step::info)
    }

    /// What the last undo or redo did, once; the app undoes the same group
    /// in the other buffers it spans.
    pub fn take_undone(&mut self) -> Option<Undone> {
        self.undone.take()
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
    /// `origin` says who made it (recorded in the change log).
    pub fn edit(
        &mut self,
        range: Range<usize>,
        text: &str,
        cursor: usize,
        origin: Origin,
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
        if self.pending.is_none() {
            self.group = GroupId::fresh();
        }
        let forward = Replace {
            start: range.start,
            end: range.end,
            text: text.to_owned(),
        };
        let removed = self.apply(&forward, &origin);
        let inverse = Replace {
            start: range.start,
            end: range.start + text.chars().count(),
            text: removed,
        };
        match &mut self.pending {
            Some(step) => {
                if step.forward.is_empty() {
                    step.origin = origin;
                }
                step.forward.push(forward);
                step.inverse.push(inverse);
            }
            None => self.history.commit(Step {
                cursor_after: inverse.end,
                forward: vec![forward],
                inverse: vec![inverse],
                cursor_before: cursor,
                group: self.group,
                origin,
                label: None,
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
        self.group = GroupId::fresh();
        for change in step.inverse.iter().rev() {
            self.apply(change, &Origin::User);
        }
        self.undone = Some(Undone {
            group: step.group,
            redo: false,
            label: step.label.clone(),
        });
        Some(step.cursor_before)
    }

    /// Redoes the most recently undone step on the current branch.
    pub fn redo(&mut self) -> Option<usize> {
        self.close_change(0);
        let step = self.history.redo()?.clone();
        self.group = GroupId::fresh();
        for change in &step.forward {
            self.apply(change, &Origin::User);
        }
        self.undone = Some(Undone {
            group: step.group,
            redo: true,
            label: step.label.clone(),
        });
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
            let (group, label) = self
                .pending
                .as_ref()
                .map(|s| (s.group, s.label.clone()))
                .unwrap_or_default();
            self.commit_pending(cursor);
            self.pending = Some(Step {
                cursor_before: cursor,
                cursor_after: cursor,
                group,
                label,
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
        let _ = self.edit(
            prefix..len - suffix,
            &disk[start_byte..end_byte],
            prefix,
            Origin::External,
        );
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

    /// The syntax map's injection layers (code fences, frontmatter, and
    /// whatever a language's `injections` query finds), without waiting.
    pub fn syntax_layers(&mut self) -> &[mnote_lang::syntax::Layer] {
        let rope = &self.rope;
        match self.syntax.as_mut() {
            Some(syntax) => syntax.layers(rope),
            None => &[],
        }
    }

    /// The deepest layer of `language` covering byte `at`.
    pub fn layer_at(&mut self, at: usize, language: &str) -> Option<&mnote_lang::syntax::Layer> {
        let rope = &self.rope;
        self.syntax.as_mut()?.layer_at(rope, at, language)
    }

    /// The layers for the current text, waiting for the parse (tests and
    /// tools; never on the keystroke path).
    pub fn syntax_layers_now(&mut self) -> &[mnote_lang::syntax::Layer] {
        let rope = &self.rope;
        match self.syntax.as_mut() {
            Some(syntax) => syntax.layers_now(rope),
            None => &[],
        }
    }

    /// Parses again though the text didn't change (a grammar an injection
    /// needs has just loaded).
    pub fn reparse_syntax(&mut self) {
        if let Some(syntax) = &mut self.syntax {
            syntax.reparse();
        }
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

    /// Applies one replacement to the rope, records it in the change log,
    /// and passes it on to the marks and the syntax tree. Returns the text it
    /// replaced. The range must already be validated.
    fn apply(&mut self, change: &Replace, origin: &Origin) -> String {
        let rope = &mut self.rope;
        let (start, end) = (CharOffset(change.start), CharOffset(change.end));
        let start_point = rope.text_point(start);
        let old_end_point = rope.text_point(end);
        let old_bytes = rope.char_to_byte_offset(start)..rope.char_to_byte_offset(end);

        let removed = rope.slice(change.start..change.end).to_string();
        rope.remove(change.start..change.end);
        rope.insert(change.start, &change.text);
        let new_chars = change.text.chars().count();
        self.marks.edited(change.start..change.end, new_chars);

        self.revision += 1;
        let record = Change {
            range: start..end,
            new_end: rope.text_point(CharOffset(change.start + new_chars)),
            old_bytes,
            new_text: Arc::from(change.text.as_str()),
            new_chars,
            start: start_point,
            old_end: old_end_point,
            revision: self.revision,
            origin: origin.clone(),
            group: self.group,
        };

        let new_end_byte = ByteOffset(record.old_bytes.start.0 + change.text.len());
        let top = start_point.row;
        let bottom = rope.len_lines() - 1 - rope.byte_to_line(new_end_byte.0);
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
        // The syntax tree is edited from the recorded change, so it sees
        // exactly what every other follower sees.
        if let Some(syntax) = &mut self.syntax {
            syntax.edit(&record.input_edit());
        }
        self.log.push(record);
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

    #[test]
    fn every_edit_is_recorded_with_its_origin() {
        let mut b = Buffer::new("héllo\n");
        let id = b.subscribe_changes();
        b.edit(1..2, "e🙂", 1, Origin::User).expect("edit");
        b.edit(0..0, "# ", 0, Origin::Plugin("tasks".into()))
            .expect("edit");
        let changes: Vec<&Change> = b.changes_since(0).expect("kept").collect();
        assert_eq!(changes.len(), 2);
        let first = changes[0];
        assert_eq!(first.range, CharOffset(1)..CharOffset(2));
        // "é" is 2 bytes at byte 1; the new text is "e🙂", 5 bytes.
        assert_eq!(first.old_bytes, ByteOffset(1)..ByteOffset(3));
        assert_eq!((&*first.new_text, first.new_chars), ("e🙂", 2));
        assert_eq!(
            (first.old_end.column_bytes, first.old_end.column_utf16),
            (3, 2)
        );
        assert_eq!(
            (first.new_end.column_bytes, first.new_end.column_utf16),
            (6, 4)
        );
        assert_eq!(first.input_edit().new_end_byte, 6);
        assert_eq!(first.revision, 1);
        assert_eq!(changes[1].origin, Origin::Plugin("tasks".into()));
        assert_eq!(b.last_origin(), Some(&Origin::Plugin("tasks".into())));
        b.ack_changes(id, 2);
        assert_eq!(b.changes_since(2).map(Iterator::count), Some(0));
        let user: Vec<_> = b.recent_user_edits().map(|c| &*c.new_text).collect();
        assert_eq!(user, ["e🙂"]);
    }

    #[test]
    fn ghost_text_never_becomes_part_of_the_text() {
        let mut b = Buffer::new("let x = 1;\n");
        let ghost = MarkKind::Ghost {
            text: ": i32".into(),
            color: "ink_3".into(),
        };
        b.marks_mut()
            .add_with_bias("hints", 5..5, ghost, Bias::After, Bias::After);
        // Typing at the ghost's place: it moves along, the text stays plain.
        b.edit(5..5, "yz", 5, Origin::User).expect("edit");
        assert_eq!(b.to_string(), "let xyz = 1;\n");
        let mark = b.marks().touching(0..20).next().expect("kept");
        assert_eq!(mark.range, 7..7);
        assert_eq!(b.len_chars(), 13, "nothing of the ghost is counted");
    }

    #[test]
    fn reloads_are_external_and_undo_is_the_users() {
        let mut b = Buffer::new("one\n");
        b.mark_saved();
        assert_eq!(b.reload("two\n"), Reload::Reloaded);
        assert_eq!(b.last_origin(), Some(&Origin::External));
        b.undo();
        assert_eq!(b.last_origin(), Some(&Origin::User));
        assert_eq!(b.to_string(), "one\n");
    }

    #[test]
    fn edits_in_one_undo_step_share_a_group() {
        let mut b = Buffer::new("");
        let id = b.subscribe_changes();
        b.begin_change(0);
        b.edit(0..0, "a", 0, Origin::User).expect("edit");
        b.edit(1..1, "b", 1, Origin::User).expect("edit");
        b.end_change(2);
        b.edit(2..2, "c", 2, Origin::User).expect("edit");
        b.undo();
        let groups: Vec<GroupId> = b.changes_since(0).expect("kept").map(|c| c.group).collect();
        assert_eq!(groups.len(), 4);
        assert_eq!(groups[0], groups[1], "one step");
        assert_ne!(groups[1], groups[2], "its own step");
        assert_ne!(groups[2], groups[3], "the undo is a step of its own");
        b.unsubscribe_changes(id);
    }

    #[test]
    fn a_snapshot_is_the_text_at_its_revision() {
        let mut b = Buffer::with_language("fn main() {}\n", "rust");
        let snap = b.snapshot();
        b.edit(0..0, "// x\n", 0, Origin::User).expect("edit");
        assert_eq!(snap.rope.to_string(), "fn main() {}\n");
        assert_eq!((snap.revision, b.revision()), (0, 1));
        assert_eq!(snap.language.as_deref(), Some("rust"));
        assert_eq!(Buffer::new("").language(), Some("markdown"));
        assert_eq!(Buffer::with_language("", "nope").language(), None);
    }

    #[test]
    fn a_rebased_position_still_points_at_its_character() {
        // Random edits that never touch the '#': the position found before
        // them, rebased through the log, still finds it.
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n.max(1) as u64) as usize
        };
        let pieces = ["a", "é", "🙂", "\n", "日本", ""];
        for _ in 0..200 {
            let mut b = Buffer::new("abc\ndef#ghi\njkl");
            let id = b.subscribe_changes();
            let found = CharOffset(7);
            for _ in 0..20 {
                let text = b.to_string();
                let mark = text.chars().position(|c| c == '#').expect("kept");
                let len = text.chars().count();
                // A range wholly before or wholly after the mark.
                let (from, to) = if next(2) == 0 {
                    let a = next(mark + 1);
                    (a, a + next(mark - a + 1))
                } else {
                    let a = mark + 1 + next(len - mark);
                    (a, a + next(len - a + 1))
                };
                let new = pieces[next(pieces.len())];
                b.edit(from..to, new, from, Origin::User).expect("edit");
            }
            // Following a character: text inserted right at it goes before
            // it, so the position moves past that text (`After`).
            let now = b.rebase(found, 0, Bias::After).expect("kept");
            assert_eq!(b.text().char(now.0), '#', "in {:?}", b.to_string());
            b.unsubscribe_changes(id);
        }
    }

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
        b.edit(6..11, "there", 0, Origin::User).expect("in range");
        assert_eq!(b.to_string(), "héllo there");
        b.edit(0..0, "🙂 ", 0, Origin::User).expect("in range");
        assert_eq!(b.to_string(), "🙂 héllo there");
        assert_eq!(b.len_chars(), 13);
    }

    #[test]
    fn out_of_range_edits_are_errors_not_panics() {
        let mut b = Buffer::new("abc");
        assert_eq!(
            b.edit(2..9, "x", 0, Origin::User),
            Err(OutOfBounds {
                start: 2,
                end: 9,
                len: 3
            })
        );
        let reversed = Range { start: 2, end: 1 };
        assert!(b.edit(reversed, "x", 0, Origin::User).is_err());
        assert_eq!(b.to_string(), "abc");
        assert!(!b.is_dirty());
    }

    #[test]
    fn undo_and_redo_restore_text_and_cursor() {
        let mut b = Buffer::new("one");
        b.edit(3..3, " two", 3, Origin::User).expect("edit");
        b.edit(0..3, "ONE", 0, Origin::User).expect("edit");
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
            b.edit(4 + i..4 + i, &c.to_string(), 4 + i, Origin::User)
                .expect("edit");
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
        b.edit(1..1, "b", 1, Origin::User).expect("edit");
        b.undo();
        b.edit(1..1, "c", 1, Origin::User).expect("edit");
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
        b.edit(1..1, "y", 1, Origin::User).expect("edit");
        assert!(b.is_dirty());
        b.undo();
        assert!(!b.is_dirty(), "undoing back to the saved text is clean");
        b.redo();
        b.mark_saved();
        assert!(!b.is_dirty());
        b.begin_change(2);
        b.edit(2..2, "z", 2, Origin::User).expect("edit");
        assert!(b.is_dirty(), "a change in progress is dirty");
        b.mark_saved();
        assert!(!b.is_dirty());
    }

    #[test]
    fn buffers_parse_their_language() {
        let mut rust = Buffer::with_language("fn main() {}\n", "rs");
        let tree = rust.syntax_tree_now().expect("parsed as rust");
        assert_eq!(tree.root_node().kind(), "source_file");
        let mut md = Buffer::with_language("# Hi\n", "markdown");
        assert_eq!(
            md.syntax_tree_now().expect("parsed").root_node().kind(),
            "document"
        );
        let mut plain = Buffer::with_language("text\n", "klingon");
        assert!(plain.syntax_tree_now().is_none(), "no grammar: plain text");
    }

    #[test]
    fn marks_follow_edits_undo_and_redo() {
        let mut b = Buffer::new("one two\n");
        b.marks_mut().add(
            "t",
            4..7,
            MarkKind::Highlight {
                color: None,
                background: Some("match".into()),
            },
        );
        b.edit(0..0, ">> ", 0, Origin::User).expect("edit");
        let range = |b: &Buffer| b.marks().touching(0..100).next().map(|m| m.range.clone());
        assert_eq!(range(&b), Some(7..10));
        b.undo();
        assert_eq!(range(&b), Some(4..7));
        b.redo();
        assert_eq!(range(&b), Some(7..10));
    }

    #[test]
    fn syntax_tree_follows_edits_incrementally() {
        let mut b = Buffer::new("# Title\n\nSome text.\n");
        let before = b.syntax_tree_now().expect("tree").root_node().to_sexp();
        assert!(before.contains("atx_heading"));
        b.edit(9..9, "- item\n- 🙂 two\n\n", 9, Origin::User)
            .expect("edit");
        b.edit(0..1, "##", 0, Origin::User).expect("edit");
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
    fn replaced_trees_are_freed_off_the_thread() {
        let mut b = Buffer::new("# Title\n\nSome text.\n");
        let held = b.syntax_tree_now().expect("tree").clone();
        // The buffer's next tree replaces (and hands off) the one it had.
        b.edit(0..0, "Intro\n\n", 0, Origin::User).expect("edit");
        let fresh = b.syntax_tree_now().expect("tree").clone();
        // A copy handed off while others still share its nodes.
        drop_tree_later(Some(held.clone()));
        drop_tree_later(None);
        assert!(held.root_node().to_sexp().contains("atx_heading"));
        assert_eq!(fresh.root_node().to_sexp(), fresh_sexp(&b.to_string()));
    }

    #[test]
    fn the_non_blocking_tree_always_lines_up_with_the_text() {
        let mut b = Buffer::new("# Title\n\nSome text.\n");
        b.syntax_tree_now().expect("tree");
        b.edit(0..0, "Intro line\n\n", 0, Origin::User)
            .expect("edit");
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
        dirty.edit(4..4, "!", 4, Origin::User).expect("edit");
        assert_eq!(dirty.reload("theirs"), Reload::KeptLocalEdits);
        assert_eq!(dirty.to_string(), "mine!");
    }

    #[test]
    fn nested_changes_are_one_undo_step() {
        let mut b = Buffer::new("");
        b.begin_change(0);
        b.edit(0..0, "a", 0, Origin::User).expect("edit");
        b.begin_change(1);
        b.edit(1..1, "b", 1, Origin::User).expect("edit");
        b.end_change(2);
        b.edit(2..2, "c", 2, Origin::User).expect("edit");
        b.end_change(3);
        assert_eq!(b.undo_steps(), 1);
        b.undo();
        assert_eq!(b.to_string(), "");
        b.end_change(0); // unbalanced extra end is harmless
        b.edit(0..0, "x", 0, Origin::User).expect("edit");
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
        b.edit(0..0, "ab", 0, Origin::User).expect("edit");
        b.mark_saved();
        assert!(!b.is_dirty());
        b.edit(2..2, "c", 2, Origin::User).expect("edit");
        b.edit(3..3, "d", 3, Origin::User).expect("edit");
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
        b.edit(2..3, "B\nB2", 0, Origin::User).expect("edit"); // line 1 becomes two lines
        b.edit(9..9, "!", 0, Origin::User).expect("edit"); // line 4: "d" becomes "!d"
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
        b.edit(0..0, "# Inbox\n", 0, Origin::User).expect("edit");
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
            b.edit(middle + i..middle + i, "x", middle + i, Origin::User)
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
