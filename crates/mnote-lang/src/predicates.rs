//! nvim-treesitter's query predicates and directives, on top of the ones
//! tree-sitter evaluates itself (`#eq?`, `#match?`, `#any-of?` and their
//! forms, `#set!`, `#is?`). Neovim adds:
//!
//! - `#lua-match?` (and `not-`, `any-`): a Lua pattern, translated to a regex;
//! - `#vim-match?`: Vim's regex, read as tree-sitter's `#match?`;
//! - `#contains?`: the text holds one of the strings;
//! - `#has-ancestor?`, `#has-parent?`: the node's ancestors or parent have one
//!   of the types;
//! - `#offset!`: moves a capture's edges by rows and columns.
//!
//! A pattern using a predicate mNote doesn't know never matches (it's
//! disabled, and reported once); a directive it doesn't know (`#gsub!`) is
//! skipped, since directives don't decide whether a pattern matches.

use std::ops::Range;

use ropey::Rope;
use tree_sitter::{Node, Query, QueryMatch, QueryPredicateArg};

/// What one pattern of a query checks beyond tree-sitter's own predicates.
#[derive(Debug, Clone)]
enum Check {
    /// The capture's text matches (or with `negate`, doesn't); with `any`,
    /// one of the capture's nodes is enough.
    Regex {
        capture: u32,
        regex: regex::Regex,
        negate: bool,
        any: bool,
    },
    Contains {
        capture: u32,
        needles: Vec<String>,
        negate: bool,
    },
    Ancestor {
        capture: u32,
        kinds: Vec<String>,
        negate: bool,
        parent_only: bool,
    },
}

/// `#offset!`: a capture's start and end moved by (rows, columns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offset {
    pub capture: u32,
    pub start: (i64, i64),
    pub end: (i64, i64),
}

/// A query's extra checks, per pattern, and what couldn't be read.
#[derive(Debug, Clone, Default)]
pub struct Checks {
    per_pattern: Vec<Vec<Check>>,
    offsets: Vec<Vec<Offset>>,
}

impl Checks {
    /// Reads `query`'s general predicates. Patterns with an unknown
    /// predicate are disabled; returns the names it didn't know (each once).
    pub fn new(query: &mut Query) -> (Checks, Vec<String>) {
        let mut checks = Checks::default();
        let mut unknown = Vec::new();
        for pattern in 0..query.pattern_count() {
            let mut list = Vec::new();
            let mut offsets = Vec::new();
            let mut disable = false;
            for p in query.general_predicates(pattern) {
                let op = &*p.operator;
                let capture = match p.args.first() {
                    Some(QueryPredicateArg::Capture(c)) => Some(*c),
                    _ => None,
                };
                let strings: Vec<String> = p
                    .args
                    .iter()
                    .filter_map(|a| match a {
                        QueryPredicateArg::String(s) => Some(s.to_string()),
                        QueryPredicateArg::Capture(_) => None,
                    })
                    .collect();
                let negate = op.starts_with("not-");
                let any = op.starts_with("any-");
                let base = op.trim_start_matches("not-").trim_start_matches("any-");
                match (base, capture) {
                    ("lua-match?" | "vim-match?", Some(capture)) => {
                        let source = strings.first().map(String::as_str).unwrap_or("");
                        let pattern_text = if base == "lua-match?" {
                            lua_to_regex(source)
                        } else {
                            Some(source.to_owned())
                        };
                        match pattern_text.and_then(|t| regex::Regex::new(&t).ok()) {
                            Some(regex) => list.push(Check::Regex {
                                capture,
                                regex,
                                negate,
                                any,
                            }),
                            None => {
                                disable = true;
                                unknown.push(format!("{op} \"{source}\""));
                            }
                        }
                    }
                    ("contains?", Some(capture)) => list.push(Check::Contains {
                        capture,
                        needles: strings,
                        negate,
                    }),
                    ("has-ancestor?" | "has-parent?", Some(capture)) => {
                        list.push(Check::Ancestor {
                            capture,
                            kinds: strings,
                            negate,
                            parent_only: base == "has-parent?",
                        })
                    }
                    ("offset!", Some(capture)) => {
                        let n: Vec<i64> = strings.iter().filter_map(|s| s.parse().ok()).collect();
                        if let [r1, c1, r2, c2] = n[..] {
                            offsets.push(Offset {
                                capture,
                                start: (r1, c1),
                                end: (r2, c2),
                            });
                        }
                    }
                    // Directives that only change what an injection or a
                    // later step sees: nothing to filter on.
                    (d, _) if d.ends_with('!') => {
                        if !KNOWN_DIRECTIVES.contains(&d) {
                            unknown.push(format!("#{op} (skipped)"));
                        }
                    }
                    _ => {
                        disable = true;
                        unknown.push(format!("#{op}"));
                    }
                }
            }
            if disable {
                query.disable_pattern(pattern);
            }
            checks.per_pattern.push(list);
            checks.offsets.push(offsets);
        }
        unknown.sort();
        unknown.dedup();
        (checks, unknown)
    }

    /// Whether `m` passes its pattern's checks.
    pub fn passes(&self, m: &QueryMatch, rope: &Rope) -> bool {
        let Some(list) = self.per_pattern.get(m.pattern_index) else {
            return true;
        };
        list.iter().all(|check| check_one(check, m, rope))
    }

    /// The `#offset!`s of `m`'s pattern.
    pub fn offsets(&self, pattern: usize) -> &[Offset] {
        self.offsets.get(pattern).map_or(&[], Vec::as_slice)
    }
}

/// Directives Neovim uses that mNote reads elsewhere or can skip.
const KNOWN_DIRECTIVES: &[&str] = &[
    "set!",
    "offset!",
    "set-lang-from-info-string!",
    "set-lang-from-mimetype!",
    "inject-language!",
    "gsub!",
    "trim!",
    "downcase!",
];

fn nodes<'a>(m: &'a QueryMatch, capture: u32) -> impl Iterator<Item = Node<'a>> + 'a {
    m.captures()
        .iter()
        .filter(move |c| c.index == capture)
        .map(|c| c.node)
}

fn text(rope: &Rope, node: Node) -> String {
    crate::syntax::text_chunks(rope, node.byte_range())
        .map(String::from_utf8_lossy)
        .collect()
}

fn check_one(check: &Check, m: &QueryMatch, rope: &Rope) -> bool {
    match check {
        Check::Regex {
            capture,
            regex,
            negate,
            any,
        } => {
            let mut results = nodes(m, *capture).map(|n| regex.is_match(&text(rope, n)) != *negate);
            if *any {
                results.any(|r| r)
            } else {
                results.all(|r| r)
            }
        }
        Check::Contains {
            capture,
            needles,
            negate,
        } => nodes(m, *capture).all(|n| {
            let t = text(rope, n);
            needles.iter().any(|needle| t.contains(needle.as_str())) != *negate
        }),
        Check::Ancestor {
            capture,
            kinds,
            negate,
            parent_only,
        } => nodes(m, *capture).all(|n| {
            let mut found = false;
            let mut up = n.parent();
            while let Some(node) = up {
                if kinds.iter().any(|k| k == node.kind()) {
                    found = true;
                    break;
                }
                if *parent_only {
                    break;
                }
                up = node.parent();
            }
            found != *negate
        }),
    }
}

/// A capture's byte range moved by an `#offset!` (rows and columns, as
/// Neovim counts them: bytes within the line).
pub fn apply_offset(rope: &Rope, range: Range<usize>, offset: &Offset) -> Range<usize> {
    let shift = |byte: usize, (rows, cols): (i64, i64)| -> usize {
        let byte = byte.min(rope.len_bytes());
        let row = rope.byte_to_line(byte) as i64;
        let col = (byte - rope.line_to_byte(row as usize)) as i64;
        let new_row = (row + rows).clamp(0, rope.len_lines() as i64 - 1) as usize;
        let start = rope.line_to_byte(new_row);
        let line_len = rope.line(new_row).len_bytes();
        start + (col + cols).clamp(0, line_len as i64) as usize
    };
    let start = shift(range.start, offset.start);
    let end = shift(range.end, offset.end).max(start);
    start..end
}

/// A Lua pattern as a regex, or `None` for what has no regex form (`%b`,
/// `%f`, back-references).
pub fn lua_to_regex(pattern: &str) -> Option<String> {
    let class = |c: char| -> Option<&'static str> {
        Some(match c {
            'a' => "A-Za-z",
            'd' => "0-9",
            'l' => "a-z",
            'u' => "A-Z",
            's' => "\\s",
            'w' => "A-Za-z0-9",
            'x' => "0-9A-Fa-f",
            'p' => "!-/:-@\\[-`{-~",
            'c' => "\\x00-\\x1f\\x7f",
            'g' => "!-~",
            _ => return None,
        })
    };
    let negated = |c: char| -> Option<&'static str> {
        Some(match c {
            'A' => "[^A-Za-z]",
            'D' => "[^0-9]",
            'L' => "[^a-z]",
            'U' => "[^A-Z]",
            'S' => "\\S",
            'W' => "[^A-Za-z0-9]",
            'X' => "[^0-9A-Fa-f]",
            'P' => "[^!-/:-@\\[-`{-~]",
            _ => return None,
        })
    };
    let mut out = String::new();
    let mut chars = pattern.chars().peekable();
    let mut in_set = false;
    while let Some(c) = chars.next() {
        match c {
            '%' => {
                let next = chars.next()?;
                if matches!(next, 'b' | 'f') || next.is_ascii_digit() {
                    return None;
                }
                if let Some(cls) = class(next) {
                    if in_set {
                        out.push_str(cls);
                    } else {
                        out.push('[');
                        out.push_str(cls);
                        out.push(']');
                    }
                } else if let Some(neg) = negated(next) {
                    if in_set {
                        return None;
                    }
                    out.push_str(neg);
                } else {
                    out.push_str(&regex::escape(&next.to_string()));
                }
            }
            '[' if !in_set => {
                in_set = true;
                out.push('[');
                if chars.peek() == Some(&'^') {
                    chars.next();
                    out.push('^');
                }
            }
            ']' if in_set => {
                in_set = false;
                out.push(']');
            }
            // Lua's lazy `-` is `*?`; in a set it's a range.
            '-' if !in_set => out.push_str("*?"),
            '\\' | '{' | '}' | '|' if !in_set => {
                out.push('\\');
                out.push(c);
            }
            '\\' | '[' if in_set => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    (!in_set).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::{Parser, QueryCursor, StreamingIterator};

    #[test]
    fn lua_patterns_become_regexes() {
        let re = |p: &str| regex::Regex::new(&lua_to_regex(p).expect("translates")).expect("valid");
        assert!(re("^[A-Z][A-Z_0-9]*$").is_match("MAX_SIZE"));
        assert!(!re("^[A-Z][A-Z_0-9]*$").is_match("MaxSize"));
        assert!(re("^%u").is_match("Type") && !re("^%u").is_match("type"));
        assert!(re("^[%a_][%w_]*$").is_match("_foo1") && !re("^[%a_][%w_]*$").is_match("1foo"));
        assert!(re("^%s*$").is_match("   "));
        assert!(re("^%-%-%-").is_match("---"), "%- is a dash");
        assert!(re("a.-b").is_match("axxbyb"));
        assert!(re("^#!").is_match("#!/bin/sh"));
        assert!(re("%.").is_match(".") && !re("^%.$").is_match("x"));
        assert!(re("^%D+$").is_match("abc") && !re("^%D+$").is_match("a1"));
        assert_eq!(lua_to_regex("%b()"), None);
        assert_eq!(lua_to_regex("%f[%w]"), None);
        assert_eq!(lua_to_regex("[abc"), None);
    }

    fn rust_matches(query_text: &str, code: &str) -> (Vec<String>, Vec<String>) {
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let mut query = Query::new(&language, query_text).expect("valid");
        let (checks, unknown) = Checks::new(&mut query);
        let mut parser = Parser::new();
        parser.set_language(&language).expect("language");
        let tree = parser.parse(code, None).expect("parses");
        let rope = Rope::from_str(code);
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), code.as_bytes());
        let mut found = Vec::new();
        while let Some(m) = matches.next() {
            if checks.passes(m, &rope) {
                for c in m.captures() {
                    found.push(code[c.node.byte_range()].to_owned());
                }
            }
        }
        (found, unknown)
    }

    #[test]
    fn neovims_predicates_filter_matches() {
        let code = "fn main() { let MAX = 1; let small = Foo::new(); }";
        let (found, _) = rust_matches(r#"((identifier) @c (#lua-match? @c "^[A-Z]+$"))"#, code);
        assert_eq!(found, ["MAX"]);
        let (found, _) = rust_matches(r#"((identifier) @c (#not-lua-match? @c "^[A-Z]+$"))"#, code);
        assert_eq!(found, ["main", "small", "Foo", "new"]);
        let (found, _) = rust_matches(r#"((identifier) @c (#contains? @c "mal" "ai"))"#, code);
        assert_eq!(found, ["main", "small"]);
        let (found, _) = rust_matches(
            r#"((identifier) @c (#has-ancestor? @c let_declaration))"#,
            code,
        );
        assert_eq!(found, ["MAX", "small", "Foo", "new"]);
        let (found, _) = rust_matches(r#"((identifier) @c (#has-parent? @c function_item))"#, code);
        assert_eq!(found, ["main"]);
        let (found, _) = rust_matches(
            r#"((identifier) @c (#not-has-parent? @c function_item))"#,
            code,
        );
        assert_eq!(found, ["MAX", "small", "Foo", "new"]);
    }

    #[test]
    fn unknown_predicates_disable_their_pattern_and_directives_are_skipped() {
        let code = "fn main() {}";
        let (found, unknown) = rust_matches(
            r#"((identifier) @a (#teleport? @a)) ((identifier) @b (#gsub! @b "x" "y")) ((identifier) @c (#frobnicate! @c))"#,
            code,
        );
        assert_eq!(
            found,
            ["main", "main"],
            "the unknown predicate's pattern never matches"
        );
        assert_eq!(unknown, ["#frobnicate! (skipped)", "#teleport?"]);
    }

    #[test]
    fn offsets_move_a_capture() {
        let rope = Rope::from_str("ab\ncdef\n");
        let offset = Offset {
            capture: 0,
            start: (0, 1),
            end: (0, -1),
        };
        // "cdef" (bytes 3..7) becomes "de".
        assert_eq!(apply_offset(&rope, 3..7, &offset), 4..6);
        let down = Offset {
            capture: 0,
            start: (1, 0),
            end: (1, 0),
        };
        assert_eq!(apply_offset(&rope, 0..2, &down), 3..5);
    }
}
