use std::sync::Mutex;

use ropey::Rope;
use tree_sitter::{InputEdit, Query};

use super::*;

/// The size limits are shared: tests that change them run one at a time.
static LIMITS: Mutex<()> = Mutex::new(());

fn markdown(text: &str) -> (Syntax, Rope) {
    (Syntax::markdown().expect("markdown"), Rope::from_str(text))
}

/// (language, the text each range covers) per layer, in order.
fn shape(layers: &[Layer], rope: &Rope) -> Vec<(String, usize, Vec<String>)> {
    let mut out: Vec<_> = layers
        .iter()
        .map(|l| {
            let texts = l
                .ranges
                .iter()
                .map(|r| rope.byte_slice(r.clone()).to_string())
                .collect();
            (l.language.clone(), l.depth, texts)
        })
        .collect();
    out.sort();
    out
}

#[test]
fn fences_become_layers() {
    let _guard = LIMITS.lock();
    let (mut syntax, rope) = markdown(
        "# Note\n\n```rust\nfn main() {}\n```\n\ntext\n\n```py\nx = 1\n```\n\n```klingon\nqapla'\n```\n",
    );
    let got = shape(syntax.layers_now(&rope), &rope);
    assert_eq!(
        got,
        [
            ("python".into(), 1, vec!["x = 1\n".into()]),
            ("rust".into(), 1, vec!["fn main() {}\n".into()]),
        ],
        "a fence's alias names its language; one with no grammar has no layer"
    );
    let rust_at = rope.to_string().find("main").expect("there");
    let layer = syntax.layer_at(&rope, rust_at, "rust").expect("found");
    assert_eq!(layer.tree.root_node().kind(), "source_file");
    assert!(syntax.layer_at(&rope, 0, "rust").is_none());
}

#[test]
fn fences_nest() {
    let _guard = LIMITS.lock();
    let (mut syntax, rope) = markdown("````markdown\n# Inside\n\n```rust\nlet x = 1;\n```\n````\n");
    let got = shape(syntax.layers_now(&rope), &rope);
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!((got[0].0.as_str(), got[0].1), ("markdown", 1));
    assert_eq!((got[1].0.as_str(), got[1].1), ("rust", 2));
    assert_eq!(got[1].2, ["let x = 1;\n"]);
}

#[test]
fn combined_injections_are_one_layer() {
    let markdown_language: Language = tree_sitter_md::LANGUAGE.into();
    let query = Query::new(
        &markdown_language,
        "((code_fence_content) @injection.content (#set! injection.language \"rust\") (#set! injection.combined))",
    )
    .expect("valid");
    let rope = Rope::from_str("```\nlet a = 1;\n```\n\n```\nlet b = 2;\n```\n");
    let mut parser = Parser::new();
    parser.set_language(&markdown_language).expect("language");
    let tree = parser.parse(rope.to_string(), None).expect("parses");
    let found = injections(&query, None, &tree, &rope);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, "rust");
    assert_eq!(found[0].1.len(), 2, "both fences, in one layer");
}

#[test]
fn big_documents_skip_injections() {
    let _guard = LIMITS.lock();
    let before = limits();
    set_limits(Limits {
        injections: 10,
        ..before
    });
    let (mut syntax, rope) = markdown("```rust\nfn main() {}\n```\n");
    assert!(syntax.layers_now(&rope).is_empty());
    assert!(syntax.tree_now(&rope).is_some(), "the root still parses");
    set_limits(before);
}

/// Applies a replacement to `rope` and gives the edit tree-sitter needs.
fn replace(rope: &mut Rope, range: Range<usize>, text: &str) -> InputEdit {
    let start_byte = rope.char_to_byte(range.start);
    let old_end_byte = rope.char_to_byte(range.end);
    let start_position = point_at(rope, start_byte);
    let old_end_position = point_at(rope, old_end_byte);
    rope.remove(range.clone());
    rope.insert(range.start, text);
    let new_end_byte = start_byte + text.len();
    InputEdit {
        start_byte,
        old_end_byte,
        new_end_byte,
        start_position,
        old_end_position,
        new_end_position: point_at(rope, new_end_byte),
    }
}

#[test]
fn incremental_layers_equal_a_fresh_parse() {
    let _guard = LIMITS.lock();
    let mut seed: u64 = 0xdead_beef_cafe_f00d;
    let mut next = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n.max(1) as u64) as usize
    };
    let pieces = [
        "x",
        "\n",
        "```",
        "```rust\n",
        "```py\n",
        "fn a() {}",
        "# h\n",
        "é",
        "🙂",
        "'",
        "\"",
        "    ",
        "",
    ];
    for _ in 0..40 {
        let start = "Intro\n\n```rust\nfn main() { let s = \"hi\"; }\n```\n\nMiddle 🙂\n\n```py\ndef f(x):\n    return x\n```\n";
        let (mut syntax, mut rope) = markdown(start);
        syntax.layers_now(&rope);
        for _ in 0..15 {
            let len = rope.len_chars();
            let a = next(len + 1);
            let b = (a + next(4)).min(len);
            let piece = pieces[next(pieces.len())];
            let edit = replace(&mut rope, a..b, piece);
            syntax.edit(&edit);
            // Sometimes let the worker catch up between edits.
            if next(3) == 0 {
                syntax.layers_now(&rope);
            }
        }
        let incremental = shape(syntax.layers_now(&rope), &rope);
        let root = syntax.tree_now(&rope).map(|t| t.root_node().to_sexp());
        let sexps: Vec<String> = {
            let mut v: Vec<_> = syntax
                .layers_now(&rope)
                .iter()
                .map(|l| format!("{} {}", l.language, l.tree.root_node().to_sexp()))
                .collect();
            v.sort();
            v
        };
        let (mut fresh, _) = markdown("");
        let fresh_shape = shape(fresh.layers_now(&rope), &rope);
        let fresh_root = fresh.tree_now(&rope).map(|t| t.root_node().to_sexp());
        let fresh_sexps: Vec<String> = {
            let mut v: Vec<_> = fresh
                .layers_now(&rope)
                .iter()
                .map(|l| format!("{} {}", l.language, l.tree.root_node().to_sexp()))
                .collect();
            v.sort();
            v
        };
        let text = rope.to_string();
        assert_eq!(root, fresh_root, "root tree, in {text:?}");
        assert_eq!(incremental, fresh_shape, "layers, in {text:?}");
        assert_eq!(sexps, fresh_sexps, "layer trees, in {text:?}");
    }
}
