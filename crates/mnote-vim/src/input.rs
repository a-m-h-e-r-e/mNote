//! [`VimInput`]: a one-line text field that speaks Vim (decision D-001), for
//! picker queries, quick capture and inline rename.
//!
//! It opens in INSERT, so typing goes straight in. `esc` goes to NORMAL, where
//! the usual motions and edits work on the text; `esc` again (or `q`) closes.
//! The keys
//! that move a list come back as [`InputAction::Move`]: `⌃n` / `⌃p` (and the
//! arrows) in any mode, `j` / `k` in NORMAL.

use mnote_core::Buffer;

use crate::text::line_start;
use crate::{Key, Mode, Vim};

/// What a key did, for the picker or dialog that owns the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    /// The text changed.
    Edited,
    /// Only the caret or mode changed (or nothing did).
    Moved,
    /// Move the list selection by this many rows.
    Select(isize),
    /// `enter`.
    Submit,
    /// `esc` in NORMAL mode: close the dialog.
    Close,
    /// A NORMAL-mode key the owner claimed (`l` to open, `h` to go up).
    Claimed(char),
}

/// A single-line Vim field.
pub struct VimInput {
    buffer: Buffer,
    vim: Vim,
}

impl std::fmt::Debug for VimInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VimInput")
            .field("text", &self.text())
            .field("mode", &self.vim.mode())
            .finish()
    }
}

impl Default for VimInput {
    fn default() -> Self {
        Self::new("")
    }
}

impl VimInput {
    /// A field holding `text`, in INSERT with the caret at the end.
    pub fn new(text: &str) -> Self {
        let buffer = Buffer::new(text);
        let mut vim = Vim::for_input();
        vim.set_cursor(buffer.len_chars());
        VimInput { buffer, vim }
    }

    pub fn text(&self) -> String {
        self.buffer.to_string()
    }

    /// The caret, in characters.
    pub fn cursor(&self) -> usize {
        self.vim.cursor()
    }

    pub fn mode(&self) -> Mode {
        self.vim.mode()
    }

    /// Empties the field, keeping its mode.
    pub fn clear(&mut self) {
        let len = self.buffer.len_chars();
        if len > 0 {
            self.vim.edit(&mut self.buffer, 0..len, "");
        }
        self.vim.set_cursor(0);
    }

    /// The selected characters in VISUAL mode.
    pub fn selection(&self) -> Option<std::ops::Range<usize>> {
        self.vim.selection()
    }

    /// Handles one key. `claims` names NORMAL-mode keys the owner handles
    /// itself (they come back as [`InputAction::Claimed`]).
    pub fn handle_key(&mut self, key: Key, claims: impl Fn(char) -> bool) -> InputAction {
        let mode = self.vim.mode();
        let idle = !self.vim.is_pending();
        match key {
            Key::Ctrl('n') | Key::Down => return InputAction::Select(1),
            Key::Ctrl('p') | Key::Up => return InputAction::Select(-1),
            Key::Enter => return InputAction::Submit,
            _ => {}
        }
        if mode == Mode::Normal && idle {
            match key {
                Key::Esc | Key::Char('q') => return InputAction::Close,
                Key::Char('j') => return InputAction::Select(1),
                Key::Char('k') => return InputAction::Select(-1),
                Key::Char(c) if claims(c) => return InputAction::Claimed(c),
                // One line: no new lines, and no command line inside a field.
                Key::Char('o' | 'O' | ':' | '/' | '?') => return InputAction::Moved,
                _ => {}
            }
        }
        if mode == Mode::Insert {
            match key {
                Key::Ctrl('w') => return self.delete_before(true),
                Key::Ctrl('u') => return self.delete_before(false),
                Key::Left | Key::Right => {
                    let to = if key == Key::Left {
                        self.cursor().saturating_sub(1)
                    } else {
                        (self.cursor() + 1).min(self.buffer.len_chars())
                    };
                    self.vim.set_cursor(to);
                    return InputAction::Moved;
                }
                _ => {}
            }
        }

        let revision = self.buffer.revision();
        self.vim.handle_key(key, &mut self.buffer);
        self.keep_one_line();
        if self.buffer.revision() != revision {
            InputAction::Edited
        } else {
            InputAction::Moved
        }
    }

    /// `⌃w` (the word before the caret) and `⌃u` (everything before it).
    fn delete_before(&mut self, word: bool) -> InputAction {
        let text = self.buffer.text();
        let end = self.cursor();
        let chars: Vec<char> = text.slice(..end).chars().collect();
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let mut start = end;
        if word {
            while start > 0 && chars[start - 1].is_whitespace() {
                start -= 1;
            }
            if start > 0 && is_word(chars[start - 1]) {
                while start > 0 && is_word(chars[start - 1]) {
                    start -= 1;
                }
            } else {
                start = start.saturating_sub(1);
            }
        } else {
            start = line_start(text, 0);
        }
        if start == end {
            return InputAction::Moved;
        }
        self.vim.edit(&mut self.buffer, start..end, "");
        self.vim.set_cursor(start);
        InputAction::Edited
    }

    /// A paste may bring line breaks; the field stays one line.
    fn keep_one_line(&mut self) {
        let text = self.buffer.text();
        if text.chars().all(|c| c != '\n') {
            return;
        }
        let flat: String = text
            .chars()
            .map(|c| if c == '\n' { ' ' } else { c })
            .collect::<String>()
            .trim_end()
            .to_owned();
        let len = self.buffer.len_chars();
        let cursor = self.cursor().min(flat.chars().count());
        self.vim.edit(&mut self.buffer, 0..len, &flat);
        self.vim.set_cursor(cursor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_keys;

    fn run(input: &mut VimInput, keys: &str) -> Vec<InputAction> {
        parse_keys(keys)
            .into_iter()
            .map(|k| input.handle_key(k, |c| c == 'l'))
            .collect()
    }

    #[test]
    fn opens_in_insert_and_types() {
        let mut input = VimInput::default();
        assert_eq!(input.mode(), Mode::Insert);
        assert_eq!(run(&mut input, "gpu"), [InputAction::Edited; 3]);
        assert_eq!(input.text(), "gpu");
        assert_eq!(input.cursor(), 3);
        // j and k are letters while inserting (`jk` together is the
        // default mapping to <Esc>).
        run(&mut input, "kj");
        assert_eq!(input.text(), "gpukj");
    }

    #[test]
    fn list_keys_in_each_mode() {
        let mut input = VimInput::new("gpu");
        assert_eq!(
            run(&mut input, "<c-n><c-p><down><up>"),
            [
                InputAction::Select(1),
                InputAction::Select(-1),
                InputAction::Select(1),
                InputAction::Select(-1)
            ]
        );
        assert_eq!(run(&mut input, "<esc>"), [InputAction::Moved]);
        assert_eq!(input.mode(), Mode::Normal);
        assert_eq!(
            run(&mut input, "jk<c-n>l<cr>"),
            [
                InputAction::Select(1),
                InputAction::Select(-1),
                InputAction::Select(1),
                InputAction::Claimed('l'),
                InputAction::Submit
            ]
        );
        assert_eq!(
            run(&mut input, "<esc>"),
            [InputAction::Close],
            "esc again closes"
        );
        let mut input = VimInput::new("gpu");
        assert_eq!(
            run(&mut input, "q"),
            [InputAction::Edited],
            "q types in INSERT"
        );
        assert_eq!(
            run(&mut input, "<esc>q"),
            [InputAction::Moved, InputAction::Close],
            "q closes from NORMAL"
        );
    }

    #[test]
    fn normal_mode_edits_the_query() {
        let mut input = VimInput::new("gpu notes");
        run(&mut input, "<esc>bdw");
        assert_eq!(input.text(), "gpu ");
        run(&mut input, "0cwzed<esc>");
        assert_eq!(input.text(), "zed ");
        run(&mut input, "u");
        assert_eq!(input.text(), "gpu ");
        run(&mut input, "A more");
        assert_eq!(
            (input.mode(), input.text()),
            (Mode::Insert, "gpu  more".into())
        );
        run(&mut input, "<esc>o:");
        assert_eq!(input.text(), "gpu  more", "o and : do nothing in a field");
    }

    #[test]
    fn insert_mode_line_keys() {
        let mut input = VimInput::new("one two three");
        run(&mut input, "<c-w>");
        assert_eq!(input.text(), "one two ");
        run(&mut input, "<left><left>X");
        assert_eq!((input.text().as_str(), input.cursor()), ("one twXo ", 7));
        run(&mut input, "<c-u>");
        assert_eq!((input.text().as_str(), input.cursor()), ("o ", 0));
    }

    #[test]
    fn clearing_keeps_the_mode() {
        let mut input = VimInput::new("des");
        run(&mut input, "<esc>");
        input.clear();
        assert_eq!((input.text().as_str(), input.mode()), ("", Mode::Normal));
    }

    #[test]
    fn a_pasted_line_break_becomes_a_space() {
        let mut input = VimInput::new("a");
        run(&mut input, "<esc>yy");
        run(&mut input, "p");
        assert_eq!(input.text(), "a a");
    }
}
