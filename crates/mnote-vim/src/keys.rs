//! Keys as the Vim layer sees them, independent of any UI toolkit.

/// One key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A printable character, already shifted (`A`, `$`, `{`).
    Char(char),
    /// A character with Control held (`⌃r` is `Ctrl('r')`).
    Ctrl(char),
    Esc,
    Enter,
    Backspace,
    Tab,
}

/// Parses Vim-style key notation, for tests and key-binding tables:
/// `"d2w"`, `"ihello<esc>"`, `"<c-r>"`, `"<cr>"`, `"<bs>"`, `"<tab>"`, `"<lt>"`.
pub fn parse_keys(notation: &str) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut rest = notation;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(close) = rest.find('>')
            && let Some(key) = named_key(&rest[1..close])
        {
            keys.push(key);
            rest = &rest[close + 1..];
            continue;
        }
        keys.push(Key::Char(c));
        rest = &rest[c.len_utf8()..];
    }
    keys
}

fn named_key(name: &str) -> Option<Key> {
    let lower = name.to_ascii_lowercase();
    Some(match lower.as_str() {
        "esc" => Key::Esc,
        "cr" | "enter" => Key::Enter,
        "bs" => Key::Backspace,
        "tab" => Key::Tab,
        "lt" => Key::Char('<'),
        _ => {
            let ch = lower.strip_prefix("c-")?;
            let mut chars = ch.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            Key::Ctrl(c)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_named_keys() {
        assert_eq!(
            parse_keys("d2w<Esc><c-r><lt>x"),
            [
                Key::Char('d'),
                Key::Char('2'),
                Key::Char('w'),
                Key::Esc,
                Key::Ctrl('r'),
                Key::Char('<'),
                Key::Char('x'),
            ]
        );
        assert_eq!(parse_keys("<nope>"), parse_keys("<nope>"));
        assert_eq!(parse_keys("<nope>").len(), 6, "unknown names stay literal");
    }
}
