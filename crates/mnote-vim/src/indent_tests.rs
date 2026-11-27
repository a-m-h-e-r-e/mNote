//! Indent from the syntax through real keys (AMI-234): `o`, `O`, Enter,
//! `==`, `=`, a closing bracket typed on its own line, and `]p`, per
//! built-in language. `|` marks the cursor.

use mnote_core::Buffer;

use crate::{Vim, parse_keys};

/// Runs `keys` on `before` in `language` (`plain` for none) with indents
/// `sw` wide, after the first parse, and shows the result with `|`.
fn run(language: &str, sw: usize, before: &str, keys: &str) -> String {
    let at = before.find('|').expect("a cursor");
    let text = before.replacen('|', "", 1);
    let mut buf = match language {
        "plain" => Buffer::new_plain(&text),
        "markdown" => Buffer::new(&text),
        _ => Buffer::with_language(&text, language),
    };
    let _ = buf.syntax_tree_now();
    let mut vim = Vim::new();
    vim.options_mut().shiftwidth = sw;
    vim.options_mut().tabstop = sw;
    vim.set_cursor(buf.text().byte_to_char(at));
    for key in parse_keys(keys) {
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
fn check(language: &str, sw: usize, before: &str, keys: &str, after: &str) {
    assert_eq!(
        run(language, sw, before, keys),
        after,
        "{keys:?} on {before:?}"
    );
}

#[test]
fn rust_new_lines_take_the_blocks_indent() {
    check(
        "rust",
        4,
        "fn f() {|\n}\n",
        "ox<Esc>",
        "fn f() {\n    |x\n}\n",
    );
    check(
        "rust",
        4,
        "fn f() {\n    |let x = 1;\n}\n",
        "Oy<Esc>",
        "fn f() {\n    |y\n    let x = 1;\n}\n",
    );
    check(
        "rust",
        4,
        "fn f() {\n    if x {|\n    }\n}\n",
        "oy<Esc>",
        "fn f() {\n    if x {\n        |y\n    }\n}\n",
    );
    check(
        "rust",
        4,
        "fn f() {|\n}\n",
        "a<CR>let<Esc>",
        "fn f() {\n    le|t\n}\n",
    );
}

#[test]
fn rust_equals_reindents() {
    check(
        "rust",
        4,
        "fn f() {\n|let x = 1;\n}\n",
        "==",
        "fn f() {\n    |let x = 1;\n}\n",
    );
    check(
        "rust",
        4,
        "|fn f() {\nif x {\ny\n      }\n\n}\n",
        "=G",
        "|fn f() {\n    if x {\n        y\n    }\n\n}\n",
    );
}

#[test]
fn a_closing_bracket_typed_alone_goes_back_out() {
    check(
        "rust",
        4,
        "fn f() {\n    x|\n",
        "o}<Esc>",
        "fn f() {\n    x\n|}\n",
    );
    check(
        "javascript",
        2,
        "function f() {\n  if (a) {\n    b();|\n",
        "o}<Esc>",
        "function f() {\n  if (a) {\n    b();\n  |}\n",
    );
}

#[test]
fn python_indents_after_a_colon_and_out_after_return() {
    check("python", 4, "def f():|\n", "ox<Esc>", "def f():\n    |x\n");
    check(
        "python",
        4,
        "def f():\n    return 1|\n",
        "ox<Esc>",
        "def f():\n    return 1\n|x\n",
    );
}

#[test]
fn javascript_typescript_bash_and_json() {
    check(
        "javascript",
        2,
        "function f() {|\n}\n",
        "ox<Esc>",
        "function f() {\n  |x\n}\n",
    );
    check(
        "typescript",
        2,
        "class A {|\n}\n",
        "om<Esc>",
        "class A {\n  |m\n}\n",
    );
    check(
        "bash",
        2,
        "if true; then|\nfi\n",
        "ox<Esc>",
        "if true; then\n  |x\nfi\n",
    );
    check("json", 2, "{|\n}\n", "o\"a\"<Esc>", "{\n  \"a|\"\n}\n");
}

#[test]
fn bracket_p_puts_lines_at_the_right_indent() {
    check(
        "rust",
        4,
        "fn f() {\n    a;\n}\n|b;\n  c;\n",
        "yjggj]p",
        "fn f() {\n    a;\n    |b;\n      c;\n}\nb;\n  c;\n",
    );
    // Without syntax, `]p` follows this line's indent, as in Vim.
    check(
        "plain",
        2,
        "    a\n|b\n  c\n",
        "yjgg]p",
        "    a\n    |b\n      c\nb\n  c\n",
    );
}

#[test]
fn tabs_without_expandtab_and_fences_in_notes() {
    check(
        "rust",
        4,
        "fn f() {|\n}\n",
        ":set noet<CR>ox<Esc>",
        "fn f() {\n\t|x\n}\n",
    );
    check(
        "markdown",
        4,
        "Text.\n\n```rust\nfn f() {|\n}\n```\n",
        "ox<Esc>",
        "Text.\n\n```rust\nfn f() {\n    |x\n}\n```\n",
    );
}

#[test]
fn without_an_indent_query_the_previous_line_is_kept() {
    check("plain", 2, "  abc|\n", "ox<Esc>", "  abc\n  |x\n");
    check("plain", 2, "  abc\n|xyz\n", "==", "  abc\n|xyz\n");
}
