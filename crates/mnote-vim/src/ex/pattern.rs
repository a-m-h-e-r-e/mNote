//! Vim search patterns and `:s` replacements, on top of the `regex` crate.
//!
//! Vim's "magic" syntax is translated to Rust regex syntax:
//!
//! - magic (the default): `. * [] ^ $` are special; `\( \) \| \+ \? \= \{n,m}`
//!   need a backslash;
//! - very magic (`\v`): `( ) | + ? { } = < >` are special without one;
//! - `\<` `\>` word boundaries, `\s \S \d \D \w \W \a \A \l \L \u \U \x \X
//!   \h \H`, `\n \t \e \r`;
//! - `\c` ignores case, `\C` matches case, else `ignorecase`/`smartcase`.
//!
//! Not supported (the `regex` crate has no backtracking): back-references
//! `\1` in a pattern, `\zs`/`\ze`, and lookaround. They give an error rather
//! than a wrong match.

use regex::{Captures, Regex, RegexBuilder};

/// A compiled Vim pattern.
#[derive(Debug, Clone)]
pub struct Pattern {
    /// The pattern as typed.
    pub source: String,
    regex: Regex,
}

/// Why a pattern couldn't be used. Shown as Vim's `E383`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternError(pub String);

impl Pattern {
    /// Compiles a Vim pattern. `ignorecase` and `smartcase` are the options of
    /// the same names; `\c` and `\C` in the pattern override them.
    pub fn new(source: &str, ignorecase: bool, smartcase: bool) -> Result<Self, PatternError> {
        let (translated, case) = translate(source)?;
        let ignore = match case {
            Some(ignore) => ignore,
            None => ignorecase && !(smartcase && has_uppercase(source)),
        };
        let regex = RegexBuilder::new(&translated)
            .case_insensitive(ignore)
            .build()
            .map_err(|e| PatternError(first_line(&e.to_string())))?;
        Ok(Pattern {
            source: source.to_owned(),
            regex,
        })
    }

    pub fn regex(&self) -> &Regex {
        &self.regex
    }

    pub fn is_match(&self, text: &str) -> bool {
        self.regex.is_match(text)
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(s)
        .trim()
        .to_owned()
}

/// Uppercase letters outside escapes, for `smartcase`.
fn has_uppercase(source: &str) -> bool {
    let mut chars = source.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            chars.next();
        } else if c.is_uppercase() {
            return true;
        }
    }
    false
}

/// Translates a Vim pattern to Rust regex syntax. Returns the translation and
/// the case override from `\c` (Some(true)) or `\C` (Some(false)).
fn translate(source: &str) -> Result<(String, Option<bool>), PatternError> {
    let mut out = String::with_capacity(source.len() + 8);
    let mut very_magic = false;
    let mut case = None;
    let mut chars = source.chars().peekable();
    let lit = |out: &mut String, c: char| out.push_str(&regex::escape(&c.to_string()));

    while let Some(c) = chars.next() {
        if c == '\\' {
            let Some(e) = chars.next() else {
                return Err(PatternError("trailing backslash".into()));
            };
            match e {
                'v' => very_magic = true,
                'm' | 'M' | 'V' => very_magic = false,
                'c' => case = Some(true),
                'C' => case = Some(false),
                '<' | '>' if !very_magic => out.push_str(r"\b"),
                '{' if !very_magic => counted(&mut chars, &mut out)?,
                '(' | ')' | '|' | '+' | '?' if !very_magic => out.push(e),
                '=' if !very_magic => out.push('?'),
                '}' => out.push('}'),
                's' => out.push_str(r"[ \t]"),
                'S' => out.push_str(r"[^ \t]"),
                'd' => out.push_str("[0-9]"),
                'D' => out.push_str("[^0-9]"),
                'w' => out.push_str("[0-9A-Za-z_]"),
                'W' => out.push_str("[^0-9A-Za-z_]"),
                'a' => out.push_str("[A-Za-z]"),
                'A' => out.push_str("[^A-Za-z]"),
                'l' => out.push_str("[a-z]"),
                'L' => out.push_str("[^a-z]"),
                'u' => out.push_str("[A-Z]"),
                'U' => out.push_str("[^A-Z]"),
                'x' => out.push_str("[0-9A-Fa-f]"),
                'X' => out.push_str("[^0-9A-Fa-f]"),
                'h' => out.push_str("[A-Za-z_]"),
                'H' => out.push_str("[^A-Za-z_]"),
                'n' => out.push_str(r"\n"),
                't' => out.push_str(r"\t"),
                'e' => out.push_str(r"\x1b"),
                'r' => out.push_str(r"\r"),
                '1'..='9' => {
                    return Err(PatternError(
                        "back-references like \\1 aren't supported in patterns".into(),
                    ));
                }
                'z' => {
                    return Err(PatternError("\\zs and \\ze aren't supported".into()));
                }
                other => lit(&mut out, other),
            }
            continue;
        }
        match c {
            '.' | '*' | '^' | '$' => out.push(c),
            '[' => copy_class(&mut chars, &mut out)?,
            '{' if very_magic => counted(&mut chars, &mut out)?,
            '(' | ')' | '|' | '+' | '?' if very_magic => out.push(c),
            '=' if very_magic => out.push('?'),
            '<' | '>' if very_magic => out.push_str(r"\b"),
            other => lit(&mut out, other),
        }
    }
    Ok((out, case))
}

/// Translates the rest of a `\{n,m}` count (after the `{`), which ends at
/// `}` or `\}`. `\{-n,m}` is the lazy form; an empty count means `*`.
fn counted(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    out: &mut String,
) -> Result<(), PatternError> {
    let mut body = String::new();
    loop {
        match chars.next() {
            Some('}') => break,
            Some('\\') if chars.peek() == Some(&'}') => {
                chars.next();
                break;
            }
            Some(c) if c.is_ascii_digit() || c == ',' || c == '-' => body.push(c),
            _ => return Err(PatternError("unclosed or invalid \\{n,m} count".into())),
        }
    }
    let lazy = body.starts_with('-');
    let range = body.trim_start_matches('-');
    if range.is_empty() || range == "," {
        out.push('*');
    } else if let Some(max) = range.strip_prefix(',') {
        out.push_str(&format!("{{0,{max}}}"));
    } else {
        out.push_str(&format!("{{{range}}}"));
    }
    if lazy {
        out.push('?');
    }
    Ok(())
}

/// Copies a `[...]` character class, which Vim and Rust write the same way
/// apart from escaping. A `[` without a closing `]` is a literal bracket.
fn copy_class(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    out: &mut String,
) -> Result<(), PatternError> {
    let rest: String = chars.clone().collect();
    let mut class = String::from("[");
    let mut it = rest.chars().enumerate().peekable();
    let mut consumed = None;
    // A leading `^` and a `]` right after it (or at the start) are literal.
    if let Some((_, '^')) = it.peek() {
        class.push('^');
        it.next();
    }
    if let Some((_, ']')) = it.peek() {
        class.push_str(r"\]");
        it.next();
    }
    while let Some((i, c)) = it.next() {
        match c {
            ']' => {
                class.push(']');
                consumed = Some(i + 1);
                break;
            }
            '\\' => match it.next() {
                Some((_, e)) => match e {
                    'n' => class.push_str(r"\n"),
                    't' => class.push_str(r"\t"),
                    'e' => class.push_str(r"\x1b"),
                    ']' | '\\' | '^' | '-' | '[' => {
                        class.push('\\');
                        class.push(e);
                    }
                    other => {
                        class.push('\\');
                        class.push('\\');
                        class.push(other);
                    }
                },
                None => return Err(PatternError("trailing backslash".into())),
            },
            '[' if !rest[rest.char_indices().nth(i).map_or(0, |(b, _)| b)..].starts_with("[:") => {
                class.push_str(r"\[");
            }
            other => class.push(other),
        }
    }
    match consumed {
        Some(n) => {
            for _ in 0..n {
                chars.next();
            }
            out.push_str(&class);
        }
        None => out.push_str(r"\["),
    }
    Ok(())
}

/// A compiled `:s` replacement string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    pieces: Vec<Piece>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Text(String),
    /// `&` or `\0`..`\9`.
    Group(usize),
    /// `\u` `\l`: change the case of the next character.
    NextUpper,
    NextLower,
    /// `\U` `\L` until `\E`.
    AllUpper,
    AllLower,
    EndCase,
}

impl Replacement {
    /// Parses Vim replacement syntax: `&`, `\0`..`\9`, `\&`, `\r` and `\n`
    /// (newline), `\t`, `\\`, `\u \l \U \L \E \e`, and `~` for the previous
    /// replacement (`previous`).
    pub fn new(source: &str, previous: Option<&str>) -> Self {
        let mut pieces = Vec::new();
        let mut text = String::new();
        let mut chars = source.chars();
        let flush = |text: &mut String, pieces: &mut Vec<Piece>| {
            if !text.is_empty() {
                pieces.push(Piece::Text(std::mem::take(text)));
            }
        };
        while let Some(c) = chars.next() {
            match c {
                '&' => {
                    flush(&mut text, &mut pieces);
                    pieces.push(Piece::Group(0));
                }
                '~' => text.push_str(previous.unwrap_or_default()),
                '\\' => match chars.next() {
                    Some(d @ '0'..='9') => {
                        flush(&mut text, &mut pieces);
                        pieces.push(Piece::Group(d as usize - '0' as usize));
                    }
                    Some('r' | 'n') => text.push('\n'),
                    Some('t') => text.push('\t'),
                    Some(m @ ('u' | 'l' | 'U' | 'L' | 'E' | 'e')) => {
                        flush(&mut text, &mut pieces);
                        pieces.push(match m {
                            'u' => Piece::NextUpper,
                            'l' => Piece::NextLower,
                            'U' => Piece::AllUpper,
                            'L' => Piece::AllLower,
                            _ => Piece::EndCase,
                        });
                    }
                    Some(other) => text.push(other),
                    None => text.push('\\'),
                },
                other => text.push(other),
            }
        }
        flush(&mut text, &mut pieces);
        Replacement { pieces }
    }

    /// Whether the replacement refers to capture groups (`\\1`..`\\9`). If
    /// not, matching can skip capturing, which is much faster.
    pub fn uses_groups(&self) -> bool {
        self.pieces
            .iter()
            .any(|p| matches!(p, Piece::Group(n) if *n > 0))
    }

    /// The replacement text for one match.
    pub fn expand(&self, caps: &Captures<'_>) -> String {
        self.expand_by(|n| caps.get(n).map_or("", |m| m.as_str()))
    }

    /// The replacement for a match when no groups are used (see
    /// [`Self::uses_groups`]).
    pub fn expand_match(&self, matched: &str) -> String {
        self.expand_by(|n| if n == 0 { matched } else { "" })
    }

    fn expand_by<'a>(&self, group: impl Fn(usize) -> &'a str) -> String {
        let mut out = String::new();
        let mut next: Option<bool> = None; // Some(true) upper, Some(false) lower
        let mut all: Option<bool> = None;
        let push = |out: &mut String, s: &str, next: &mut Option<bool>, all: Option<bool>| {
            if next.is_none() && all.is_none() {
                out.push_str(s);
                return;
            }
            for c in s.chars() {
                match (next.take(), all) {
                    (Some(true), _) | (None, Some(true)) => out.extend(c.to_uppercase()),
                    (Some(false), _) | (None, Some(false)) => out.extend(c.to_lowercase()),
                    (None, None) => out.push(c),
                }
            }
        };
        for piece in &self.pieces {
            match piece {
                Piece::Text(t) => push(&mut out, t, &mut next, all),
                Piece::Group(n) => push(&mut out, group(*n), &mut next, all),
                Piece::NextUpper => next = Some(true),
                Piece::NextLower => next = Some(false),
                Piece::AllUpper => all = Some(true),
                Piece::AllLower => all = Some(false),
                Piece::EndCase => all = None,
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, text: &str) -> Vec<String> {
        let p = Pattern::new(pattern, false, false).expect("pattern compiles");
        p.regex()
            .find_iter(text)
            .map(|m| m.as_str().to_owned())
            .collect()
    }

    #[test]
    fn magic_syntax() {
        assert_eq!(matches(r"a\+", "caaat"), ["aaa"]);
        assert_eq!(matches(r"colou\=r", "color colour"), ["color", "colour"]);
        assert_eq!(matches(r"\(ab\)\{2}", "ababab"), ["abab"]);
        assert_eq!(matches(r"foo\|bar", "foo bar"), ["foo", "bar"]);
        assert_eq!(matches(r"\<is\>", "this is"), ["is"]);
        assert_eq!(
            matches(r"(x)", "f(x)"),
            ["(x)"],
            "parens are literal in magic"
        );
        assert_eq!(matches(r"a.c", "abc a.c"), ["abc", "a.c"]);
        assert_eq!(matches(r"\d\d", "a12b"), ["12"]);
        assert_eq!(matches(r"[a-c]\+", "xabcx"), ["abc"]);
        assert_eq!(matches(r"[]x]", "a]x"), ["]", "x"]);
        assert_eq!(matches(r"[", "a[b"), ["["], "unclosed bracket is literal");
        assert_eq!(matches(r"x\{2,}", "x xx xxx"), ["xx", "xxx"]);
        assert_eq!(matches(r"x\{,2}y", "xxxy"), ["xxy"]);
        assert_eq!(matches(r"<.\{-}>", "<a><b>"), ["<a>", "<b>"], "lazy count");
        assert_eq!(matches(r"\vx{2}", "xxx"), ["xx"]);
    }

    #[test]
    fn very_magic_syntax() {
        assert_eq!(matches(r"\v(ab)+", "ababx"), ["abab"]);
        assert_eq!(matches(r"\vfoo|bar", "foo bar"), ["foo", "bar"]);
        assert_eq!(matches(r"\v<is>", "this is"), ["is"]);
        assert_eq!(matches(r"\v\(x\)", "f(x)"), ["(x)"]);
    }

    #[test]
    fn case_rules() {
        assert!(
            Pattern::new("hello", true, false)
                .expect("ok")
                .is_match("HELLO")
        );
        assert!(
            !Pattern::new("Hello", true, true)
                .expect("ok")
                .is_match("hello")
        );
        assert!(
            Pattern::new("hello", true, true)
                .expect("ok")
                .is_match("HELLO")
        );
        assert!(
            Pattern::new(r"hello\c", false, false)
                .expect("ok")
                .is_match("HELLO")
        );
        assert!(
            !Pattern::new(r"\Chello", true, false)
                .expect("ok")
                .is_match("HELLO")
        );
    }

    #[test]
    fn unsupported_features_are_errors_not_wrong_matches() {
        assert!(Pattern::new(r"\(a\)\1", false, false).is_err());
        assert!(Pattern::new(r"foo\zsbar", false, false).is_err());
        assert!(Pattern::new(r"a\{", false, false).is_err());
    }

    #[test]
    fn replacements() {
        let p = Pattern::new(r"\(\w\+\) \(\w\+\)", false, false).expect("ok");
        let caps = p.regex().captures("hello world").expect("match");
        let r = |s: &str| Replacement::new(s, Some("prev")).expand(&caps);
        assert_eq!(r(r"\2 \1"), "world hello");
        assert_eq!(r(r"[&]"), "[hello world]");
        assert_eq!(r(r"\&"), "&");
        assert_eq!(r(r"\u\1"), "Hello");
        assert_eq!(r(r"\U\1\E \2"), "HELLO world");
        assert_eq!(r(r"a\rb"), "a\nb");
        assert_eq!(r("~!"), "prev!");
    }
}
