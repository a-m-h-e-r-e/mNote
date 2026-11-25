//! Edits that span several notes as one undo step (ADR-0010): applying an
//! edit group across documents, and carrying `u` / `⌃r` from one of them
//! to the others.
//!
//! A note the group touches that isn't open is opened as a parked document
//! (kept, not shown, so its undo history lives on), edited and saved at
//! once. Undoing the group from any of its notes undoes it in the others
//! whose next undo step is still that group; a note edited since keeps its
//! newer text and is reported.

use std::ops::Range;

use mnote_core::text::{GroupId, Origin, Undone};

use super::*;

/// The edits for one note: character ranges of its current text and their
/// new text, in any order (they mustn't overlap).
pub(super) type NoteEdits = (NotePath, Vec<(Range<usize>, String)>);

impl Editor {
    /// Applies `edits` as one edit group with `origin` and `label`; returns
    /// how many notes changed, or why nothing could.
    pub(super) fn apply_edit_group(
        &mut self,
        edits: Vec<NoteEdits>,
        origin: Origin,
        label: &str,
        cx: &mut Context<Self>,
    ) -> Result<usize, String> {
        let Some(vault) = self.vault.clone() else {
            return Err("a scratch note has no vault to edit".into());
        };
        // Every note must exist before anything changes.
        for (note, _) in &edits {
            if !vault.path_of(note).exists() {
                return Err(format!("there's no note {note}"));
            }
        }
        let group = GroupId::fresh();
        let mut touched = Vec::new();
        for (note, mut ranges) in edits {
            if ranges.is_empty() {
                continue;
            }
            // Back to front, so earlier ranges keep their offsets.
            ranges.sort_by_key(|(r, _)| std::cmp::Reverse(r.start));
            let key = DocKey::Note(note.clone());
            if self.source.doc_key() == key {
                self.edit_in_group(group, &origin, label, &ranges, true);
                self.sync_lines();
                self.schedule_autosave(cx);
            } else {
                if self.parked_index(&key).is_none() {
                    let buffer = Buffer::open(&vault, &note)
                        .map_err(|err| format!("couldn't open {note}: {err}"))?;
                    let source = Source::Note {
                        vault: vault.clone(),
                        note: note.clone(),
                    };
                    let doc = self.new_doc(buffer, source);
                    self.parked.push(doc);
                }
                let saved = self.with_parked(&note, |e| {
                    e.edit_in_group(group, &origin, label, &ranges, false);
                    e.buffer.save(&vault, &note)
                });
                if let Some(Err(err)) = saved {
                    self.show_toast(format!("Couldn't save {note}: {err}"), false, cx);
                }
            }
            touched.push(key);
        }
        let count = touched.len();
        self.edit_groups.record(group, touched);
        cx.notify();
        Ok(count)
    }

    /// The edits of one note, as one step of `group`. The focused note goes
    /// through Vim (so its marks follow); a parked one only through its
    /// buffer, since Vim's marks belong to the focused note.
    fn edit_in_group(
        &mut self,
        group: GroupId,
        origin: &Origin,
        label: &str,
        ranges: &[(Range<usize>, String)],
        focused: bool,
    ) {
        let cursor = self.vim.cursor();
        self.buffer.begin_group(cursor, group, Some(label));
        let user = self.vim.set_origin(origin.clone());
        for (range, text) in ranges {
            let len = self.buffer.len_chars();
            let range = range.start.min(len)..range.end.min(len);
            if focused {
                self.vim.edit_text(&mut self.buffer, range, text);
            } else {
                let _ = self.buffer.edit(range, text, cursor, origin.clone());
            }
        }
        self.vim.set_origin(user);
        let cursor = clamp_normal(self.buffer.text(), self.vim.cursor());
        self.buffer.end_change(cursor);
        if focused {
            self.vim.set_cursor(cursor);
        }
    }

    /// After an undo or redo in the focused note: the same group in the
    /// other notes it spans.
    pub(super) fn carry_undo(&mut self, cx: &mut Context<Self>) {
        let Some(Undone { group, redo, label }) = self.buffer.take_undone() else {
            return;
        };
        let others = self.edit_groups.others(group, &self.source.doc_key());
        if others.is_empty() {
            return;
        }
        let Some(vault) = self.vault.clone() else {
            return;
        };
        let (mut done, mut skipped) = (0, 0);
        for key in others {
            let DocKey::Note(note) = key else {
                continue;
            };
            let undone = self.with_parked(&note, |e| {
                let undone = e.buffer.undo_group(group, redo).is_some();
                if undone {
                    let _ = e.buffer.save(&vault, &note);
                }
                undone
            });
            match undone {
                Some(true) => done += 1,
                // Changed since, or closed (its history went with it).
                _ => skipped += 1,
            }
        }
        let what = label.as_deref().unwrap_or("the edit");
        let verb = if redo { "Redid" } else { "Undid" };
        let notes = |n: usize| if n == 1 { "note" } else { "notes" };
        let mut text = format!("{verb} {what} in {done} other {}", notes(done));
        if skipped > 0 {
            text.push_str(&format!(
                "; {skipped} {} changed since and kept {}",
                notes(skipped),
                if skipped == 1 {
                    "its text"
                } else {
                    "their text"
                }
            ));
        }
        self.reply = Some((text.into(), skipped > 0));
        cx.notify();
    }
}
