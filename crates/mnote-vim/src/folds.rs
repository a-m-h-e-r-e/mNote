//! Folds (AMI-238): which are closed, and the `z` commands that open and
//! close them. The ranges come from the syntax ([`SyntaxSnapshot::fold_ranges`]);
//! the closed ones are kept as character ranges that move with edits, as
//! marks do, and are swapped per tab by the app. Hiding closed lines on
//! screen is the view's (AMI-239).
//!
//! [`SyntaxSnapshot::fold_ranges`]: mnote_core::lang::view::SyntaxSnapshot::fold_ranges

use std::ops::{Range, RangeInclusive};

use mnote_core::Buffer;
use ropey::Rope;

use crate::text::{first_non_blank, line_end, line_of, line_start};
use crate::{Effect, Vim};

impl Vim {
    /// The closed folds as line ranges, by first line.
    pub fn closed_folds(&self, text: &Rope) -> Vec<RangeInclusive<usize>> {
        fold_lines(&self.closed, text)
    }

    /// The closed folds as they're kept (character ranges), for the app to
    /// keep per tab and in the session.
    pub fn folds(&self) -> Vec<Range<usize>> {
        self.closed.clone()
    }

    /// Puts back what [`Self::folds`] gave (another tab's, or a restored
    /// session's).
    pub fn set_folds(&mut self, folds: Vec<Range<usize>>) {
        self.closed = folds;
    }

    /// Closes the lines `first..=last` (a restored session's, by line).
    pub fn close_fold_lines(&mut self, text: &Rope, first: usize, last: usize) {
        let range = fold_chars(text, &(first..=last));
        if !self.closed.contains(&range) {
            self.closed.push(range);
        }
    }

    /// Moves closed folds with an edit of `range` replaced by `added`
    /// characters, as marks move.
    pub(crate) fn shift_folds(&mut self, range: &Range<usize>, added: usize) {
        let shift = |p: &mut usize| {
            if *p >= range.end {
                *p = *p - range.len() + added;
            } else if *p > range.start {
                *p = range.start;
            }
        };
        for r in &mut self.closed {
            shift(&mut r.start);
            shift(&mut r.end);
        }
        self.closed.retain(|r| r.start < r.end);
    }

    /// The closed folds that aren't inside another closed one: what's
    /// hidden, by first and last line.
    pub fn hidden_folds(&self, text: &Rope) -> Vec<RangeInclusive<usize>> {
        outermost(self.closed_folds(text))
    }

    /// The line `n` shown lines down (or up) from `line`, a closed fold
    /// counting as one, landing on its first line.
    pub(crate) fn fold_step(&self, text: &Rope, line: usize, n: usize, down: bool) -> usize {
        let hidden = self.hidden_folds(text);
        let around = |l: usize| hidden.iter().find(|r| r.contains(&l));
        let last = crate::text::last_line(text);
        let mut at = line;
        for _ in 0..n {
            if down {
                let end = around(at).map_or(at, |r| *r.end());
                if end >= last {
                    break;
                }
                at = end + 1;
            } else {
                let start = around(at).map_or(at, |r| *r.start());
                if start == 0 {
                    break;
                }
                at = around(start - 1).map_or(start - 1, |r| *r.start());
            }
        }
        at
    }

    /// After a key: a cursor that went into a closed fold some other way
    /// than `j`/`k` (a search, a jump, `G`), or one that changed the text
    /// in one, shows its line: the folds around it open, as Vim's
    /// `foldopen` does.
    pub(crate) fn open_folds_around_cursor(&mut self, buf: &Buffer, revision: u64) {
        let text = buf.text();
        let line = line_of(text, self.cursor);
        let hidden = self.hidden_folds(text);
        let Some(fold) = hidden.iter().find(|r| r.contains(&line)) else {
            return;
        };
        let edited = buf.revision() != revision;
        let inside = line != *fold.start();
        if edited || (inside && !self.vertical) {
            let around: Vec<RangeInclusive<usize>> = self
                .closed_folds(text)
                .into_iter()
                .filter(|r| r.contains(&line))
                .collect();
            for r in &around {
                self.open_fold(text, r);
            }
        }
    }

    /// `zc` `zo` `za` `zC` `zO` `zA` `zv` `zR` `zM` `zj` `zk`.
    pub(crate) fn fold_command(&mut self, c: char, buf: &mut Buffer) -> Effect {
        // A fold closed around the cursor keeps it (shown on its first
        // line), as in Vim.
        self.vertical = true;
        let count = self.take_count().unwrap_or(1).max(1);
        let all = buf
            .syntax_snapshot()
            .map(|s| s.fold_ranges())
            .unwrap_or_default();
        let text = buf.text().clone();
        let line = line_of(&text, self.cursor);
        let closed = self.closed_folds(&text);
        // The folds around the cursor's line, innermost first.
        let mut around: Vec<&RangeInclusive<usize>> =
            all.iter().filter(|r| r.contains(&line)).collect();
        around.sort_by_key(|r| r.end() - r.start());
        let is_closed = |r: &RangeInclusive<usize>| closed.contains(r);
        let closed_here = closed.iter().any(|r| r.contains(&line));
        let open = match c {
            'a' => Some(closed_here),
            'A' => Some(closed_here),
            'o' | 'O' | 'v' => Some(true),
            'c' | 'C' => Some(false),
            _ => None,
        };
        match (c, open) {
            ('R', _) => self.closed.clear(),
            ('M', _) => {
                if all.is_empty() {
                    return self.no_fold(buf);
                }
                self.closed = all.iter().map(|r| fold_chars(&text, r)).collect();
            }
            ('j' | 'k', _) => {
                let mut at = line;
                for _ in 0..count {
                    let next = if c == 'j' {
                        all.iter().map(|r| *r.start()).filter(|&s| s > at).min()
                    } else {
                        all.iter().map(|r| *r.end()).filter(|&e| e < at).max()
                    };
                    match next {
                        Some(l) => at = l,
                        None => break,
                    }
                }
                if at == line {
                    return self.no_fold(buf);
                }
                self.cursor = first_non_blank(&text, at);
            }
            // One level: `zo` opens the closed fold the cursor is hidden in
            // (the outermost), `count` levels down.
            ('o' | 'a', Some(true)) => {
                let mut hidden: Vec<&RangeInclusive<usize>> =
                    closed.iter().filter(|r| r.contains(&line)).collect();
                if hidden.is_empty() && around.is_empty() {
                    return self.no_fold(buf);
                }
                hidden.sort_by_key(|r| std::cmp::Reverse(*r.end() - *r.start()));
                for r in hidden.into_iter().take(count) {
                    self.open_fold(&text, r);
                }
            }
            // All levels around the cursor.
            ('O' | 'A' | 'v', Some(true)) => {
                if around.is_empty() && !closed_here {
                    return self.no_fold(buf);
                }
                let hidden: Vec<RangeInclusive<usize>> = closed
                    .iter()
                    .filter(|r| r.contains(&line))
                    .cloned()
                    .collect();
                for r in &hidden {
                    self.open_fold(&text, r);
                }
            }
            // `zc` closes the innermost open fold around the cursor (one
            // more level out when it's in a closed one), `count` of them.
            ('c' | 'a', Some(false)) => {
                let mut left = count;
                let mut any = false;
                for r in around.iter().filter(|r| !is_closed(r)) {
                    self.closed.push(fold_chars(&text, r));
                    any = true;
                    left -= 1;
                    if left == 0 {
                        break;
                    }
                }
                if !any && !closed_here {
                    return self.no_fold(buf);
                }
            }
            ('C' | 'A', Some(false)) => {
                if around.is_empty() {
                    return self.no_fold(buf);
                }
                for r in around.iter().filter(|r| !is_closed(r)) {
                    self.closed.push(fold_chars(&text, r));
                }
            }
            _ => {}
        }
        self.done(buf, false);
        Effect::Handled
    }

    fn open_fold(&mut self, text: &Rope, lines: &RangeInclusive<usize>) {
        self.closed.retain(|r| {
            let (first, last) = (
                line_of(text, r.start),
                line_of(text, r.end.saturating_sub(1).max(r.start)),
            );
            first != *lines.start() || last != *lines.end()
        });
    }

    /// Vim's E490.
    fn no_fold(&mut self, buf: &mut Buffer) -> Effect {
        self.cmd.message = Some(crate::ex::cmdline::Message {
            text: "E490: No fold found".into(),
            error: true,
            line: None,
        });
        self.done(buf, false);
        Effect::Handled
    }
}

/// The ranges not inside another (sorted by first line, outermost first).
fn outermost(sorted: Vec<RangeInclusive<usize>>) -> Vec<RangeInclusive<usize>> {
    let mut out: Vec<RangeInclusive<usize>> = Vec::new();
    for r in sorted {
        if out.last().is_some_and(|o| o.contains(r.start())) {
            continue;
        }
        out.push(r);
    }
    out
}

/// The lines folds kept as characters hide: the outermost, by first and
/// last line.
pub fn hidden_lines(folds: &[Range<usize>], text: &Rope) -> Vec<RangeInclusive<usize>> {
    outermost(fold_lines(folds, text))
}

/// Folds kept as characters ([`Vim::folds`]) as line ranges, by first
/// line.
pub fn fold_lines(folds: &[Range<usize>], text: &Rope) -> Vec<RangeInclusive<usize>> {
    let len = text.len_chars();
    let mut out: Vec<RangeInclusive<usize>> = folds
        .iter()
        .filter(|r| r.start < r.end && r.end <= len)
        .map(|r| line_of(text, r.start)..=line_of(text, r.end.saturating_sub(1).max(r.start)))
        .filter(|r| r.start() < r.end())
        .collect();
    out.sort_by_key(|r| (*r.start(), std::cmp::Reverse(*r.end())));
    out.dedup();
    out
}

/// Lines `first..=last` as the characters from the first's start to the
/// last's end, as [`Vim::folds`] keeps them.
pub fn fold_chars(text: &Rope, lines: &RangeInclusive<usize>) -> Range<usize> {
    line_start(text, *lines.start())..line_end(text, *lines.end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_keys;

    const RUST: &str = "fn a() {\n    if x {\n        y();\n    }\n}\n\nfn b() {\n    z();\n}\n";

    /// A Rust buffer after the parse, the cursor on line `line`.
    fn setup(line: usize) -> (Vim, Buffer) {
        let mut buf = Buffer::with_language(RUST, "rust");
        let _ = buf.syntax_tree_now();
        let mut vim = Vim::new();
        vim.set_cursor(line_start(buf.text(), line));
        (vim, buf)
    }

    fn keys(vim: &mut Vim, buf: &mut Buffer, keys: &str) {
        for key in parse_keys(keys) {
            vim.handle_key(key, buf);
        }
    }

    fn closed(vim: &Vim, buf: &Buffer) -> Vec<(usize, usize)> {
        vim.closed_folds(buf.text())
            .into_iter()
            .map(|r| (*r.start(), *r.end()))
            .collect()
    }

    #[test]
    fn zc_closes_out_a_level_at_a_time_and_zo_opens_back() {
        let (mut vim, mut buf) = setup(2);
        keys(&mut vim, &mut buf, "zc");
        assert_eq!(closed(&vim, &buf), [(1, 3)]);
        keys(&mut vim, &mut buf, "zc");
        assert_eq!(closed(&vim, &buf), [(0, 4), (1, 3)]);
        // `zo` opens the fold the cursor is hidden in: the outer one.
        keys(&mut vim, &mut buf, "zo");
        assert_eq!(closed(&vim, &buf), [(1, 3)]);
        keys(&mut vim, &mut buf, "za");
        assert!(closed(&vim, &buf).is_empty(), "za opens a closed one");
        keys(&mut vim, &mut buf, "za");
        assert_eq!(closed(&vim, &buf), [(1, 3)], "and closes an open one");
    }

    #[test]
    fn capital_letters_take_every_level() {
        let (mut vim, mut buf) = setup(2);
        keys(&mut vim, &mut buf, "zC");
        assert_eq!(closed(&vim, &buf), [(0, 4), (1, 3)]);
        keys(&mut vim, &mut buf, "zO");
        assert!(closed(&vim, &buf).is_empty());
        keys(&mut vim, &mut buf, "zA");
        assert_eq!(closed(&vim, &buf), [(0, 4), (1, 3)]);
        keys(&mut vim, &mut buf, "zv");
        assert!(closed(&vim, &buf).is_empty(), "zv shows the cursor's line");
        keys(&mut vim, &mut buf, "2zc");
        assert_eq!(
            closed(&vim, &buf),
            [(0, 4), (1, 3)],
            "a count closes that many"
        );
    }

    #[test]
    fn zm_zr_zj_and_zk() {
        let (mut vim, mut buf) = setup(0);
        keys(&mut vim, &mut buf, "zM");
        assert_eq!(closed(&vim, &buf), [(0, 4), (1, 3), (6, 8)]);
        keys(&mut vim, &mut buf, "zR");
        assert!(closed(&vim, &buf).is_empty());
        keys(&mut vim, &mut buf, "zj");
        assert_eq!(line_of(buf.text(), vim.cursor()), 1);
        keys(&mut vim, &mut buf, "2zj");
        assert_eq!(
            line_of(buf.text(), vim.cursor()),
            6,
            "no fold starts in between"
        );
        keys(&mut vim, &mut buf, "zk");
        assert_eq!(
            line_of(buf.text(), vim.cursor()),
            4,
            "the end of the one before"
        );
    }

    #[test]
    fn closed_folds_move_with_edits_and_round_trip_by_line() {
        let (mut vim, mut buf) = setup(7);
        keys(&mut vim, &mut buf, "zc");
        assert_eq!(closed(&vim, &buf), [(6, 8)]);
        keys(&mut vim, &mut buf, "ggOuse x;<CR>use y;<Esc>");
        assert_eq!(
            closed(&vim, &buf),
            [(8, 10)],
            "two lines above push it down"
        );
        let kept = vim.folds();
        let mut other = Vim::new();
        other.set_folds(kept.clone());
        assert_eq!(fold_lines(&kept, buf.text()), vec![(8..=10)]);
        assert_eq!(fold_chars(buf.text(), &(8..=10)), kept[0]);
    }

    #[test]
    fn without_folds_vim_says_so() {
        let mut buf = Buffer::new_plain("abc\n");
        let mut vim = Vim::new();
        keys(&mut vim, &mut buf, "zc");
        assert_eq!(
            vim.message().map(|m| m.text.as_str()),
            Some("E490: No fold found")
        );
    }

    #[test]
    fn j_and_k_step_over_a_closed_fold() {
        let (mut vim, mut buf) = setup(0);
        keys(&mut vim, &mut buf, "zc");
        assert_eq!(closed(&vim, &buf), [(0, 4)]);
        keys(&mut vim, &mut buf, "j");
        assert_eq!(line_of(buf.text(), vim.cursor()), 5, "past the fold's end");
        keys(&mut vim, &mut buf, "k");
        assert_eq!(
            line_of(buf.text(), vim.cursor()),
            0,
            "back to its first line"
        );
        assert_eq!(closed(&vim, &buf), [(0, 4)], "still closed");
        keys(&mut vim, &mut buf, "2j");
        assert_eq!(line_of(buf.text(), vim.cursor()), 6);
    }

    #[test]
    fn jumps_searches_and_edits_into_a_fold_open_it() {
        let (mut vim, mut buf) = setup(7);
        keys(&mut vim, &mut buf, "zcggG");
        assert!(closed(&vim, &buf).is_empty(), "G into it opens it");
        keys(&mut vim, &mut buf, "kzcgg/z()<CR>");
        assert!(closed(&vim, &buf).is_empty(), "a search hit in it opens it");
        assert_eq!(line_of(buf.text(), vim.cursor()), 7);
        keys(&mut vim, &mut buf, "ggzcA x<Esc>");
        assert!(
            closed(&vim, &buf).is_empty(),
            "an edit on its line opens it"
        );
        // Moving along its first line leaves it closed.
        keys(&mut vim, &mut buf, "uzcl");
        assert_eq!(closed(&vim, &buf), [(0, 4)]);
    }
}
