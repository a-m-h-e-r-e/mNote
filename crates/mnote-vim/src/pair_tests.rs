//! `%` across strings and comments, and auto-pairs, through real keys
//! (AMI-237). `|` marks the cursor.

use mnote_core::Buffer;

use crate::{Vim, parse_keys};

fn run(language: &str, before: &str, keys: &str) -> String {
    let at = before.find('|').expect("a cursor");
    let text = before.replacen('|', "", 1);
    let mut buf = match language {
        "plain" => Buffer::new_plain(&text),
        "markdown" => Buffer::new(&text),
        _ => Buffer::with_language(&text, language),
    };
    let _ = buf.syntax_tree_now();
    let mut vim = Vim::new();
    vim.options_mut().shiftwidth = 4;
    vim.set_cursor(buf.text().byte_to_char(at));
    for key in parse_keys(keys) {
        // As in the app, typing doesn't wait for the parser.
        vim.handle_key(key, &mut buf);
    }
    let mut shown = buf.to_string();
    shown.insert(
        buf.text().char_to_byte(vim.cursor().min(buf.len_chars())),
        '|',
    );
    shown
}

#[track_caller]
fn check(language: &str, before: &str, keys: &str, after: &str) {
    assert_eq!(run(language, before, keys), after, "{keys:?} on {before:?}");
}

#[test]
fn percent_skips_brackets_in_strings_and_comments() {
    check("rust", "|f(\"(\", x);\n", "%", "f(\"(\", x|);\n");
    check(
        "rust",
        "fn f() {\n    // }\n|}\n",
        "%",
        "fn f() |{\n    // }\n}\n",
    );
    check(
        "javascript",
        "|if (a === \")\") {}\n",
        "%",
        "if (a === \")\"|) {}\n",
    );
    // Without syntax it counts every bracket, as before.
    check("plain", "|(a) (b)\n", "%", "(a|) (b)\n");
}

#[test]
fn openers_pair_and_closers_step_over() {
    check("rust", "|\n", "i(<Esc>", "|()\n");
    check("rust", "|\n", "i(x)<Esc>", "(x|)\n");
    check("rust", "|\n", "if(a[1]<Esc>", "f(a[1|])\n");
    check("rust", "|\n", "i\"s\"<Esc>", "\"s|\"\n");
    // Not before a word, nor a quote after one or inside a string.
    check("rust", "|x\n", "i(<Esc>", "|(x\n");
    check("python", "ab|\n", "a'<Esc>", "ab|'\n");
    check("python", "x = \"a|b\"\n", "i'<Esc>", "x = \"a|'b\"\n");
    // Inside a string a bracket is text, and the closing quote steps over.
    check("rust", "|\n", "ig(x, \"(\");<Esc>", "g(x, \"(\")|;\n");
    // Rust's lifetimes keep their single quote.
    check("rust", "fn f(x: &|) {}\n", "i'a<Esc>", "fn f(x: &'|a) {}\n");
}

#[test]
fn backspace_and_enter_between_a_pair() {
    check("rust", "|\n", "i(<BS><Esc>", "|\n");
    check(
        "rust",
        "fn f() |\n",
        "A{<CR>x<Esc>",
        "fn f() {\n    |x\n}\n",
    );
}

#[test]
fn counts_notes_and_the_setting() {
    check("rust", "|\n", "3i(<Esc>", "()()(|)\n");
    // Prose in a note doesn't pair; a fence in it does.
    check("markdown", "|\n", "i(<Esc>", "|(\n");
    check(
        "markdown",
        "```rust\n|\n```\n",
        "i(<Esc>",
        "```rust\n|()\n```\n",
    );
    check("rust", "|\n", ":set noap<CR>i(<Esc>", "|(\n");
}
