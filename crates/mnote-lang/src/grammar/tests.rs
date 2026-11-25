//! The loader against fixture libraries, compiled here with the system C
//! compiler (skipped, with a message, if there's none). They aren't real
//! grammars: each exports a language whose only meaningful field is its ABI
//! version, which is all the loader reads before checking it.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::native::NativeEngine;
use super::*;

/// Compiles `source` into `dir/<name>.so`; `None` without a compiler.
fn library(dir: &Path, name: &str, source: &str) -> Option<PathBuf> {
    let c = dir.join(format!("{name}.c"));
    let out = dir.join(format!("{name}.so"));
    std::fs::write(&c, source).ok()?;
    let built = Command::new("cc")
        .args(["-shared", "-fPIC", "-o"])
        .arg(&out)
        .arg(&c)
        .status()
        .ok()?;
    built.success().then_some(out)
}

/// A language struct with this ABI version and nothing else; big enough
/// for anything tree-sitter reads before using it.
fn fake(symbol: &str, abi: u32) -> String {
    format!(
        "static const unsigned int lang[1024] = {{ {abi} }};\n\
         const void *tree_sitter_{symbol}(void) {{ return lang; }}\n"
    )
}

#[test]
fn the_loader_checks_folder_symbol_and_abi() {
    let root = std::env::temp_dir().join(format!("mnote-grammars-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let grammars = root.join("grammars");
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&grammars).expect("dir");
    std::fs::create_dir_all(&elsewhere).expect("dir");
    let abi = tree_sitter::LANGUAGE_VERSION as u32;
    let Some(good) = library(&grammars, "good_fixture", &fake("good_fixture", abi)) else {
        eprintln!("no C compiler: skipping the grammar loader test");
        return;
    };
    let old = library(&grammars, "old_fixture", &fake("old_fixture", 99)).expect("compiles");
    let nosym = library(
        &grammars,
        "nosym_fixture",
        "int nothing(void) { return 0; }\n",
    )
    .expect("compiles");
    let outside =
        library(&elsewhere, "outside_fixture", &fake("outside_fixture", abi)).expect("compiles");
    let folders = vec![grammars.clone()];
    let engine = NativeEngine;

    // Found by name, in the folders only.
    assert_eq!(find("good_fixture", &folders), Some(good.clone()));
    assert_eq!(find("outside_fixture", &folders), None);
    assert_eq!(find("../elsewhere/outside_fixture", &folders), None);
    assert_eq!(find("good_fixture.so", &folders), None);

    // A good one loads and is kept.
    let language = load(&engine, "good_fixture", &good, &folders).expect("loads");
    assert_eq!(language.abi_version() as u32, abi);
    assert!(loaded_language("good_fixture").is_some());

    // The wrong ABI is refused, naming it.
    let err = load(&engine, "old_fixture", &old, &folders).expect_err("old");
    assert!(err.message.contains("ABI 99"), "{err}");
    assert!(loaded_language("old_fixture").is_none());

    // No language function.
    let err = load(&engine, "nosym_fixture", &nosym, &folders).expect_err("no symbol");
    assert!(err.message.contains("tree_sitter_nosym_fixture"), "{err}");

    // Outside the folders: refused, directly or through a link inside one.
    let err = load(&engine, "outside_fixture", &outside, &folders).expect_err("outside");
    assert!(err.message.contains("only from"), "{err}");
    #[cfg(unix)]
    {
        let link = grammars.join("linked_fixture.so");
        std::os::unix::fs::symlink(&outside, &link).expect("link");
        let err = load(&engine, "linked_fixture", &link, &folders).expect_err("linked");
        assert!(err.message.contains("only from"), "{err}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn folders_are_mnotes_then_neovims() {
    let found = folders(Some(Path::new("/c/mnote")), Some(Path::new("/d/nvim")));
    assert_eq!(
        found,
        [
            PathBuf::from("/c/mnote/grammars"),
            PathBuf::from("/d/nvim/site/parser"),
            PathBuf::from("/d/nvim/lazy/nvim-treesitter/parser"),
        ]
    );
    assert_eq!(folders(None, None), Vec::<PathBuf>::new());
}

/// By hand, against this machine's Neovim parsers: which load and which
/// ABI they have (`cargo test -p mnote-lang real_neovim_parsers --
/// --ignored --nocapture`).
#[test]
#[ignore]
fn real_neovim_parsers_load() {
    let data = PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".local/share/nvim");
    let folders = folders(None, Some(&data));
    for name in [
        "go", "lua", "c", "toml", "yaml", "html", "css", "rust", "python",
    ] {
        match find(name, &folders) {
            None => println!("{name:>8}: no library"),
            Some(path) => match load(&NativeEngine, name, &path, &folders) {
                Ok(lang) => {
                    let mut parser = tree_sitter::Parser::new();
                    let parsed =
                        parser.set_language(&lang).is_ok() && parser.parse("x", None).is_some();
                    println!(
                        "{name:>8}: ABI {} parses: {parsed} ({})",
                        lang.abi_version(),
                        path.display()
                    );
                }
                Err(err) => println!("{name:>8}: {err}"),
            },
        }
    }
}
