//! Code text objects and motions through real keys (AMI-236). `|` marks
//! the cursor.

use mnote_core::Buffer;

use crate::{Vim, parse_keys};

fn run(language: &str, before: &str, keys: &str) -> (String, Option<String>) {
    let at = before.find('|').expect("a cursor");
    let text = before.replacen('|', "", 1);
    let mut buf = match language {
        "plain" => Buffer::new_plain(&text),
        "markdown" => Buffer::new(&text),
        _ => Buffer::with_language(&text, language),
    };
    let _ = buf.syntax_tree_now();
    let mut vim = Vim::new();
    vim.set_cursor(buf.text().byte_to_char(at));
    for key in parse_keys(keys) {
        vim.handle_key(key, &mut buf);
    }
    let mut shown = buf.to_string();
    shown.insert(
        buf.text().char_to_byte(vim.cursor().min(buf.len_chars())),
        '|',
    );
    (shown, vim.message().map(|m| m.text.clone()))
}

#[track_caller]
fn check(language: &str, before: &str, keys: &str, after: &str) {
    assert_eq!(
        run(language, before, keys).0,
        after,
        "{keys:?} on {before:?}"
    );
}

#[test]
fn rust_objects_with_operators() {
    check(
        "rust",
        "fn a() {\n    |x\n}\n\nfn b() {}\n",
        "daf",
        "|\nfn b() {}\n",
    );
    check(
        "rust",
        "fn a() {\n    |x + 1\n}\n",
        "ciflet y = 2<Esc>",
        "fn a() {\n    let y = |2\n}\n",
    );
    check(
        "rust",
        "fn a(x: u8, |y: u8) {}\n",
        "ciaz<Esc>",
        "fn a(x: u8, |z) {}\n",
    );
    check(
        "rust",
        "fn a(x: u8, |y: u8) {}\n",
        "daa",
        "fn a(x: u8|) {}\n",
    );
    check(
        "rust",
        "struct S {\n    |f: u8,\n}\nfn g() {}\n",
        "dac",
        "|fn g() {}\n",
    );
}

#[test]
fn visual_grows_to_the_enclosing_object() {
    check(
        "rust",
        "impl S {\n    fn a() {\n        |x\n    }\n}\n",
        "vifafd",
        "impl S {\n|}\n",
    );
}

#[test]
fn function_and_class_motions_with_counts() {
    let text = "fn a() {}\n|\nfn b() {}\nfn c() {}\n";
    check("rust", text, "]m", "fn a() {}\n\n|fn b() {}\nfn c() {}\n");
    check("rust", text, "2]m", "fn a() {}\n\nfn b() {}\n|fn c() {}\n");
    check("rust", text, "[m", "|fn a() {}\n\nfn b() {}\nfn c() {}\n");
    check(
        "rust",
        "|fn a() {\n}\nfn b() {}\n",
        "]M",
        "fn a() {\n|}\nfn b() {}\n",
    );
    // A jump: `''` comes back.
    check("rust", text, "]m''", "fn a() {}\n|\nfn b() {}\nfn c() {}\n");
    check(
        "rust",
        "struct A;\n|fn f() {}\nstruct B {}\n",
        "]]",
        "struct A;\nfn f() {}\n|struct B {}\n",
    );
    // In a note's prose `]]` still goes to the next heading.
    check(
        "markdown",
        "|Some text.\n\n# Heading\n",
        "]]",
        "Some text.\n\n|# Heading\n",
    );
}

#[test]
fn python_javascript_and_fences() {
    check(
        "python",
        "def f():\n    |return 1\n\nx = 2\n",
        "daf",
        "|\nx = 2\n",
    );
    check("javascript", "f(); // |hi\n", "da/", "f();| \n");
    check(
        "markdown",
        "Text.\n\n```rust\nfn g() {\n    |1\n}\n```\n",
        "daf",
        "Text.\n\n```rust\n|```\n",
    );
}

#[test]
fn without_text_objects_the_keys_say_why() {
    let (out, message) = run("plain", "|abc\n", "daf");
    assert_eq!(out, "|abc\n");
    assert_eq!(
        message.as_deref(),
        Some("plain text has no code text objects")
    );
    let (_, message) = run("plain", "|abc\n", "]m");
    assert_eq!(
        message.as_deref(),
        Some("plain text has no code text objects")
    );
}
