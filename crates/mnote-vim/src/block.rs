//! VISUAL BLOCK (`⌃v`): a rectangle of columns across lines. Operators act
//! on each line's piece, `I` / `A` repeat their typing on every line when
//! INSERT ends, and a block yank pastes as a column. Columns count
//! characters.

use std::ops::Range;

use mnote_core::Buffer;
use ropey::Rope;

use crate::Mode;
use crate::registers::Register;
use crate::state::{Operator, Vim};
use crate::text::{column, last_line, line_end, line_len, line_of, line_start};

/// The selected rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub top: usize,
    pub bottom: usize,
    pub left: usize,
    /// Exclusive.
    pub right: usize,
    /// `$`: every line to its end.
    pub to_end: bool,
}

impl Block {
    /// The selected characters of `line` (relative to its start); `None`
    /// when the line is shorter than the block's left edge.
    pub fn columns(&self, text: &Rope, line: usize) -> Option<Range<usize>> {
        let len = line_len(text, line);
        if !(self.top..=self.bottom).contains(&line) || self.left >= len {
            return None;
        }
        let right = if self.to_end {
            len
        } else {
            self.right.min(len)
        };
        Some(self.left..right)
    }

    fn span(&self, text: &Rope, line: usize) -> Option<Range<usize>> {
        let start = line_start(text, line);
        self.columns(text, line)
            .map(|cols| start + cols.start..start + cols.end)
    }
}

/// Block state kept on [`Vim`].
#[derive(Debug, Clone, Default)]
pub(crate) struct BlockState {
    /// `$` was the last horizontal motion.
    pub(crate) to_end: bool,
    /// `I`, `A` or `c` on a block: where the typing goes on the other lines.
    insert: Option<BlockInsert>,
}

#[derive(Debug, Clone, Copy)]
struct BlockInsert {
    first: usize,
    last: usize,
    /// `None`: each line's end.
    col: Option<usize>,
    /// Lines shorter than `col` get spaces first (`A`), or are skipped.
    pad: bool,
}

impl Vim {
    /// The rectangle in VISUAL BLOCK, for drawing.
    pub fn block(&self, text: &Rope) -> Option<Block> {
        (self.mode == Mode::VisualBlock).then(|| self.block_now(text))
    }

    fn block_now(&self, text: &Rope) -> Block {
        let len = text.len_chars();
        let (a, c) = (self.anchor.min(len), self.cursor.min(len));
        let col_a = column(text, a);
        // The column `j` / `k` aim for, so short lines don't narrow it.
        let col_c = self.want_col.unwrap_or_else(|| column(text, c));
        let (la, lc) = (line_of(text, a), line_of(text, c));
        Block {
            top: la.min(lc),
            bottom: la.max(lc),
            left: col_a.min(col_c),
            right: col_a.max(col_c) + 1,
            to_end: self.block.to_end,
        }
    }

    /// `O`: the cursor to the other corner on its own line.
    pub(crate) fn block_other_corner(&mut self, text: &Rope) {
        let b = self.block_now(text);
        let col_a = column(text, self.anchor);
        let col_c = self.want_col.unwrap_or_else(|| column(text, self.cursor));
        let at = |line: usize, col: usize| {
            line_start(text, line) + col.min(line_len(text, line).saturating_sub(1))
        };
        let (la, lc) = (line_of(text, self.anchor), line_of(text, self.cursor));
        self.anchor = at(la, col_c);
        self.cursor = at(lc, col_a);
        self.want_col = (b.right > b.left).then_some(col_a);
    }

    /// An operator on the block: `d` `x` `c` `s` `y` `~` `u` `U` `>` `<`.
    pub(crate) fn block_operator(&mut self, op: Operator, buf: &mut Buffer) {
        let text = buf.text().clone();
        let b = self.block_now(&text);
        self.leave_block(&text, &b);
        let name = self.register.take();
        buf.begin_change(self.cursor);
        match op {
            Operator::Yank | Operator::Delete | Operator::Change => {
                let pieces: Vec<String> = (b.top..=b.bottom)
                    .map(|line| {
                        b.span(&text, line)
                            .map_or_else(String::new, |r| text.slice(r).to_string())
                    })
                    .collect();
                let reg = Register {
                    text: pieces.join("\n"),
                    linewise: false,
                    block: true,
                };
                if op == Operator::Yank {
                    self.registers.yank(name, reg);
                } else {
                    self.registers.delete(name, reg);
                    for line in (b.top..=b.bottom).rev() {
                        if let Some(span) = b.span(&text, line) {
                            self.edit(buf, span, "");
                        }
                    }
                }
                if op == Operator::Change {
                    self.block.insert = Some(BlockInsert {
                        first: b.top,
                        last: b.bottom,
                        col: Some(b.left),
                        pad: false,
                    });
                    self.cursor =
                        line_start(buf.text(), b.top) + b.left.min(line_len(buf.text(), b.top));
                    self.begin_insert(buf);
                    return;
                }
            }
            Operator::Lower | Operator::Upper | Operator::Toggle => {
                for line in b.top..=b.bottom {
                    if let Some(span) = b.span(&text, line) {
                        let piece: String = text
                            .slice(span.clone())
                            .chars()
                            .map(|c| change_case(op, c))
                            .collect();
                        self.edit(buf, span, &piece);
                    }
                }
            }
            Operator::Indent | Operator::Outdent => {
                let width = self.options.shiftwidth;
                // Bottom up, so the spans read before editing stay right.
                for line in (b.top..=b.bottom).rev() {
                    let Some(span) = b.span(&text, line) else {
                        continue;
                    };
                    let at = span.start;
                    if op == Operator::Indent {
                        self.edit(buf, at..at, &" ".repeat(width));
                    } else {
                        let end = line_end(&text, line);
                        let spaces = (at..end.min(at + width))
                            .take_while(|&i| text.char(i) == ' ')
                            .count();
                        self.edit(buf, at..at + spaces, "");
                    }
                }
            }
            Operator::Func(_) => {}
        }
        let text = buf.text();
        self.cursor = crate::text::clamp_normal(
            text,
            line_start(text, b.top) + b.left.min(line_len(text, b.top)),
        );
        buf.end_change(self.cursor);
        self.done(buf, false);
    }

    /// `r{char}` on the block.
    pub(crate) fn block_replace(&mut self, with: char, buf: &mut Buffer) {
        let text = buf.text().clone();
        let b = self.block_now(&text);
        self.leave_block(&text, &b);
        buf.begin_change(self.cursor);
        for line in b.top..=b.bottom {
            if let Some(span) = b.span(&text, line) {
                let piece = with.to_string().repeat(span.len());
                self.edit(buf, span, &piece);
            }
        }
        buf.end_change(self.cursor);
        self.done(buf, false);
    }

    /// `I` / `A`: INSERT on the block's first line; the typing is repeated
    /// on the others when it ends.
    pub(crate) fn block_insert(&mut self, append: bool, buf: &mut Buffer) {
        let text = buf.text().clone();
        let b = self.block_now(&text);
        self.leave_block(&text, &b);
        buf.begin_change(self.cursor);
        let (col, pad) = match (append, b.to_end) {
            (false, _) => (Some(b.left), false),
            (true, true) => (None, false),
            (true, false) => (Some(b.right), true),
        };
        self.cursor = match col {
            None => line_end(&text, b.top),
            Some(col) => {
                let len = line_len(&text, b.top);
                if pad && len < col {
                    let end = line_start(&text, b.top) + len;
                    self.edit(buf, end..end, &" ".repeat(col - len));
                }
                line_start(buf.text(), b.top) + col.min(line_len(buf.text(), b.top))
            }
        };
        self.block.insert = Some(BlockInsert {
            first: b.top,
            last: b.bottom,
            col,
            pad,
        });
        self.begin_insert(buf);
    }

    /// When INSERT ends after `I`, `A` or `c` on a block: the typed text goes
    /// on the block's other lines too (not when it spans lines).
    pub(crate) fn finish_block_insert(&mut self, typed: Option<&str>, buf: &mut Buffer) {
        let Some(bi) = self.block.insert.take() else {
            return;
        };
        let Some(typed) = typed.filter(|t| !t.is_empty() && !t.contains('\n')) else {
            return;
        };
        for line in bi.first + 1..=bi.last {
            let text = buf.text();
            let (start, len) = (line_start(text, line), line_len(text, line));
            let at = match bi.col {
                None => start + len,
                Some(col) if len >= col => start + col,
                Some(col) if bi.pad => {
                    self.edit(buf, start + len..start + len, &" ".repeat(col - len));
                    start + col
                }
                Some(_) => continue,
            };
            self.edit(buf, at..at, typed);
        }
    }

    /// Puts a block register: its lines go into the column at the cursor
    /// (after it with `after`) on successive lines, padding short lines and
    /// adding lines past the end. The cursor ends at the column's top.
    pub(crate) fn put_block(
        &mut self,
        reg: &Register,
        after: bool,
        buf: &mut Buffer,
        count: usize,
    ) {
        let text = buf.text();
        let line = line_of(text, self.cursor);
        let col = column(text, self.cursor) + usize::from(after && line_len(text, line) > 0);
        for (i, piece) in reg.text.split('\n').enumerate() {
            let piece = piece.repeat(count.max(1));
            let target = line + i;
            let text = buf.text();
            let len = text.len_chars();
            if target > last_line(text) {
                let row = format!("{}{piece}", " ".repeat(col));
                if len > 0 && text.char(len - 1) == '\n' {
                    self.edit(buf, len..len, &format!("{row}\n"));
                } else {
                    self.edit(buf, len..len, &format!("\n{row}"));
                }
                continue;
            }
            let (start, width) = (line_start(text, target), line_len(text, target));
            if width < col {
                if piece.is_empty() {
                    continue;
                }
                let end = start + width;
                self.edit(
                    buf,
                    end..end,
                    &format!("{}{piece}", " ".repeat(col - width)),
                );
            } else {
                self.edit(buf, start + col..start + col, &piece);
            }
        }
        let text = buf.text();
        self.cursor = crate::text::clamp_normal(text, line_start(text, line) + col);
    }

    /// `p` / `P` over a block: the block goes (into the unnamed register)
    /// and the register is put where it was.
    pub(crate) fn block_put_over(&mut self, buf: &mut Buffer) -> crate::state::Effect {
        let reg = self.registers.get(self.register.take());
        let text = buf.text().clone();
        let b = self.block_now(&text);
        self.leave_block(&text, &b);
        let Some(reg) = reg else {
            return crate::state::Effect::Handled;
        };
        buf.begin_change(self.cursor);
        let mut pieces = Vec::new();
        for line in (b.top..=b.bottom).rev() {
            let span = b.span(&text, line);
            pieces.push(
                span.clone()
                    .map_or_else(String::new, |r| text.slice(r).to_string()),
            );
            if let Some(span) = span {
                self.edit(buf, span, "");
            }
        }
        pieces.reverse();
        let at = buf.text();
        self.cursor = line_start(at, b.top) + b.left.min(line_len(at, b.top));
        self.put(&reg, false, buf, 1);
        self.registers.delete(
            None,
            Register {
                text: pieces.join("\n"),
                linewise: false,
                block: true,
            },
        );
        buf.end_change(self.cursor);
        self.done(buf, false);
        crate::state::Effect::Handled
    }

    /// Leaves VISUAL BLOCK with `'<` `'>` set and the cursor at the top-left
    /// corner.
    fn leave_block(&mut self, text: &Rope, b: &Block) {
        self.marks.insert(
            '<',
            line_start(text, b.top) + b.left.min(line_len(text, b.top)),
        );
        self.marks.insert(
            '>',
            line_start(text, b.bottom) + (b.right - 1).min(line_len(text, b.bottom)),
        );
        self.cursor = line_start(text, b.top) + b.left.min(line_len(text, b.top));
        self.block.to_end = false;
        self.enter(Mode::Normal, text);
    }
}

fn change_case(op: Operator, c: char) -> char {
    let flip = |s: String| s.chars().next().unwrap_or(c);
    match op {
        Operator::Lower => flip(c.to_lowercase().collect()),
        Operator::Upper => flip(c.to_uppercase().collect()),
        _ if c.is_uppercase() => flip(c.to_lowercase().collect()),
        _ => flip(c.to_uppercase().collect()),
    }
}

#[cfg(test)]
mod tests {
    use crate::Mode;
    use crate::harness::{check, run};

    const TEXT: &str = "|abcd\nefgh\nijkl\n";

    #[test]
    fn ctrl_v_selects_a_rectangle() {
        let (_, vim, buf) = run(TEXT, "l<C-v>jl");
        assert_eq!(vim.mode(), Mode::VisualBlock);
        assert_eq!(Mode::VisualBlock.label(), "V-BLOCK");
        let b = vim.block(buf.text()).expect("block");
        assert_eq!((b.top, b.bottom, b.left, b.right), (0, 1, 1, 3));
        assert_eq!(b.columns(buf.text(), 1), Some(1..3));
        assert_eq!(b.columns(buf.text(), 2), None);
        let (_, vim, _) = run(TEXT, "<C-v><C-v>");
        assert_eq!(vim.mode(), Mode::Normal);
        let (_, vim, _) = run(TEXT, "<C-v>v");
        assert_eq!(vim.mode(), Mode::Visual);
    }

    #[test]
    fn delete_and_change_case() {
        check(TEXT, "l<C-v>jld", "a|d\neh\nijkl\n");
        check(TEXT, "l<C-v>jlx", "a|d\neh\nijkl\n");
        check(TEXT, "l<C-v>jjU", "a|Bcd\neFgh\niJkl\n");
        check("|ABc\nDEf\n", "<C-v>jl~", "|abc\ndef\n");
        check(TEXT, "l<C-v>jrx", "a|xcd\nexgh\nijkl\n");
    }

    #[test]
    fn short_lines_are_skipped() {
        check("|abcd\nx\nefgh\n", "ll<C-v>jjd", "ab|d\nx\nefh\n");
    }

    #[test]
    fn dollar_reaches_every_line_end() {
        check("|ab\nabcd\na\n", "<C-v>jj$d", "|\n\n\n");
        check("a|b\nabcd\n", "<C-v>j$d", "|a\na\n");
        // A sideways motion after `$` ends it.
        let (_, vim, buf) = run("|ab\nabcd\n", "<C-v>j$h");
        assert!(!vim.block(buf.text()).expect("block").to_end);
    }

    #[test]
    fn o_and_big_o_swap_corners() {
        let (_, vim, buf) = run(TEXT, "<C-v>jlO");
        let b = vim.block(buf.text()).expect("block");
        assert_eq!((b.left, b.right), (0, 2));
        assert_eq!(vim.cursor(), 5);
        check(TEXT, "<C-v>jlod", "|cd\ngh\nijkl\n");
    }

    #[test]
    fn insert_and_append_on_every_line() {
        check(TEXT, "l<C-v>jjI-<Esc>", "a|-bcd\ne-fgh\ni-jkl\n");
        check(TEXT, "l<C-v>jjA+<Esc>", "ab|+cd\nef+gh\nij+kl\n");
        check("|ab\nabcd\n", "<C-v>j$A!<Esc>", "ab|!\nabcd!\n");
        // `A` pads a short line; `I` skips it.
        check("|abcd\nx\n", "lll<C-v>jA!<Esc>", "abcd|!\nx   !\n");
        check("|abcd\nx\n", "lll<C-v>jI!<Esc>", "abc|!d\nx\n");
        check(TEXT, "l<C-v>jlcZ<Esc>", "a|Zd\neZh\nijkl\n");
        // All of it is one undo step.
        check(TEXT, "l<C-v>jjI-<Esc>u", "a|bcd\nefgh\nijkl\n");
    }

    #[test]
    fn indent_from_the_left_edge() {
        check(TEXT, "l<C-v>j>", "a|  bcd\ne  fgh\nijkl\n");
        check("a   b\nc   d\n|", "gg0l<C-v>j<", "a| b\nc d\n");
    }

    #[test]
    fn a_block_yank_pastes_as_a_block() {
        check(TEXT, "<C-v>jly$p", "abcd|ab\nefghef\nijkl\n");
        check(TEXT, "<C-v>jlyjjP", "abcd\nefgh\n|abijkl\nef\n");
        check("|abc\nd\n", "<C-v>jy$p", "abc|a\nd  d\n");
        let (_, mut vim, _) = run(TEXT, "<C-v>jly");
        let reg = vim.register('"').expect("yanked");
        assert!(reg.block);
        assert_eq!(reg.text, "ab\nef");
    }

    #[test]
    fn put_over_a_block() {
        check("|xy\nabcd\nefgh\n", "yljl<C-v>jlp", "xy\na|xd\neh\n");
    }
}
