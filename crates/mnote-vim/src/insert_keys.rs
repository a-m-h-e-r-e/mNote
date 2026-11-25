//! INSERT-mode keys beyond typing: `⌃w` `⌃u` (delete before the cursor),
//! `⌃r {reg}` (insert a register), `⌃t` `⌃d` (indent, outdent) and `⌃o` (one
//! NORMAL command, then back to INSERT). They stay in the insert's undo step.

use mnote_core::Buffer;

use crate::Mode;
use crate::registers::is_register_name;
use crate::state::{InsertSession, Vim};
use crate::text::{first_non_blank, line_len, line_of, line_start};

/// What `⌃o` set aside while its NORMAL command runs.
#[derive(Debug, Clone)]
pub(crate) struct Paused {
    session: Option<InsertSession>,
    /// The cursor in INSERT, which may be just past the end of the line.
    pos: usize,
    revision: u64,
}

impl Vim {
    /// `⌃w`: the word (or the blanks and then the word) before the cursor;
    /// at the start of a line, the line break.
    pub(crate) fn delete_word_before(&mut self, buf: &mut Buffer) {
        let text = buf.text();
        let start = line_start(text, line_of(text, self.cursor));
        if self.cursor == start {
            self.delete_before(self.cursor.saturating_sub(1), buf);
            return;
        }
        let class = |c: char| {
            if c.is_whitespace() {
                0
            } else if c.is_alphanumeric() || c == '_' {
                1
            } else {
                2
            }
        };
        let mut i = self.cursor;
        while i > start && class(text.char(i - 1)) == 0 {
            i -= 1;
        }
        if i > start {
            let cls = class(text.char(i - 1));
            while i > start && class(text.char(i - 1)) == cls {
                i -= 1;
            }
        }
        self.delete_before(i, buf);
    }

    /// `⌃u`: what was typed before the cursor on this line; with nothing
    /// typed, the text back to the indent, then the indent; at the start of
    /// a line, the line break.
    pub(crate) fn delete_line_before(&mut self, buf: &mut Buffer) {
        let text = buf.text();
        let start = line_start(text, line_of(text, self.cursor));
        if self.cursor == start {
            self.delete_before(self.cursor.saturating_sub(1), buf);
            return;
        }
        let typed_from = self
            .insert
            .as_ref()
            .map(|s| s.start)
            .filter(|&at| at > start && at < self.cursor);
        let indent = first_non_blank(text, line_of(text, self.cursor));
        let from = match typed_from {
            Some(at) => at,
            None if self.cursor > indent => indent,
            None => start,
        };
        self.delete_before(from, buf);
    }

    /// Deletes `from..cursor`, taking it back from what the insert typed.
    fn delete_before(&mut self, from: usize, buf: &mut Buffer) {
        let n = self.cursor - from;
        if n == 0 {
            return;
        }
        let at = self.cursor;
        self.cursor = from;
        self.edit(buf, from..at, "");
        if let Some(session) = &mut self.insert {
            for _ in 0..n {
                if session.typed.pop().is_none() {
                    session.broken = true;
                }
            }
            session.start = session.start.min(from);
        }
    }

    /// `⌃r {reg}`: the register's text, as if typed.
    pub(crate) fn insert_register(&mut self, name: char, buf: &mut Buffer) {
        if !is_register_name(name) {
            return;
        }
        if let Some(reg) = self.registers.get(Some(name)) {
            self.typed(buf, &reg.text);
        }
    }

    /// `⌃t` / `⌃d`: the line's indent to the next or previous multiple of
    /// `shiftwidth`, with the cursor staying on its text.
    pub(crate) fn shift_insert_line(&mut self, indent: bool, buf: &mut Buffer) {
        let text = buf.text();
        let line = line_of(text, self.cursor);
        let start = line_start(text, line);
        let width = self.options.shiftwidth.max(1);
        let (mut chars, mut cols) = (0, 0);
        while chars < line_len(text, line) {
            match text.char(start + chars) {
                ' ' => cols += 1,
                '\t' => cols += width,
                _ => break,
            }
            chars += 1;
        }
        let new_cols = if indent {
            (cols / width + 1) * width
        } else if cols == 0 {
            return;
        } else {
            (cols - 1) / width * width
        };
        let old_end = start + chars;
        let new_end = start + new_cols;
        self.edit(buf, start..old_end, &" ".repeat(new_cols));
        let shift = |pos: usize| {
            if pos >= old_end {
                pos - old_end + new_end
            } else {
                new_end.min(pos)
            }
        };
        self.cursor = shift(self.cursor);
        if let Some(session) = &mut self.insert {
            session.broken = true;
            if session.start >= start {
                session.start = shift(session.start);
            }
        }
    }

    /// `⌃o`: NORMAL for one command. The insert's undo step stays open, so
    /// the command and the typing around it undo together.
    pub(crate) fn pause_insert(&mut self, buf: &mut Buffer) {
        self.paused = Some(Paused {
            session: self.insert.take(),
            pos: self.cursor,
            revision: buf.revision(),
        });
        self.enter(Mode::Normal, buf.text());
    }

    /// After a key in NORMAL (or the command line) following `⌃o`: once the
    /// command is done, back to INSERT where it left the cursor.
    pub(crate) fn resume_insert(&mut self, buf: &mut Buffer) {
        if matches!(self.mode, Mode::Insert | Mode::Replace) {
            // The command started an insert of its own (`⌃o o`): that one
            // carries on, inside the same undo step.
            if self.paused.take().is_some() {
                buf.end_change(self.cursor);
            }
            return;
        }
        if self.mode != Mode::Normal || !self.is_idle() || self.cmd.cmdline.is_some() {
            return;
        }
        let Some(paused) = self.paused.take() else {
            return;
        };
        let text = buf.text();
        // A command that didn't move the cursor leaves it where INSERT had
        // it, even past the end of the line.
        if buf.revision() == paused.revision
            && self.cursor == crate::text::clamp_normal(text, paused.pos)
        {
            self.cursor = paused.pos.min(text.len_chars());
        }
        let mut session = paused.session.unwrap_or_default();
        session.broken = true;
        session.start = self.cursor;
        self.insert = Some(session);
        self.enter(Mode::Insert, buf.text());
        // `.` repeats the typing after the command, as an insert.
        self.restart_keys(vec![crate::Key::Char('i')], buf.revision());
    }
}

#[cfg(test)]
mod tests {
    use crate::harness::{check, run};

    #[test]
    fn ctrl_w_deletes_the_word_before_the_cursor() {
        check("|\n", "ifoo bar<C-w><Esc>", "foo| \n");
        check("|\n", "ifoo bar  <C-w><Esc>", "foo| \n");
        check("|\n", "ifoo.bar<C-w><C-w><Esc>", "fo|o\n");
        // At the start of a line: the line break.
        check("ab\n|cd\n", "i<C-w><Esc>", "a|bcd\n");
        check("|\n", "iab<C-w>cd<Esc>", "c|d\n");
    }

    #[test]
    fn ctrl_u_deletes_what_was_typed_then_to_the_indent() {
        check("ab|c\n", "ixy<C-u><Esc>", "a|bc\n");
        check("  ab|c\n", "i<C-u><Esc>", " | c\n");
        check("  |abc\n", "i<C-u><Esc>", "|abc\n");
        check("ab\n|cd\n", "i<C-u><Esc>", "a|bcd\n");
        // Typed after Backspace past the start: back to the new start.
        check("ab|\n", "a<BS>xy<C-u>z<Esc>", "a|z\n");
    }

    #[test]
    fn ctrl_h_is_backspace() {
        check("|\n", "iabc<C-h><Esc>", "a|b\n");
    }

    #[test]
    fn ctrl_r_inserts_a_register() {
        check("|foo bar\n", "yiwA <C-r>\"<Esc>", "foo bar fo|o\n");
        check(
            "|foo bar\n",
            "\"ayiwwyiwA <C-r>a<C-r>0<Esc>",
            "foo bar fooba|r\n",
        );
        // An unknown or empty register inserts nothing.
        check("|x\n", "a<C-r>q!<Esc>", "x|!\n");
        let (_, vim, _) = run("|x\n", "a<C-r>");
        assert!(vim.awaits_register());
    }

    #[test]
    fn ctrl_t_and_ctrl_d_shift_the_line() {
        // `shiftwidth` is 2.
        check("|ab\n", "i<C-t><Esc>", " | ab\n");
        check("a|b\n", "i<C-t><C-t>x<Esc>", "    a|xb\n");
        check("   a|b\n", "i<C-t><Esc>", "    |ab\n");
        check("   a|b\n", "i<C-d><Esc>", "  |ab\n");
        check("a|b\n", "i<C-d><Esc>", "|ab\n");
    }

    #[test]
    fn ctrl_o_runs_one_normal_command() {
        check("|abc def\n", "ix<C-o>wy<Esc>", "xabc |ydef\n");
        check("|abc\n", "A<C-o>0x<Esc>", "|xabc\n");
        // At the end of the line, the cursor comes back past the end.
        check("|abc\n", "A<C-o>:<Esc>!<Esc>", "abc|!\n");
        check("|abc\n", "A<C-o>zz!<Esc>", "abc|!\n");
        // A command that inserts carries on as that insert.
        check("|ab\n", "i<C-o>oxy<Esc>", "ab\nx|y\n");
        // A command with a count and an operator.
        check("|a b c d\n", "i<C-o>2dwX<Esc>", "|Xc d\n");
    }

    #[test]
    fn insert_keys_undo_with_the_typing() {
        check("|ab\n", "ifoo bar<C-w>x<C-t><Esc>u", "|ab\n");
        check("|ab cd\n", "ix<C-o>wy<Esc>u", "|ab cd\n");
        check("|\n", "iab cd<C-w>x<Esc>.", "ab ab |xx\n");
    }
}
