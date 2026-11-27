//! mNote's Vim layer: modes, motions, operators and the ex command parser.
//!
//! Pure logic with no UI or file-system access, so every behaviour can be unit
//! tested. Text is a `ropey::Rope`; positions are character offsets.
//!
//! - [`Vim`] is the mode state machine. It reads keys and edits a
//!   `mnote_core::Buffer`: motions, operators, text objects, registers, marks,
//!   undo and `.` repeat.
//! - [`ex`] runs `:` commands, and its [`ex::cmdline`] module the command
//!   line itself: `/` and `?` search and the live preview.
//! - [`Motion`] computes where a motion lands and how operators treat it.

mod anchors;
mod block;
#[cfg(test)]
mod code_object_tests;
mod comment;
mod folds;
pub use folds::{fold_chars, fold_lines};
pub mod ex;
pub mod ext;
#[cfg(test)]
mod indent_tests;
mod input;
mod insert_keys;
mod keys;
mod macros;
mod motion;
#[cfg(test)]
mod pair_tests;
mod registers;
mod state;
pub mod text;
mod textobj;
mod view;

use std::fmt;

pub use block::Block;
pub use input::{InputAction, VimInput};
pub use keys::{Key, key_notation, parse_keys};
pub use motion::{Find, Motion, MotionKind, Target};
pub use registers::{Clipboard, MemoryClipboard, Register};
pub use state::{Effect, LastChange, MapRhs, Operator, ScreenRows, Vim};
pub use textobj::Selection;
pub use view::Scroll;

/// The Vim mode an editor or text input is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
    /// `⌃v`: a rectangle of columns.
    VisualBlock,
    Command,
    /// `R`: typing overwrites the text.
    Replace,
}

/// How the caret is drawn for a mode (design decision D-001: the caret alone
/// shows the mode inside text inputs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaretShape {
    Block,
    Bar,
    /// A line under the character (REPLACE).
    Underline,
}

impl Mode {
    /// The label shown in the status line's mode pill.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
            Mode::VisualBlock => "V-BLOCK",
            Mode::Command => "COMMAND",
            Mode::Replace => "REPLACE",
        }
    }

    /// Visual, visual-line or visual-block mode.
    pub fn is_visual(self) -> bool {
        matches!(self, Mode::Visual | Mode::VisualLine | Mode::VisualBlock)
    }

    pub fn caret(self) -> CaretShape {
        match self {
            Mode::Insert | Mode::Command => CaretShape::Bar,
            Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::VisualBlock => CaretShape::Block,
            Mode::Replace => CaretShape::Underline,
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
/// Shared test harness: `|` in a string marks the cursor.
#[cfg(test)]
pub(crate) mod harness {
    use super::*;
    use mnote_core::Buffer;

    /// Runs `keys` on `before` (cursor at its `|`) and returns the text with
    /// `|` at the new cursor, plus the Vim state for further checks.
    pub fn run(before: &str, keys: &str) -> (String, Vim, Buffer) {
        let start = before.find('|').expect("cursor marker in before");
        let mut buf = Buffer::new(&before.replacen('|', "", 1));
        let mut vim = Vim::new();
        vim.set_cursor(buf.text().byte_to_char(start));
        for key in parse_keys(keys) {
            vim.handle_key(key, &mut buf);
        }
        let mut shown = buf.to_string();
        let at = buf.text().char_to_byte(vim.cursor().min(buf.len_chars()));
        shown.insert(at, '|');
        (shown, vim, buf)
    }

    /// Asserts that `keys` turn `before` into `after`, text and cursor.
    #[track_caller]
    pub fn check(before: &str, keys: &str, after: &str) {
        let (got, _, _) = run(before, keys);
        assert_eq!(got, after, "after {keys:?} on {before:?}");
    }
}

#[cfg(test)]
mod motion_tests {
    use super::harness::check;
    use super::*;
    use mnote_core::Buffer;

    #[test]
    fn edits_record_the_origin_vim_was_given() {
        use mnote_core::text::Origin;
        let mut buf = Buffer::new("abc\n");
        let mut vim = Vim::new();
        vim.handle_key(Key::Char('x'), &mut buf);
        assert_eq!(buf.last_origin(), Some(&Origin::User));
        let user = vim.set_origin(Origin::Plugin("tasks".into()));
        vim.edit_text(&mut buf, 0..0, "!");
        assert_eq!(buf.last_origin(), Some(&Origin::Plugin("tasks".into())));
        assert_eq!(vim.set_origin(user), Origin::Plugin("tasks".into()));
        vim.handle_key(Key::Char('x'), &mut buf);
        assert_eq!(buf.last_origin(), Some(&Origin::User));
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
    fn ge_g_underscore_and_sentences() {
        check("abc de|f\n", "ge", "ab|c def\n");
        check("abc de|f\n", "2ge", "|abc def\n");
        check("a.b c|d\n", "ge", "a.|b cd\n");
        check("a.b c|d\n", "gE", "a.|b cd\n");
        check("one\n\n|two\n", "ge", "one\n|\ntwo\n");
        // Inclusive at both ends, as in Vim.
        check("abc d|ef\n", "dge", "ab|f\n");
        check("|  ab  \n", "g_", "  a|b  \n");
        check("|ab  \ncd \n", "2g_", "ab  \nc|d \n");
        check("|ab  \n", "dg_", "|  \n");
        check("|a. B c\n", ")", "a. |B c\n");
        check("a. B |c\n", "(", "a. |B c\n");
        check("a. |B c\n", "(", "|a. B c\n");
        check("|Hi! Yes? (Ok.) Next\n", "3)", "Hi! Yes? (Ok.) |Next\n");
        check("|a\n\nb\n", ")", "a\n|\nb\n");
        check("|a.b c\n", ")", "a.b |c\n");
        check("|Done. Next one\n", "d)", "|Next one\n");
    }

    #[test]
    fn plus_minus_underscore_and_enter() {
        check("|a\n  b\nc\n", "+", "a\n  |b\nc\n");
        check("|a\n  b\nc\n", "<CR>", "a\n  |b\nc\n");
        check("|a\n  b\nc\n", "2+", "a\n  b\n|c\n");
        check("a\n  b\nc|c\n", "-", "a\n  |b\ncc\n");
        check("  |a\nb\n", "_", "  |a\nb\n");
        check("|a\nb\nc\n", "2_", "a\n|b\nc\n");
        check("|a\nb\nc\n", "d+", "|c\n");
    }

    #[test]
    fn tag_objects() {
        check("<a>x|y</a>\n", "dit", "<a>|</a>\n");
        check("<a>x|y</a>!\n", "dat", "|!\n");
        check("<p>a <b>|c</b> d</p>\n", "dit", "<p>a <b>|</b> d</p>\n");
        check("<p>a <b>|c</b> d</p>\n", "d2it", "<p>|</p>\n");
        check("<p>a <br/> |c</p>\n", "dit", "<p>|</p>\n");
        check(
            "<div class=\"x\">\n  |hi\n</div>\n",
            "dit",
            "<div class=\"x\">|</div>\n",
        );
        check("<a>|x\n", "dit", "<a>|x\n");
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
        let mut buf = Buffer::new("  hello\n");
        let mut vim = Vim::new();
        vim.set_cursor(4);
        vim.handle_key(Key::Char('a'), &mut buf);
        assert_eq!((vim.mode(), vim.cursor()), (Mode::Insert, 5));
        vim.handle_key(Key::Char('x'), &mut buf);
        assert_eq!(buf.to_string(), "  helxlo\n");
        vim.handle_key(Key::Esc, &mut buf);
        assert_eq!(
            (vim.mode(), vim.cursor()),
            (Mode::Normal, 5),
            "esc steps back onto the typed character"
        );
        vim.handle_key(Key::Char('I'), &mut buf);
        assert_eq!((vim.mode(), vim.cursor()), (Mode::Insert, 2));
        vim.handle_key(Key::Esc, &mut buf);
        vim.handle_key(Key::Char('A'), &mut buf);
        assert_eq!(vim.cursor(), 8);
        vim.handle_key(Key::Esc, &mut buf);
        vim.handle_key(Key::Char(':'), &mut buf);
        assert_eq!(vim.mode(), Mode::Command);
        assert_eq!(
            vim.handle_key(Key::Char('w'), &mut buf),
            Effect::Handled,
            "command mode types into the command line"
        );
        vim.handle_key(Key::Esc, &mut buf);
        assert_eq!(vim.mode(), Mode::Normal);
    }

    #[test]
    fn visual_mode_selects_from_anchor_to_cursor() {
        let mut buf = Buffer::new("one two three");
        let mut vim = Vim::new();
        vim.set_cursor(4);
        vim.handle_key(Key::Char('v'), &mut buf);
        vim.handle_key(Key::Char('e'), &mut buf);
        assert_eq!(vim.selection(), Some(4..7));
        vim.handle_key(Key::Char('o'), &mut buf);
        assert_eq!((vim.anchor(), vim.cursor()), (6, 4));
        vim.handle_key(Key::Esc, &mut buf);
        assert_eq!((vim.mode(), vim.selection()), (Mode::Normal, None));
    }

    #[test]
    fn inputs_start_in_insert_mode() {
        assert_eq!(Vim::for_input().mode(), Mode::Insert);
    }

    #[test]
    fn a_stray_key_cancels_a_half_typed_sequence() {
        check("|a-b", "fg", "|a-b");
        check("|one two", "gxw", "one |two");
        check("|one two three", "2gxw", "one |two three");
    }

    #[test]
    fn unbound_keys_are_left_to_the_app() {
        let mut buf = Buffer::new("text");
        let mut vim = Vim::new();
        assert_eq!(
            vim.handle_key(Key::Char(' '), &mut buf),
            Effect::Unhandled(Key::Char(' '))
        );
    }

    #[test]
    fn empty_text_never_panics() {
        let mut buf = Buffer::new("");
        let mut vim = Vim::new();
        for key in parse_keys("hjklwbeWBE0^$ggG%{}]][[fxtx;,vjk<esc>xXDdddwyypPu<c-r>.>><<guu~") {
            vim.handle_key(key, &mut buf);
            assert!(vim.cursor() <= buf.len_chars());
        }
    }
}

#[cfg(test)]
mod operator_tests {
    use super::harness::{check, run};
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn delete_with_motions() {
        check("one |two three", "dw", "one |three");
        check("one |two three", "d2w", "one| ");
        check("one tw|o\nthree", "dw", "one t|w\nthree");
        check("|one two", "de", "| two");
        check("one |two", "d$", "one| ");
        check("one |two", "D", "one| ");
        check("a-|b-c", "dt-", "a-|-c");
        check("a|bc", "x", "a|c");
        check("ab|c", "x", "a|b");
        check("ab|c", "X", "a|c");
        check("|abc", "fzd", "|abc");
    }

    #[test]
    fn doubled_operators_work_on_whole_lines() {
        check("one\n|two\nthree\n", "dd", "one\n|three\n");
        check("one\ntwo\n|three", "dd", "one\n|two");
        check("|a\nb\nc\n", "2dd", "|c\n");
        check("|a\nb\nc\n", "d2d", "|c\n");
        check("|a\nb\nc\n", "dj", "|c\n");
        check("one\n|two\n", "yyp", "one\ntwo\n|two\n");
        check("  |old\n", "ccnew<esc>", "  ne|w\n");
        check("|a\nb\n", ">>", "  |a\nb\n");
        check("|a\nb\n", "2>>", "  |a\n  b\n");
        check("    |a\n", "<<", "  |a\n");
        check("\t|a\n", "<<", "|a\n");
    }

    #[test]
    fn case_operators() {
        check("|Hello World", "gUiw", "|HELLO World");
        check("|Hello World", "guu", "|hello world");
        check("|Hello World", "gUU", "|HELLO WORLD");
        check("|Hello World", "gUgU", "|HELLO WORLD");
        check("|Hello World", "gu$", "|hello world");
    }

    #[test]
    fn change_word_changes_to_the_end_of_the_word() {
        check("|foo bar", "cwbaz<esc>", "ba|z bar");
        check("fo|o bar", "cwx<esc>", "fo|x bar");
        check("|foo bar baz", "c2wq<esc>", "|q baz");
        check("one |two", "Cend<esc>", "one en|d");
        check("|ab", "sX<esc>", "|Xb");
    }

    #[test]
    fn operators_take_text_objects() {
        check("say \"he|llo\" now", "ci\"bye<esc>", "say \"by|e\" now");
        check("f(a, |b)", "da(", "|f");
        check("f(a, |b)", "di(", "f(|)");
        check("one tw|o three", "daw", "one |three");
        check("a\n|b\nc\n\nd\n", "dap", "|d\n");
        check("x [1, |2] y", "ci[0<esc>", "x [|0] y");
    }

    #[test]
    fn paste_charwise_and_linewise() {
        check("|ab", "ylp", "a|ab");
        check("|ab", "ylP", "|aab");
        check("|one\ntwo", "yyjp", "one\ntwo\n|one");
        check("one\n|two\n", "yykP", "|two\none\ntwo\n");
        check("|x", "yl3p", "xxx|x");
        check("|", "p", "|");
    }

    #[test]
    fn named_numbered_and_black_hole_registers() {
        let (_, mut vim, _) = run("|one two", "\"ayiwwyiw");
        assert_eq!(vim.register('a').expect("a").text, "one");
        assert_eq!(vim.register('0').expect("0").text, "two");
        check("|one two", "\"ayiww\"ap", "one ton|ewo");
        let (_, mut vim, _) = run("|a\nb\n", "dddd");
        assert_eq!(vim.register('1').expect("1").text, "b\n");
        assert_eq!(vim.register('2').expect("2").text, "a\n");
        check("|ab", "yl\"_xp", "b|a");
        let (_, mut vim, _) = run("|one two", "\"byw\"Byw");
        assert_eq!(vim.register('b').expect("b").text, "one one ");
    }

    #[test]
    fn the_plus_register_uses_the_system_clipboard() {
        struct Shared(Rc<RefCell<Option<String>>>);
        impl Clipboard for Shared {
            fn read(&mut self) -> Option<String> {
                self.0.borrow().clone()
            }
            fn write(&mut self, text: &str) {
                *self.0.borrow_mut() = Some(text.to_owned());
            }
        }
        let system = Rc::new(RefCell::new(None));
        let mut buf = mnote_core::Buffer::new("copy me");
        let mut vim = Vim::new();
        vim.set_clipboard(Box::new(Shared(system.clone())));
        for key in parse_keys("\"+yiw") {
            vim.handle_key(key, &mut buf);
        }
        assert_eq!(system.borrow().as_deref(), Some("copy"));
        *system.borrow_mut() = Some("from elsewhere\n".into());
        for key in parse_keys("\"+p") {
            vim.handle_key(key, &mut buf);
        }
        assert_eq!(
            buf.to_string(),
            "copy me\nfrom elsewhere",
            "a trailing newline pastes as a line"
        );
    }

    #[test]
    fn undo_and_redo() {
        check("|one two", "dwu", "|one two");
        check("|one two", "dwu<c-r>", "|two");
        check("|ab", "ixy<esc>u", "|ab");
        check("|a b c", "dwdwuu", "|a b c");
        check("|a b c", "dwdw2u", "|a b c");
    }

    #[test]
    fn dot_repeats_the_last_change() {
        check("|a b c d", "dw..", "|d");
        check("|a b c d e", "dw2.", "|d e");
        check("|x\ny\nz\n", "A!<esc>j.j.", "x!\ny!\nz|!\n");
        check("|foo foo", "cwbar<esc>w.", "bar ba|r");
        check("|a\nb\n", "ddu.", "|b\n");
        check("|ab", "lx.", "|");
        check("|one two", "dwyw.", "|");
    }

    #[test]
    fn insert_counts_and_open_line() {
        check("|", "3ix<esc>", "xx|x");
        check("|  item", "onext<esc>", "  item\n  nex|t");
        check("|a", "Ob<esc>", "|b\na");
        check("|a", "2ob<esc>", "a\nb\n|b");
        check("|a", "ix<bs><bs>y<esc>", "|ya");
    }

    #[test]
    fn marks_follow_edits() {
        check("one\n|two\nthree", "majdd'a", "one\n|two");
        check("one\n|two\nthree", "makdd`a", "|two\nthree");
        check("|one\ntwo", "G''", "|one\ntwo");
        let (_, vim, _) = run("|one two", "wvey<esc>");
        assert_eq!((vim.mark('<'), vim.mark('>')), (Some(4), Some(6)));
    }

    #[test]
    fn visual_operators() {
        check("one |two three", "ved", "one | three");
        check("|a\nb\nc\n", "Vjd", "|c\n");
        check("|a\nb\n", "Vj>", "  |a\n  b\n");
        check("|Hello", "veU", "|HELLO");
        check("|one two", "yiwwviwp", "one on|e");
        check("say \"|hi\" ok", "vi\"d", "say \"|\" ok");
        let (_, mut vim, _) = run("|one two", "vey");
        assert_eq!(vim.register('"').expect("unnamed").text, "one");
    }
}

#[cfg(test)]
mod budget {
    use super::*;
    use std::time::{Duration, Instant};

    /// Keystroke budget from CLAUDE.md (8 ms to pixels): the slowest motions
    /// (which scan the whole note) and common edits stay well inside it on a
    /// 1 MB note.
    /// Run with `cargo test -p mnote-vim --release -- --ignored`.
    #[test]
    #[ignore = "timing test; run with --release -- --ignored"]
    fn whole_note_motions_on_one_megabyte_stay_within_budget() {
        let block = "## Section\n\nSome text with words and (brackets).\n\n```\n# fenced\n```\n\n";
        let mut buf = mnote_core::Buffer::new(&block.repeat(1_048_576 / block.len() + 1));
        let mut worst = Duration::ZERO;
        for keys in [
            "G",
            "[[",
            "gg",
            "]]",
            "50%",
            "}",
            "{",
            "w",
            "e",
            "b",
            "dd",
            "dap",
            "u",
            ">>",
            "yiwP",
            "ciwx<esc>",
            "x.",
        ] {
            let mut vim = Vim::new();
            vim.set_cursor(buf.len_chars() / 2);
            let start = Instant::now();
            for key in parse_keys(keys) {
                vim.handle_key(key, &mut buf);
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

#[cfg(test)]
mod editing_tests {
    use super::harness::{check, run};

    #[test]
    fn r_replaces_characters() {
        check("o|ne\n", "rx", "o|xe\n");
        check("|abcd\n", "3rx", "xx|xd\n");
        check("|ab\n", "5rx", "|ab\n");
        check("a|bc\n", "r<CR>", "a\n|c\n");
        check("|abc\n", "rxl.", "x|xc\n");
        check("o|ne\n", "rxu", "o|ne\n");
        check("|abc\n", "vlrx", "|xxc\n");
        check("|a\nb\n", "Vjrx", "|x\nx\n");
        check("o|ne\n", "r<esc>", "o|ne\n");
    }

    #[test]
    fn capital_r_overwrites_until_escape() {
        check("|abc\n", "Rxy<esc>", "x|yc\n");
        check("|ab\n", "Rxyz<esc>", "xy|z\n");
        check("|abc\n", "Rxy<bs><esc>", "|xbc\n");
        check("|ab\n", "Rxyz<bs><bs><esc>", "|xb\n");
        check("|abcd\n", "2Rx<esc>", "x|xcd\n");
        check("|abc\n", "Rxy<esc>u", "|abc\n");
        check("|abc\ndef\n", "Rxy<esc>j0.", "xyc\nx|yf\n");
        let (_, vim, _) = run("|ab\n", "R");
        assert_eq!(vim.mode(), crate::Mode::Replace);
        assert_eq!(vim.mode().caret(), crate::CaretShape::Underline);
    }

    #[test]
    fn tilde_and_g_tilde_swap_case() {
        check("|ab\n", "~", "A|b\n");
        check("|abc\n", "3~", "AB|C\n");
        check("a|B\n", "5~", "a|b\n");
        check("|ab cd\n", "g~w", "|AB cd\n");
        check("a|b Cd\n", "g~~", "|AB cD\n");
        check("|aB\n", "vl~", "|Ab\n");
        check("|ab\n", "~~u", "A|b\n");
    }

    #[test]
    fn ctrl_a_and_ctrl_x_change_numbers() {
        check("|x 5 y\n", "<C-a>", "x |6 y\n");
        check("|9\n", "<C-a>", "1|0\n");
        check("|5\n", "3<C-x>", "|2\n");
        check("|-1\n", "2<C-a>", "|1\n");
        check("|0\n", "<C-x>", "-|1\n");
        check("item 1|2 of 30\n", "<C-a>", "item 1|3 of 30\n");
        check("|no digits\n", "<C-a>", "|no digits\n");
        check("|1\n", "<C-a>.", "|3\n");
    }

    #[test]
    fn gp_gi_and_ampersand() {
        check("|ab\n", "ylgp", "aa|b\n");
        check("|ab\n", "ylgP", "a|ab\n");
        check("|a\nb\n", "yygp", "a\na\n|b\n");
        check("|a\n", "A b<esc>0gi c<esc>", "a b |c\n");
        check("|a a\na a\n", ":s/a/b/<CR>j&", "b a\n|b a\n");
        check("|a a\na a\n", ":s/a/b/g<CR>g&", "b b\n|b b\n");
    }

    #[test]
    fn linewise_visual_put_lands_on_the_put_line() {
        check("|a\nb\n", "yyjVp", "a\n|a\n");
        check("|a\nb\nc\n", "yyjVp", "a\n|a\nc\n");
    }
}
