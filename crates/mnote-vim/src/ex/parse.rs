//! Parsing a command line: `[range]name[!] [args]`.

use super::ExError;

/// The base of a line address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Base {
    /// A line number as typed (1-based; 0 means "before the first line").
    Line(usize),
    /// `.`, or nothing before an offset (`+3`).
    Current,
    /// `$`
    Last,
    /// `'a`, `'<`, `''`
    Mark(char),
    /// `/pat/` (forward) or `?pat?` (backward).
    Search { pattern: String, forward: bool },
}

/// A line address: a base and offsets (`.+3`, `$-1`, `'a+2`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub base: Base,
    pub offset: i64,
}

/// The range before a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Range {
    /// No range typed: the command's default applies.
    None,
    /// `%`: the whole note.
    Whole,
    One(Address),
    Two(Address, Address),
}

/// A command line split into its parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed<'a> {
    pub range: Range,
    /// The full command name, after abbreviation (`s` → `substitute`).
    pub name: &'static str,
    pub bang: bool,
    /// Everything after the name and `!`, untrimmed.
    pub args: &'a str,
}

/// Commands and the shortest abbreviation Vim accepts for each. Order
/// matters where abbreviations overlap: the first match wins.
const COMMANDS: &[(&str, usize)] = &[
    ("substitute", 1),
    ("imap", 2),
    ("inoremap", 3),
    ("iunmap", 3),
    ("map", 3),
    ("noremap", 2),
    ("unmap", 3),
    ("nmap", 2),
    ("nnoremap", 2),
    ("nunmap", 3),
    ("vmap", 2),
    ("vnoremap", 2),
    ("vunmap", 2),
    ("global", 1),
    ("vglobal", 1),
    ("delete", 1),
    ("display", 2),
    ("move", 1),
    ("t", 1),
    ("copy", 2),
    ("yank", 1),
    ("put", 2),
    ("normal", 4),
    ("nohlsearch", 3),
    ("sort", 3),
    ("set", 2),
    ("undo", 1),
    ("redo", 3),
    ("registers", 3),
];

/// Splits `input` (without the leading `:`) into range, name, `!` and args.
pub fn parse(input: &str) -> Result<Parsed<'_>, ExError> {
    let (range, rest) = parse_range(input.trim_start())?;
    let rest = rest.trim_start();
    if rest.is_empty() {
        // A bare range jumps to its last line (`:42`).
        return Ok(Parsed {
            range,
            name: "goto",
            bang: false,
            args: "",
        });
    }
    let first = rest.chars().next().unwrap_or(' ');
    let (word, after) = if first.is_ascii_alphabetic() {
        let end = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        // `:s` takes its delimiter right after the name: `:s/a/b/`,
        // `:g/x/d`. Letters only count as the name up to the known command.
        (&rest[..end], &rest[end..])
    } else {
        (&rest[..first.len_utf8()], &rest[first.len_utf8()..])
    };
    let name = match word {
        ">" => ">",
        "<" => "<",
        "&" => "&",
        _ => lookup(word).ok_or_else(|| ExError::not_a_command(rest))?,
    };
    // `:s` and `:g` may be glued to a pattern that starts with letters only if
    // a delimiter follows; `:sno` etc. aren't supported.
    let (bang, args) = match after.strip_prefix('!') {
        Some(args) => (true, args),
        None => (false, after),
    };
    Ok(Parsed {
        range,
        name,
        bang,
        args,
    })
}

/// A command's full name from what was typed: Vim's own first, then
/// extensions' ([`crate::ext::register_ex`]), then the app's (so `:s` stays
/// `substitute` and `:sp` is the app's `split`).
pub(crate) fn lookup(word: &str) -> Option<&'static str> {
    COMMANDS
        .iter()
        .find(|(full, min)| word.len() >= *min && full.starts_with(word))
        .map(|(full, _)| *full)
        .or_else(|| crate::ext::command_lookup(word))
        .or_else(|| {
            APP.with(|app| {
                app.borrow()
                    .iter()
                    .find(|c| word.len() >= c.abbrev.max(1) && c.name.starts_with(word))
                    .map(|c| c.name)
            })
        })
}

/// Whether `word` starts some command's name (`so` while typing `sort`).
pub(crate) fn is_command_prefix(word: &str) -> bool {
    COMMANDS.iter().any(|(full, _)| full.starts_with(word))
        || crate::ext::commands()
            .iter()
            .any(|c| c.name.starts_with(word))
        || APP.with(|app| app.borrow().iter().any(|c| c.name.starts_with(word)))
}

/// An app command as registered, with its name kept for the program's life
/// (parsed names are `&'static str`; the app registers a few dozen once).
pub(crate) struct AppSpec {
    pub name: &'static str,
    pub abbrev: usize,
    pub spec: super::CommandSpec,
}

thread_local! {
    static APP: std::cell::RefCell<std::rc::Rc<[AppSpec]>> =
        std::cell::RefCell::new(std::rc::Rc::from(Vec::new()));
}

pub(crate) fn set_app_commands(specs: Vec<super::CommandSpec>) {
    let old = APP.with(|app| app.borrow().clone());
    let specs: Vec<AppSpec> = specs
        .into_iter()
        .map(|spec| {
            // Reuse a name kept before rather than keeping it again.
            let name = old
                .iter()
                .find(|o| o.name == spec.name)
                .map(|o| o.name)
                .unwrap_or_else(|| Box::leak(spec.name.clone().into_boxed_str()));
            AppSpec {
                name,
                abbrev: spec.abbrev,
                spec,
            }
        })
        .collect();
    APP.with(|app| *app.borrow_mut() = std::rc::Rc::from(specs));
}

/// The app's (or an extension's) command named `name` (its full name).
pub(crate) fn app_command(name: &str) -> Option<super::CommandSpec> {
    if let Some(ext) = crate::ext::command(name) {
        return Some(ext.spec.clone());
    }
    APP.with(|app| {
        app.borrow()
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.spec.clone())
    })
}

/// Every app command, for completion.
pub(crate) fn app_commands() -> std::rc::Rc<[AppSpec]> {
    APP.with(|app| app.borrow().clone())
}

pub(crate) fn parse_range(input: &str) -> Result<(Range, &str), ExError> {
    if let Some(rest) = input.strip_prefix('%') {
        return Ok((Range::Whole, rest));
    }
    let Some((first, rest)) = parse_address(input)? else {
        return Ok((Range::None, input));
    };
    let trimmed = rest.trim_start();
    if let Some(after) = trimmed
        .strip_prefix(',')
        .or_else(|| trimmed.strip_prefix(';'))
    {
        let (second, rest) = parse_address(after.trim_start())?.unwrap_or((
            Address {
                base: Base::Current,
                offset: 0,
            },
            after,
        ));
        return Ok((Range::Two(first, second), rest));
    }
    Ok((Range::One(first), rest))
}

/// Parses one address, or `None` if `input` doesn't start with one.
fn parse_address(input: &str) -> Result<Option<(Address, &str)>, ExError> {
    let mut rest = input;
    let base = match rest.chars().next() {
        Some(c) if c.is_ascii_digit() => {
            let end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            let n = rest[..end].parse().map_err(|_| ExError::invalid_range())?;
            rest = &rest[end..];
            Base::Line(n)
        }
        Some('.') => {
            rest = &rest[1..];
            Base::Current
        }
        Some('$') => {
            rest = &rest[1..];
            Base::Last
        }
        Some('\'') => {
            let mut chars = rest[1..].chars();
            let mark = chars.next().ok_or_else(ExError::invalid_range)?;
            rest = chars.as_str();
            Base::Mark(mark)
        }
        Some(d @ ('/' | '?')) => {
            let (pattern, after) = split_delimited(&rest[1..], d);
            rest = after;
            Base::Search {
                pattern,
                forward: d == '/',
            }
        }
        Some('+' | '-') => Base::Current,
        _ => return Ok(None),
    };

    let mut offset = 0i64;
    loop {
        let trimmed = rest.trim_start();
        let sign = match trimmed.chars().next() {
            Some('+') => 1,
            Some('-') => -1,
            _ => break,
        };
        let digits = &trimmed[1..];
        let end = digits
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(digits.len());
        let n: i64 = if end == 0 {
            1
        } else {
            digits[..end]
                .parse()
                .map_err(|_| ExError::invalid_range())?
        };
        offset += sign * n;
        rest = &digits[end..];
    }
    Ok(Some((Address { base, offset }, rest)))
}

/// Splits `s` at the first `delim` not escaped with a backslash. The escape
/// before an escaped delimiter is dropped; other escapes are kept for the
/// pattern. Returns the part before and the rest after the delimiter (or the
/// whole string and `""` if there's none).
pub fn split_delimited(s: &str, delim: char) -> (String, &str) {
    let mut out = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some((_, n)) if n == delim => out.push(n),
                Some((_, n)) => {
                    out.push('\\');
                    out.push(n);
                }
                None => out.push('\\'),
            }
        } else if c == delim {
            return (out, &s[i + c.len_utf8()..]);
        } else {
            out.push(c);
        }
    }
    (out, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(base: Base, offset: i64) -> Address {
        Address { base, offset }
    }

    #[test]
    fn ranges() {
        assert_eq!(parse("%s/a/b/").expect("ok").range, Range::Whole);
        assert_eq!(
            parse("1,20d").expect("ok").range,
            Range::Two(addr(Base::Line(1), 0), addr(Base::Line(20), 0))
        );
        assert_eq!(
            parse(".,+5>").expect("ok").range,
            Range::Two(addr(Base::Current, 0), addr(Base::Current, 5))
        );
        assert_eq!(
            parse("'<,'>sort").expect("ok").range,
            Range::Two(addr(Base::Mark('<'), 0), addr(Base::Mark('>'), 0))
        );
        assert_eq!(
            parse("$-1d").expect("ok").range,
            Range::One(addr(Base::Last, -1))
        );
        assert_eq!(
            parse("/TODO/,/END/d").expect("ok").range,
            Range::Two(
                addr(
                    Base::Search {
                        pattern: "TODO".into(),
                        forward: true
                    },
                    0
                ),
                addr(
                    Base::Search {
                        pattern: "END".into(),
                        forward: true
                    },
                    0
                )
            )
        );
    }

    #[test]
    fn names_abbreviations_and_bang() {
        crate::ex::tests::test_app_commands();
        let p = parse("s/a/b/g").expect("ok");
        assert_eq!((p.name, p.args), ("substitute", "/a/b/g"));
        assert_eq!(parse("g!/x/d").expect("ok").name, "global");
        assert!(parse("g!/x/d").expect("ok").bang);
        assert_eq!(parse("noh").expect("ok").name, "nohlsearch");
        assert_eq!(parse("norm Ax").expect("ok").name, "normal");
        assert_eq!(parse("sort u").expect("ok").name, "sort");
        assert_eq!(parse("wq").expect("ok").name, "wq");
        assert!(parse("q!").expect("ok").bang);
        assert_eq!(parse("u").expect("ok").name, "undo");
        assert_eq!(parse("reg").expect("ok").name, "registers");
        assert_eq!(parse("m 0").expect("ok").name, "move");
        assert_eq!(parse(">>").expect("ok").args, ">");
        assert_eq!(parse("42").expect("ok").name, "goto");
        assert_eq!(parse("cn").expect("ok").name, "cnext");
        assert_eq!(parse("cp").expect("ok").name, "cprevious");
        assert_eq!(parse("cope").expect("ok").name, "copen");
        assert_eq!(parse("co 0").expect("ok").name, "copy");
    }

    #[test]
    fn unknown_commands_are_e492() {
        let err = parse("frobnicate").expect_err("unknown");
        assert_eq!(err.to_string(), "E492: Not an editor command: frobnicate");
        assert!(parse("n").is_err(), "too short to choose");
        assert_eq!(parse("no").expect("Vim's :no").name, "noremap");
        assert_eq!(parse("noh").expect("ok").name, "nohlsearch");
        assert_eq!(parse("norm x").expect("ok").name, "normal");
    }

    #[test]
    fn delimiters_and_escapes() {
        assert_eq!(split_delimited(r"a\/b/rest", '/'), ("a/b".into(), "rest"));
        assert_eq!(split_delimited(r"a\.b#c", '#'), (r"a\.b".into(), "c"));
        assert_eq!(split_delimited("no end", '/'), ("no end".into(), ""));
    }
}
