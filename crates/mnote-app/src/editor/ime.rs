//! Text input through the system (IME): accents from dead keys and
//! press-and-hold, and composed input such as Chinese or Japanese.
//!
//! In INSERT, printable keys aren't taken as key events; macOS's input
//! system gets them and hands back text, which goes to Vim as typed keys (so
//! `jk`, mappings and undo work as before). Text still being composed (the
//! "marked" text) isn't in the note yet: it's drawn under the caret until
//! it's committed. In every other mode keys are handled before the input
//! system sees them, and any text it sends is ignored.

use std::ops::Range;

use gpui::{
    Bounds, Context, EntityInputHandler, Pixels, Point, UTF16Selection, Window, point, px, size,
};
use mnote_vim::text::line_of;
use mnote_vim::{Key, Mode};

use super::{Editor, styled};

impl Editor {
    /// Whether typed text goes to the note: INSERT, with nothing over it.
    pub(super) fn takes_text(&self) -> bool {
        self.vim.mode() == Mode::Insert
            && self.picker.is_none()
            && self.folder_edit.is_none()
            && self.first_run.is_none()
            && self.vim.cmd_window().is_none()
    }

    fn cursor_utf16(&self) -> usize {
        let rope = self.buffer.text();
        rope.char_to_utf16_cu(self.vim.cursor().min(rope.len_chars()))
    }

    fn utf16_to_char(&self, offset: usize) -> usize {
        let rope = self.buffer.text();
        rope.utf16_cu_to_char(offset.min(rope.len_utf16_cu()))
    }

    /// Committed text, typed into the note one key at a time.
    fn type_text(&mut self, text: &str, cx: &mut Context<Self>) {
        for c in text.chars() {
            let key = if c == '\n' { Key::Enter } else { Key::Char(c) };
            self.press(key, cx);
        }
    }

    /// The input system replaces text just before the caret (an accent
    /// chosen by holding a key): those characters go first.
    fn delete_before_caret(&mut self, range: Option<Range<usize>>, cx: &mut Context<Self>) {
        let Some(range) = range else {
            return;
        };
        let caret = self.cursor_utf16();
        if self.marked.is_some() || range.start >= caret {
            return;
        }
        let chars = self.vim.cursor() - self.utf16_to_char(range.start);
        for _ in 0..chars {
            self.press(Key::Backspace, cx);
        }
    }
}

impl EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let rope = self.buffer.text();
        let total = rope.len_utf16_cu();
        let range = range.start.min(total)..range.end.min(total);
        *adjusted = Some(range.clone());
        let (from, to) = (
            self.utf16_to_char(range.start),
            self.utf16_to_char(range.end),
        );
        Some(rope.slice(from..to.max(from)).to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let caret = self.cursor_utf16();
        Some(UTF16Selection {
            range: caret..caret,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let marked = self.marked.as_ref()?;
        let caret = self.cursor_utf16();
        Some(caret..caret + marked.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.takes_text() {
            self.marked = None;
            return;
        }
        self.delete_before_caret(range, cx);
        self.marked = None;
        self.type_text(text, cx);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.takes_text() {
            return;
        }
        self.delete_before_caret(range, cx);
        self.marked = (!new_text.is_empty()).then(|| new_text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The caret's cell, for the candidate window: the cursor line's row,
        // past the gutter, at the caret's column.
        let rope = self.buffer.text();
        let line = line_of(rope, self.vim.cursor());
        let column = self.vim.cursor() - rope.line_to_char(line);
        let row = self.lines.bounds_for_item(line).unwrap_or(element_bounds);
        let x = row.origin.x + px(crate::theme::size::GUTTER_W + column as f32 * styled::char_w());
        Some(Bounds::new(
            point(x, row.origin.y),
            size(px(styled::char_w()), row.size.height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
