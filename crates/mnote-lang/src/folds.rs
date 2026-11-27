//! Fold ranges (AMI-238): the lines `folds.scm` captures as `@fold`, in
//! code and in notes (sections under headings, code blocks, nested lists),
//! fences in notes with their own language's.
//!
//! As Neovim's `foldexpr`: a fold covers its node's first line to its last;
//! a node ending at the start of a line stops on the line before; trailing
//! blank lines aren't part of it (the queries' `#trim!`); a fold must span
//! at least two lines.

use std::ops::RangeInclusive;

use crate::view::SyntaxSnapshot;

impl SyntaxSnapshot {
    /// Every fold, as line ranges, ordered by first line and then outermost
    /// first.
    pub fn fold_ranges(&self) -> Vec<RangeInclusive<usize>> {
        let rope = self.text();
        let len = rope.len_chars();
        let mut out: Vec<RangeInclusive<usize>> = self
            .captures("folds", 0..len)
            .into_iter()
            .filter(|c| c.name == "fold" && !c.range.is_empty())
            .filter_map(|c| {
                let first = rope.char_to_line(c.range.start);
                let end = c.range.end.min(len);
                let mut last = rope.char_to_line(end);
                // Ending where a line starts: the fold stops on the line before.
                if last > first && end == rope.line_to_char(last) {
                    last -= 1;
                }
                while last > first && rope.line(last).chars().all(char::is_whitespace) {
                    last -= 1;
                }
                (last > first).then_some(first..=last)
            })
            .collect();
        out.sort_by_key(|r| (*r.start(), std::cmp::Reverse(*r.end())));
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests {
    use crate::syntax::Syntax;
    use crate::view::SyntaxSnapshot;
    use ropey::Rope;

    fn folds(text: &str, language: &str) -> Vec<(usize, usize)> {
        let rope = Rope::from_str(text);
        let mut syntax = if language == "markdown" {
            Syntax::markdown()
        } else {
            let grammar = crate::code::language(language).expect("a built-in grammar");
            Syntax::with_injections(&grammar, Some(language))
        }
        .expect("starts parsing");
        let root = syntax.tree_now(&rope).expect("parses").clone();
        let layers = syntax.layers_now(&rope).to_vec();
        SyntaxSnapshot::new(rope, language, root, &layers)
            .fold_ranges()
            .into_iter()
            .map(|r| (*r.start(), *r.end()))
            .collect()
    }

    #[test]
    fn code_folds_per_language() {
        let rust = "fn a() {\n    if x {\n        y();\n    }\n}\n\nstruct S;\n";
        assert_eq!(folds(rust, "rust"), [(0, 4), (1, 3)]);
        let py = "def f():\n    x = 1\n    return x\n\nclass C:\n    pass\n";
        assert_eq!(folds(py, "python"), [(0, 2), (4, 5)]);
        let js = "function f() {\n  return 1;\n}\n";
        assert_eq!(folds(js, "javascript"), [(0, 2)]);
        let ts = "interface A {\n  x: number;\n}\n";
        assert_eq!(folds(ts, "typescript"), [(0, 2)]);
        let sh = "f() {\n  echo hi\n}\n";
        assert_eq!(folds(sh, "bash"), [(0, 2)]);
        let json = "{\n  \"a\": [\n    1\n  ]\n}\n";
        assert_eq!(folds(json, "json"), [(0, 4), (1, 3)]);
    }

    #[test]
    fn notes_fold_sections_and_fences() {
        let note =
            "# One\n\ntext\n\n## Two\n\n```rust\nfn f() {\n    1\n}\n```\n\n# Three\n\nend\n";
        let found = folds(note, "markdown");
        // Each section to its last non-blank line, the fence, and the
        // function inside the fence.
        assert!(found.contains(&(0, 10)), "{found:?}");
        assert!(found.contains(&(4, 10)), "{found:?}");
        assert!(found.contains(&(6, 10)), "{found:?}");
        assert!(found.contains(&(7, 9)), "{found:?}");
        assert!(found.contains(&(12, 14)), "{found:?}");
    }
}
