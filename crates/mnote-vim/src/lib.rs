//! mNote's Vim layer: modes, motions, operators and the ex command parser.
//!
//! Pure logic with no UI or file-system access, so every behaviour can be unit
//! tested. Text is a `ropey::Rope`; positions are character offsets.
//!
//! - [`Vim`] is the mode state machine: keys in, [`Action`]s out.
//! - [`Motion`] computes where a motion lands and how operators treat it.
//! - Operators, text objects and registers arrive in AMI-158, the ex parser in
//!   AMI-160.

mod keys;
mod motion;
mod state;
pub mod text;

use std::fmt;

pub use keys::{Key, parse_keys};
pub use motion::{Find, Motion, MotionKind, Target};
pub use state::{Action, Vim};

/// The Vim mode an editor or text input is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
    Command,
}

/// How the caret is drawn for a mode (design decision D-001: the caret alone
/// shows the mode inside text inputs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaretShape {
    Block,
    Bar,
}

impl Mode {
    /// The label shown in the status line's mode pill.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
            Mode::Command => "COMMAND",
        }
    }

    pub fn caret(self) -> CaretShape {
        match self {
            Mode::Insert | Mode::Command => CaretShape::Bar,
            Mode::Normal | Mode::Visual | Mode::VisualLine => CaretShape::Block,
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_starts_in_normal_mode() {
        assert_eq!(Mode::default(), Mode::Normal);
    }

    #[test]
    fn caret_follows_mode() {
        assert_eq!(Mode::Normal.caret(), CaretShape::Block);
        assert_eq!(Mode::Visual.caret(), CaretShape::Block);
        assert_eq!(Mode::Insert.caret(), CaretShape::Bar);
        assert_eq!(Mode::Command.caret(), CaretShape::Bar);
    }

    #[test]
    fn labels_match_the_status_line() {
        assert_eq!(Mode::Insert.to_string(), "INSERT");
        assert_eq!(Mode::VisualLine.label(), "V-LINE");
    }
}

/// Motion tests written the way Vim's docs read: `|` marks the cursor.
#[cfg(test)]
mod motion_tests {
    use super::*;
    use ropey::Rope;

    /// Runs `keys` in normal mode on `before` and checks the cursor lands
    /// where `after` puts its `|`. Both strings are the same text otherwise.
    #[track_caller]
    fn check(before: &str, keys: &str, after: &str) {
        let start = before.find('|').expect("cursor marker in before");
        let text = Rope::from_str(&before.replacen('|', "", 1));
        let want = after.find('|').expect("cursor marker in after");
        assert_eq!(
            before.replacen('|', "", 1),
            after.replacen('|', "", 1),
            "test texts differ"
        );
        let mut vim = Vim::new();
        vim.set_cursor(text.byte_to_char(start));
        for key in parse_keys(keys) {
            vim.handle_key(key, &text);
        }
        let got = vim.cursor();
        let shown = {
            let mut s = text.to_string();
            s.insert(text.char_to_byte(got), '|');
            s
        };
        assert_eq!(
            got,
            text.byte_to_char(want),
            "after {keys:?}:\n got: {shown:?}\nwant: {after:?}"
        );
    }

    #[test]
    fn h_and_l_stay_on_the_line() {
        check("ab|c\ndef", "l", "ab|c\ndef");
        check("abc\n|def", "h", "abc\n|def");
        check("|abcdef", "3l", "abc|def");
        check("abcd|ef", "9h", "|abcdef");
    }

    #[test]
    fn j_and_k_keep_the_column_across_short_lines() {
        check(
            "long l|ine\nab\nanother line",
            "jj",
            "long line\nab\nanothe|r line",
        );
        check(
            "long line\nab\nanothe|r line",
            "kk",
            "long l|ine\nab\nanother line",
        );
        check("a\nb\n|c\nd", "5k", "|a\nb\nc\nd");
        check("|a\nb\nc\n", "9j", "a\nb\n|c\n");
    }

    #[test]
    fn word_motions() {
        check("|foo bar.baz qux", "w", "foo |bar.baz qux");
        check("foo |bar.baz qux", "w", "foo bar|.baz qux");
        check("foo |bar.baz qux", "W", "foo bar.baz |qux");
        check("|foo bar.baz qux", "3w", "foo bar.|baz qux");
        check("foo bar.baz |qux", "b", "foo bar.|baz qux");
        check("foo bar.baz |qux", "B", "foo |bar.baz qux");
        check("|foo bar.baz", "e", "fo|o bar.baz");
        check("fo|o bar.baz", "e", "foo ba|r.baz");
        check("fo|o bar.baz", "E", "foo bar.ba|z");
    }

    #[test]
    fn words_cross_lines_and_stop_at_empty_lines() {
        check("one|\n\ntwo", "w", "one\n|\ntwo");
        check("one\n|\ntwo", "w", "one\n\n|two");
        check("one\n\n|two", "b", "one\n|\ntwo");
        check("lást wörd|s", "b", "lást |wörds");
        check("end of no|te", "w", "end of not|e");
    }

    #[test]
    fn line_motions() {
        check("  indented |text", "0", "|  indented text");
        check("  indented |text", "^", "  |indented text");
        check("|first\nsecond", "$", "firs|t\nsecond");
        check("|first\nsecond", "2$", "first\nsecon|d");
    }

    #[test]
    fn file_motions_land_on_the_first_non_blank() {
        check("one\n  two\n|three", "gg", "|one\n  two\nthree");
        check("|one\n  two\n  three\n", "G", "one\n  two\n  |three\n");
        check("|one\n  two\nthree", "2G", "one\n  |two\nthree");
        check("one\n  two\n|three", "2gg", "one\n  |two\nthree");
    }

    #[test]
    fn find_and_till_with_repeat() {
        check("|a-b-c-d", "f-", "a|-b-c-d");
        check("|a-b-c-d", "2f-", "a-b|-c-d");
        check("|a-b-c-d", "t-", "|a-b-c-d");
        check("|a-b-c-d", "t-;", "a-|b-c-d");
        check("|a-b-c-d", "f-;;", "a-b-c|-d");
        check("|a-b-c-d", "f-;,", "a|-b-c-d");
        check("a-b-c-|d", "F-", "a-b-c|-d");
        check("a-b-c-|d", "T-", "a-b-c-|d");
        check("|abc", "fz", "|abc");
        check("|ab\nz", "fz", "|ab\nz");
    }

    #[test]
    fn percent_jumps_between_brackets_or_to_a_percentage() {
        check("|call(a[1], b)", "%", "call(a[1], b|)");
        check("call(a[1], b|)", "%", "call|(a[1], b)");
        check("call(a|[1], b)", "%", "call(a[1|], b)");
        check(
            "|1\n2\n3\n4\n5\n6\n7\n8\n9\n10",
            "50%",
            "1\n2\n3\n4\n|5\n6\n7\n8\n9\n10",
        );
    }

    #[test]
    fn paragraph_motions() {
        check("|a\nb\n\nc\nd\n\ne", "}", "a\nb\n|\nc\nd\n\ne");
        check("|a\nb\n\nc\nd\n\ne", "2}", "a\nb\n\nc\nd\n|\ne");
        check("a\nb\n\nc\n|d", "{", "a\nb\n|\nc\nd");
        check("a\nb\n\nc\n|d", "}", "a\nb\n\nc\n|d");
    }

    #[test]
    fn heading_jumps_skip_code_fences() {
        let doc = "# One\ntext\n```\n# not a heading\n```\n## Two\n####### seven\n### Three\n";
        let at = |line: &str| doc.replacen(line, &format!("|{line}"), 1);
        check(&at("# One"), "]]", &at("## Two"));
        check(&at("# One"), "2]]", &at("### Three"));
        check(&at("### Three"), "[[", &at("## Two"));
        check(&at("## Two"), "[[", &at("# One"));
    }

    #[test]
    fn modes_switch_and_move_the_cursor_like_vim() {
        let text = Rope::from_str("  hello\n");
        let mut vim = Vim::new();
        vim.set_cursor(4);
        assert_eq!(
            vim.handle_key(Key::Char('a'), &text),
            Action::ModeChanged(Mode::Insert)
        );
        assert_eq!(vim.cursor(), 5);
        assert_eq!(
            vim.handle_key(Key::Char('x'), &text),
            Action::Insert("x".into())
        );
        assert_eq!(
            vim.handle_key(Key::Esc, &text),
            Action::ModeChanged(Mode::Normal)
        );
        assert_eq!(vim.cursor(), 4, "esc steps back onto the last character");
        vim.handle_key(Key::Char('I'), &text);
        assert_eq!((vim.mode(), vim.cursor()), (Mode::Insert, 2));
        vim.handle_key(Key::Esc, &text);
        vim.handle_key(Key::Char('A'), &text);
        assert_eq!(vim.cursor(), 7);
        vim.handle_key(Key::Esc, &text);
        vim.handle_key(Key::Char(':'), &text);
        assert_eq!(vim.mode(), Mode::Command);
        vim.handle_key(Key::Esc, &text);
        assert_eq!(vim.mode(), Mode::Normal);
    }

    #[test]
    fn visual_mode_selects_from_anchor_to_cursor() {
        let text = Rope::from_str("one two three");
        let mut vim = Vim::new();
        vim.set_cursor(4);
        vim.handle_key(Key::Char('v'), &text);
        for key in parse_keys("e") {
            vim.handle_key(key, &text);
        }
        assert_eq!(vim.selection(), Some(4..7));
        vim.handle_key(Key::Char('o'), &text);
        assert_eq!((vim.anchor(), vim.cursor()), (6, 4));
        vim.handle_key(Key::Esc, &text);
        assert_eq!((vim.mode(), vim.selection()), (Mode::Normal, None));
    }

    #[test]
    fn inputs_start_in_insert_mode() {
        assert_eq!(Vim::for_input().mode(), Mode::Insert);
    }

    #[test]
    fn a_stray_key_cancels_a_half_typed_sequence() {
        check("|a-b", "fg", "|a-b");
        check("|one two", "gx w", "one |two");
        check("|one two three", "2gxw", "one |two three");
    }

    #[test]
    fn empty_text_never_panics() {
        let text = Rope::new();
        let mut vim = Vim::new();
        for key in parse_keys("hjklwbeWBE0^$ggG%{}]][[fxtx;,vjk<esc>") {
            vim.handle_key(key, &text);
            assert_eq!(vim.cursor(), 0);
        }
    }
}

#[cfg(test)]
mod budget {
    use super::*;
    use ropey::Rope;
    use std::time::{Duration, Instant};

    /// Keystroke budget from CLAUDE.md (8 ms to pixels): the slowest motions,
    /// which scan the whole note, stay well inside it on a 1 MB note.
    /// Run with `cargo test -p mnote-vim --release -- --ignored`.
    #[test]
    #[ignore = "timing test; run with --release -- --ignored"]
    fn whole_note_motions_on_one_megabyte_stay_within_budget() {
        let block = "## Section\n\nSome text with words and (brackets).\n\n```\n# fenced\n```\n\n";
        let text = Rope::from_str(&block.repeat(1_048_576 / block.len() + 1));
        let mut worst = Duration::ZERO;
        for keys in ["G", "[[", "gg", "]]", "50%", "}", "{", "w", "e", "b"] {
            let mut vim = Vim::new();
            vim.set_cursor(text.len_chars() / 2);
            let start = Instant::now();
            for key in parse_keys(keys) {
                vim.handle_key(key, &text);
            }
            let took = start.elapsed();
            eprintln!("{keys:>4}: {took:?}");
            worst = worst.max(took);
        }
        assert!(
            worst < Duration::from_millis(4),
            "slowest motion took {worst:?}"
        );
    }
}
