//! Code text objects and motions (AMI-236), from `textobjects.scm`
//! captures, with nvim-treesitter-textobjects' rules: a capture made of
//! several nodes in one match (a parameter and its comma) covers them all;
//! `@x.outer` is the smallest range around the cursor, else the next one
//! after it (lookahead); `@x.inner` is looked for inside the `@x.outer`
//! around the cursor first, so the inner part of this function is chosen
//! rather than an enclosing one's.

use std::collections::BTreeMap;
use std::ops::Range;

use crate::view::SyntaxSnapshot;

/// How far around a position text objects are looked for at first, in
/// characters, before the whole document is.
const WINDOW: usize = 64 * 1024;

impl SyntaxSnapshot {
    /// Whether the language at `pos` has a text-objects query.
    pub fn has_text_objects(&self, pos: usize) -> bool {
        crate::editing::query(self.language_at(pos), "textobjects").is_some()
    }

    /// The ranges captured as `capture` (`function.outer`) by matches
    /// reaching into `range`, each match's nodes joined into one range.
    pub fn object_ranges(&self, capture: &str, range: Range<usize>) -> Vec<Range<usize>> {
        let mut joined: BTreeMap<usize, Range<usize>> = BTreeMap::new();
        for c in self.captures("textobjects", range) {
            if c.name != capture {
                continue;
            }
            joined
                .entry(c.matched)
                .and_modify(|r| {
                    r.start = r.start.min(c.range.start);
                    r.end = r.end.max(c.range.end);
                })
                .or_insert(c.range);
        }
        let mut out: Vec<Range<usize>> = joined.into_values().filter(|r| !r.is_empty()).collect();
        out.sort_by_key(|r| (r.start, r.end));
        out.dedup();
        out
    }

    /// The `object` (`function`, `class`, `parameter`, `comment`) at or
    /// after `at`: its outer or inner range. `at` is the cursor (one
    /// character) or a VISUAL selection, which an object must be bigger
    /// than, so repeating it grows to the enclosing one.
    pub fn text_object(&self, object: &str, inner: bool, at: Range<usize>) -> Option<Range<usize>> {
        let len = self.text().len_chars();
        let window = at.start.saturating_sub(WINDOW)..(at.end + WINDOW).min(len);
        let outer_name = format!("{object}.outer");
        let outers = self.object_ranges(&outer_name, window.clone());
        if !inner {
            return best(&outers, &at, true);
        }
        let inners = self.object_ranges(&format!("{object}.inner"), window);
        let Some(outer) = best(&outers, &at, false) else {
            return best(&inners, &at, true);
        };
        let within: Vec<Range<usize>> = inners
            .iter()
            .filter(|r| outer.start <= r.start && r.end <= outer.end)
            .cloned()
            .collect();
        if within.is_empty() {
            return best(&inners, &at, true);
        }
        best(&within, &at, true).or_else(|| best(&within, &(outer.start..outer.start + 1), true))
    }

    /// Where `count` jumps to the next (or previous) start, or with `end`
    /// last character, of an `object.outer` from `pos` go, as `]m` `[m`
    /// `]M` `[M`.
    pub fn object_jump(
        &self,
        object: &str,
        pos: usize,
        forward: bool,
        end: bool,
        count: usize,
    ) -> Option<usize> {
        let len = self.text().len_chars();
        let name = format!("{object}.outer");
        let mut at = pos;
        let mut moved = None;
        for _ in 0..count.max(1) {
            let points = |range: Range<usize>| -> Vec<usize> {
                let mut p: Vec<usize> = self
                    .object_ranges(&name, range)
                    .into_iter()
                    .map(|r| {
                        if end {
                            r.end.saturating_sub(1)
                        } else {
                            r.start
                        }
                    })
                    .collect();
                p.sort_unstable();
                p.dedup();
                p
            };
            let pick = |p: Vec<usize>| {
                if forward {
                    p.into_iter().find(|&x| x > at)
                } else {
                    p.into_iter().rev().find(|&x| x < at)
                }
            };
            let near = if forward {
                at..(at + WINDOW).min(len)
            } else {
                at.saturating_sub(WINDOW)..(at + 1).min(len)
            };
            let all = if forward {
                at..len
            } else {
                0..(at + 1).min(len)
            };
            let next = pick(points(near)).or_else(|| pick(points(all)))?;
            at = next;
            moved = Some(next);
        }
        moved
    }
}

/// nvim-treesitter-textobjects' `best_range_at_point`: the smallest range
/// around `at` (bigger than it, when it's a selection), the earliest on a
/// tie; else, with `lookahead`, the first one starting after it (the
/// largest of those starting there).
fn best(ranges: &[Range<usize>], at: &Range<usize>, lookahead: bool) -> Option<Range<usize>> {
    let selection = at.len() > 1;
    let around = ranges
        .iter()
        .filter(|r| r.start <= at.start && at.end <= r.end)
        .filter(|r| !selection || r.len() > at.len())
        .min_by_key(|r| (r.len(), r.start));
    if let Some(r) = around {
        return Some(r.clone());
    }
    if !lookahead {
        return None;
    }
    ranges
        .iter()
        .filter(|r| r.start > at.start)
        .min_by_key(|r| (r.start, std::cmp::Reverse(r.len())))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::Syntax;
    use ropey::Rope;

    fn snapshot(text: &str, language: &str) -> SyntaxSnapshot {
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
    }

    fn at(text: &str, needle: &str) -> Range<usize> {
        let byte = text.find(needle).expect("in the text");
        let c = text[..byte].chars().count();
        c..c + 1
    }

    fn show(text: &str, r: Option<Range<usize>>) -> String {
        let r = r.expect("an object");
        text.chars().skip(r.start).take(r.len()).collect()
    }

    const RUST: &str =
        "fn a(x: u8, y: u8) -> u8 {\n    x + y\n}\n\nstruct S {\n    f: u8,\n}\n\nfn b() {}\n";

    #[test]
    fn functions_classes_and_parameters_in_rust() {
        let s = snapshot(RUST, "rust");
        assert_eq!(
            show(RUST, s.text_object("function", false, at(RUST, "x + y"))),
            "fn a(x: u8, y: u8) -> u8 {\n    x + y\n}"
        );
        assert_eq!(
            show(RUST, s.text_object("function", true, at(RUST, "x + y"))),
            "x + y"
        );
        assert_eq!(
            show(RUST, s.text_object("class", false, at(RUST, "f: u8"))),
            "struct S {\n    f: u8,\n}"
        );
        // The comma goes with the parameter outside, not inside.
        assert_eq!(
            show(RUST, s.text_object("parameter", true, at(RUST, "y: u8"))),
            "y: u8"
        );
        assert_eq!(
            show(RUST, s.text_object("parameter", false, at(RUST, "y: u8"))),
            ", y: u8"
        );
        assert_eq!(
            show(RUST, s.text_object("parameter", false, at(RUST, "x: u8"))),
            "x: u8,"
        );
    }

    #[test]
    fn lookahead_finds_the_next_one_and_selections_grow() {
        let s = snapshot(RUST, "rust");
        // Between items: the next function.
        assert_eq!(
            show(RUST, s.text_object("function", false, at(RUST, "\n\nfn b"))),
            "fn b() {}"
        );
        // A selection that already is the inner part grows to the outer.
        let inner = s
            .text_object("function", true, at(RUST, "x + y"))
            .expect("inner");
        let grown = s.text_object("function", false, inner).expect("grows");
        assert_eq!(
            show(RUST, Some(grown)),
            "fn a(x: u8, y: u8) -> u8 {\n    x + y\n}"
        );
    }

    #[test]
    fn jumps_go_between_starts_and_ends() {
        let s = snapshot(RUST, "rust");
        let starts: Vec<usize> = [at(RUST, "fn a").start, at(RUST, "fn b").start].to_vec();
        assert_eq!(
            s.object_jump("function", 0, true, false, 1),
            Some(starts[1])
        );
        assert_eq!(
            s.object_jump("function", starts[1], false, false, 1),
            Some(starts[0])
        );
        let end_a = RUST.find("}\n\nstruct").expect("a's end");
        assert_eq!(s.object_jump("function", 0, true, true, 1), Some(end_a));
        assert_eq!(
            s.object_jump("function", 0, true, false, 5),
            None,
            "no fifth"
        );
        assert_eq!(
            s.object_jump("class", 0, true, false, 1),
            Some(at(RUST, "struct").start)
        );
    }

    #[test]
    fn other_languages_and_fences() {
        let py = "def f(a, b):\n    return a\n\nclass C:\n    pass\n";
        let s = snapshot(py, "python");
        assert_eq!(
            show(py, s.text_object("function", true, at(py, "return"))),
            "return a"
        );
        assert_eq!(
            show(py, s.text_object("class", false, at(py, "pass"))),
            "class C:\n    pass"
        );
        let js = "function f(a) {\n  return a; // hi\n}\n";
        let s = snapshot(js, "javascript");
        assert_eq!(
            show(js, s.text_object("comment", false, at(js, "hi"))),
            "// hi"
        );
        assert_eq!(
            show(js, s.text_object("parameter", true, at(js, "a)"))),
            "a"
        );
        let ts = "class A {\n  m(x: number) { return x; }\n}\n";
        let s = snapshot(ts, "typescript");
        assert_eq!(
            show(ts, s.text_object("class", true, at(ts, "m("))),
            "m(x: number) { return x; }"
        );
        let sh = "f() {\n  echo hi\n}\n";
        let s = snapshot(sh, "bash");
        assert_eq!(
            show(sh, s.text_object("function", false, at(sh, "echo"))),
            "f() {\n  echo hi\n}"
        );
        let note = "# Title\n\n```rust\nfn g() {\n    1\n}\n```\n";
        let s = snapshot(note, "markdown");
        assert_eq!(
            show(note, s.text_object("function", false, at(note, "1\n"))),
            "fn g() {\n    1\n}"
        );
        assert!(
            !snapshot("{\"a\": 1}\n", "json")
                .text_object("function", false, 1..2)
                .is_some()
        );
    }
}
