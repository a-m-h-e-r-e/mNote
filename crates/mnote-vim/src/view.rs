//! What depends on the window: `H` `M` `L`, `zz` `zt` `zb`, `⌃e` `⌃y`, and
//! the jump list behind `⌃o` / `⌃i`. Also `*` and `#`, which search for the
//! word under the cursor.

use mnote_core::Buffer;

use crate::motion::{MotionKind, Target};
use crate::state::{Effect, Vim};
use crate::text::{first_non_blank, last_line, line_of, line_start};

/// How the app should scroll, after `zt`, `zz`, `zb`, `⌃e` or `⌃y`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// This line at the top of the window.
    Top(usize),
    /// This line in the middle.
    Center(usize),
    /// This line at the bottom.
    Bottom(usize),
}

/// The lines on screen, as the app last reported them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct View {
    pub(crate) top: usize,
    /// The last line fully on screen; `None` until the app says.
    pub(crate) bottom: Option<usize>,
    /// Lines in a page, for Page Up/Down and `⌃f` `⌃b` `⌃d` `⌃u`.
    pub(crate) page: usize,
    pub(crate) scroll: Option<Scroll>,
}

impl Default for View {
    fn default() -> Self {
        View {
            top: 0,
            bottom: None,
            page: 20,
            scroll: None,
        }
    }
}

impl View {
    fn bottom(&self) -> usize {
        self.bottom
            .unwrap_or(self.top + self.page.saturating_sub(1))
            .max(self.top)
    }

    /// Lines on screen, at least one.
    fn height(&self) -> usize {
        self.bottom() - self.top + 1
    }
}

/// Most positions the jump list remembers.
const MAX_JUMPS: usize = 100;

/// Where jumps (`G`, `gg`, searches, `%`, marks, `:N`) started.
#[derive(Debug, Clone, Default)]
pub(crate) struct Jumps {
    list: Vec<usize>,
    /// Where `⌃o` / `⌃i` are in the list; `list.len()` when at the newest.
    at: usize,
}

impl Jumps {
    /// Remembers `pos` as the newest jump, dropping an older one on the same
    /// line (as Vim does).
    fn push(&mut self, text: &ropey::Rope, pos: usize) {
        let line = line_of(text, pos.min(text.len_chars()));
        self.list
            .retain(|&p| line_of(text, p.min(text.len_chars())) != line);
        self.list.push(pos);
        if self.list.len() > MAX_JUMPS {
            self.list.remove(0);
        }
        self.at = self.list.len();
    }

    /// Keeps the positions on their text after an edit of `range`.
    pub(crate) fn edited(&mut self, range: std::ops::Range<usize>, added: usize) {
        for pos in &mut self.list {
            if *pos >= range.end {
                *pos = *pos - range.len() + added;
            } else if *pos > range.start {
                *pos = range.start;
            }
        }
    }
}

impl Vim {
    /// Tells Vim which lines are on screen (`bottom` is the last one fully
    /// shown), for `H` `M` `L` and the scrolling commands.
    pub fn set_view(&mut self, top: usize, bottom: usize) {
        self.view.top = top;
        self.view.bottom = Some(bottom.max(top));
    }

    /// Sets how many lines Page Up/Down and `⌃f` `⌃b` move (`⌃d` `⌃u` move
    /// half).
    pub fn set_page_lines(&mut self, lines: usize) {
        self.view.page = lines.max(1);
    }

    pub(crate) fn page(&self) -> usize {
        self.view.page
    }

    /// The scroll the last key asked for, once.
    pub fn take_scroll(&mut self) -> Option<Scroll> {
        self.view.scroll.take()
    }

    /// Forgets the jump list (another note is open).
    pub fn clear_jumps(&mut self) {
        self.jumps = Jumps::default();
    }

    /// Remembers the cursor as where a jump starts: for `''` and `⌃o`.
    pub(crate) fn record_jump(&mut self, text: &ropey::Rope) {
        self.marks.insert('\'', self.cursor);
        self.jumps.push(text, self.cursor);
    }

    /// `⌃o` (back) and `⌃i` / Tab (forward) through the jump list.
    pub(crate) fn jump_list(&mut self, back: bool, buf: &mut Buffer) -> Effect {
        let n = self.take_count().unwrap_or(1);
        let text = buf.text();
        let len = self.jumps.list.len();
        let to = if back {
            if self.jumps.at >= len {
                // Leaving the newest end: remember here, so `⌃i` comes back.
                self.jumps.push(text, self.cursor);
                self.jumps.at = self.jumps.list.len() - 1;
            }
            let Some(to) = self.jumps.at.checked_sub(n) else {
                self.cancel();
                return Effect::Handled;
            };
            to
        } else {
            let to = self.jumps.at + n;
            if to >= len {
                self.cancel();
                return Effect::Handled;
            }
            to
        };
        self.jumps.at = to;
        let pos = self.jumps.list[to].min(text.len_chars());
        self.marks.insert('\'', self.cursor);
        let target = Target {
            pos,
            kind: MotionKind::Exclusive,
            keep_column: false,
        };
        self.arrive(Some(target), false, buf)
    }

    /// `H` / `M` / `L`: a line near the top, the middle or the bottom of the
    /// window (`3H` is the third line from the top).
    pub(crate) fn window_line(&mut self, which: char, buf: &mut Buffer) -> Effect {
        let count = self.take_count();
        let text = buf.text();
        let last = last_line(text);
        let (top, bottom) = (self.view.top.min(last), self.view.bottom().min(last));
        let line = match which {
            'H' => (top + count.unwrap_or(1).max(1) - 1).min(bottom),
            'L' => bottom
                .saturating_sub(count.unwrap_or(1).max(1) - 1)
                .max(top),
            _ => top + (bottom - top) / 2,
        };
        let target = Target {
            pos: first_non_blank(text, line),
            kind: MotionKind::Linewise,
            keep_column: false,
        };
        self.arrive(Some(target), true, buf)
    }

    /// `zt` `zz` `zb` (and `z⏎` `z.` `z-`, which also go to the first
    /// non-blank): scrolls the cursor's line (or line `count`) to the top,
    /// middle or bottom.
    pub(crate) fn scroll_cursor(&mut self, key: char, buf: &mut Buffer) -> Effect {
        let count = self.take_count();
        let text = buf.text();
        let last = last_line(text);
        let line = count.map_or(line_of(text, self.cursor), |c| (c.max(1) - 1).min(last));
        let height = self.view.height();
        let (scroll, top) = match key {
            't' | '\n' => (Scroll::Top(line), line),
            'z' | '.' => (Scroll::Center(line), line.saturating_sub(height / 2)),
            _ => (Scroll::Bottom(line), (line + 1).saturating_sub(height)),
        };
        if matches!(key, '\n' | '.' | '-') {
            self.cursor = first_non_blank(text, line);
        } else if count.is_some() {
            let col = self.cursor - line_start(text, line_of(text, self.cursor));
            let start = line_start(text, line);
            let end = crate::text::line_end(text, line);
            self.cursor = (start + col).min(end.saturating_sub(1).max(start));
        }
        self.move_view(top, scroll);
        self.cancel();
        Effect::Handled
    }

    /// `⌃e` / `⌃y`: scrolls `count` lines down or up, keeping the cursor on
    /// screen.
    pub(crate) fn scroll_lines(&mut self, down: bool, buf: &mut Buffer) -> Effect {
        let n = self.take_count().unwrap_or(1).max(1);
        let text = buf.text();
        let last = last_line(text);
        let top = if down {
            (self.view.top + n).min(last)
        } else {
            self.view.top.saturating_sub(n)
        };
        let height = self.view.height();
        self.move_view(top, Scroll::Top(top));
        let bottom = (top + height - 1).min(last);
        let line = line_of(text, self.cursor);
        let to = line.clamp(top, bottom);
        if to != line {
            self.cursor = first_non_blank(text, to);
        }
        self.cancel();
        Effect::Handled
    }

    fn move_view(&mut self, top: usize, scroll: Scroll) {
        let height = self.view.height();
        self.view.top = top;
        self.view.bottom = Some(top + height - 1);
        self.view.scroll = Some(scroll);
    }

    /// `*` / `#` (`whole`), `g*` / `g#`: searches forward or back for the
    /// keyword under the cursor, or the next one on the line.
    pub(crate) fn search_word(&mut self, forward: bool, whole: bool, buf: &mut Buffer) -> Effect {
        let count = self.take_count().unwrap_or(1);
        let text = buf.text();
        let line = line_of(text, self.cursor);
        let end = crate::text::line_end(text, line);
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let mut start = self.cursor;
        while start < end && !is_word(text.char(start)) {
            start += 1;
        }
        if start >= end {
            self.cancel();
            self.cmd.message = Some(crate::ex::cmdline::Message {
                text: "E348: No string under cursor".into(),
                error: true,
                line: None,
            });
            return Effect::Handled;
        }
        let first = line_start(text, line);
        while start > first && is_word(text.char(start - 1)) {
            start -= 1;
        }
        let mut stop = start;
        while stop < end && is_word(text.char(stop)) {
            stop += 1;
        }
        let word: String = text.slice(start..stop).chars().collect();
        let source = if whole { format!("\\<{word}\\>") } else { word };
        let pattern = match self.compile(&source) {
            Ok(pattern) => pattern,
            Err(_) => {
                self.cancel();
                return Effect::Handled;
            }
        };
        self.remember_search(&pattern);
        self.search.search_forward = forward;
        // The search starts from the word, not from inside it or the blanks
        // before it.
        if self.op.is_none() {
            self.cursor = start;
        }
        self.search_next(false, count, buf);
        if self.op.is_some() {
            self.cancel();
        }
        Effect::Handled
    }
}

#[cfg(test)]
mod tests {
    use super::Scroll;
    use crate::harness::{check, run};
    use crate::{Vim, parse_keys};
    use mnote_core::Buffer;

    fn lines(n: usize) -> String {
        (0..n).map(|i| format!("line {i}\n")).collect()
    }

    /// Runs `keys` with lines `top..=bottom` on screen; returns the cursor's
    /// line and the scroll asked for.
    fn on_screen(cursor_line: usize, top: usize, bottom: usize, keys: &str) -> (usize, Vim) {
        let mut buf = Buffer::new(&lines(50));
        let mut vim = Vim::new();
        vim.set_cursor(buf.text().line_to_char(cursor_line));
        vim.set_view(top, bottom);
        for key in parse_keys(keys) {
            vim.handle_key(key, &mut buf);
        }
        (buf.text().char_to_line(vim.cursor()), vim)
    }

    #[test]
    fn star_and_hash_search_the_word_under_the_cursor() {
        check("|foo bar foo\n", "*", "foo bar |foo\n");
        check("foo bar |foo\n", "#", "|foo bar foo\n");
        check("fo|o bar foo\n", "*n", "|foo bar foo\n");
        check("foo bar fo|o\n", "#N", "foo bar |foo\n");
        // Whole words only, unless `g*`.
        check("|foo food foo\n", "*", "foo food |foo\n");
        check("|foo food foo\n", "g*", "foo |food foo\n");
        // From a blank, the next word on the line.
        check("|  foo x foo\n", "*", "  foo x |foo\n");
        check("|foo x foo\n", "d*", "|foo\n");
    }

    #[test]
    fn star_on_an_empty_line_says_so() {
        let (got, vim, _) = run("|\nfoo\n", "*");
        assert_eq!(got, "|\nfoo\n");
        assert!(vim.message().is_some_and(|m| m.text.contains("E348")));
    }

    #[test]
    fn ctrl_o_and_ctrl_i_walk_the_jump_list() {
        let text = "a\nb\nc\nd\n";
        check(&format!("|{text}"), "G<C-o>", &format!("|{text}"));
        check(&format!("|{text}"), "G<C-o><C-i>", "a\nb\nc\n|d\n");
        check(&format!("|{text}"), "G<C-o><Tab>", "a\nb\nc\n|d\n");
        // Two jumps back.
        check("|a\nb\nc\nd\n", "3GG<C-o><C-o>", "|a\nb\nc\nd\n");
        check("|a\nb\nc\nd\n", "3GG<C-o>", "a\nb\n|c\nd\n");
        // Coming back to a line drops its older entry, as in Vim.
        check("|a\nb\nc\nd\n", "3Ggg<C-o><C-o>", "a\nb\n|c\nd\n");
        // Searches and `:N` are jumps too.
        check("|a\nb\nc\nd\n", "/d<CR><C-o>", "|a\nb\nc\nd\n");
        check("|a\nb\nc\nd\n", ":3<CR><C-o>", "|a\nb\nc\nd\n");
        // Nothing older: stays put.
        check("a\n|b\n", "<C-o>", "a\n|b\n");
    }

    #[test]
    fn jumps_follow_edits() {
        check("|a\nb\nc\n", "Gggix<Esc><C-o>", "xa\nb\n|c\n");
        check("x|a\nb\nc\n", "GggOnew<Esc><C-o>", "new\nxa\nb\n|c\n");
    }

    #[test]
    fn h_m_l_go_to_the_window_lines() {
        assert_eq!(on_screen(15, 10, 30, "H").0, 10);
        assert_eq!(on_screen(15, 10, 30, "3H").0, 12);
        assert_eq!(on_screen(15, 10, 30, "L").0, 30);
        assert_eq!(on_screen(15, 10, 30, "2L").0, 29);
        assert_eq!(on_screen(15, 10, 30, "M").0, 20);
        // The window ends past the text: `L` stops at the last line.
        assert_eq!(on_screen(45, 40, 80, "L").0, 49);
    }

    #[test]
    fn dl_deletes_to_the_bottom_line() {
        let mut buf = Buffer::new("a\nb\nc\nd\n");
        let mut vim = Vim::new();
        vim.set_view(0, 2);
        for key in parse_keys("dL") {
            vim.handle_key(key, &mut buf);
        }
        assert_eq!(buf.to_string(), "d\n");
    }

    #[test]
    fn z_scrolls_the_cursor_line() {
        let (line, mut vim) = on_screen(20, 10, 29, "zt");
        assert_eq!((line, vim.take_scroll()), (20, Some(Scroll::Top(20))));
        let (_, mut vim) = on_screen(20, 10, 29, "zz");
        assert_eq!(vim.take_scroll(), Some(Scroll::Center(20)));
        let (_, mut vim) = on_screen(20, 10, 29, "zb");
        assert_eq!(vim.take_scroll(), Some(Scroll::Bottom(20)));
        // Asked once.
        assert_eq!(vim.take_scroll(), None);
        // `z⏎` also goes to the first non-blank; `5zt` to line 5.
        let (line, mut vim) = on_screen(20, 10, 29, "5zt");
        assert_eq!((line, vim.take_scroll()), (4, Some(Scroll::Top(4))));
        check("  a\n  |b\n", "z<CR>", "  a\n  |b\n");
        check("  a\n|  b\n", "z<CR>", "  a\n  |b\n");
    }

    #[test]
    fn ctrl_e_and_ctrl_y_scroll_and_keep_the_cursor_on_screen() {
        let (line, mut vim) = on_screen(15, 10, 29, "<C-e>");
        assert_eq!((line, vim.take_scroll()), (15, Some(Scroll::Top(11))));
        // The cursor was on the top line: it moves down with it.
        let (line, _) = on_screen(10, 10, 29, "3<C-e>");
        assert_eq!(line, 13);
        let (line, mut vim) = on_screen(29, 10, 29, "2<C-y>");
        assert_eq!((line, vim.take_scroll()), (27, Some(Scroll::Top(8))));
        // Two in a row before the app draws: the second starts where the
        // first left the view.
        let (_, mut vim) = on_screen(15, 10, 29, "<C-e><C-e>");
        assert_eq!(vim.take_scroll(), Some(Scroll::Top(12)));
        let (_, mut vim) = on_screen(15, 0, 19, "<C-y>");
        assert_eq!(vim.take_scroll(), Some(Scroll::Top(0)));
    }
}
