//! Keys as the Vim layer sees them, independent of any UI toolkit.

/// One key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A printable character, already shifted (`A`, `$`, `{`).
    Char(char),
    /// A character with Control held (`⌃r` is `Ctrl('r')`).
    Ctrl(char),
    /// A letter with Option / Alt held (`⌥j` is `Alt('j')`).
    Alt(char),
    Esc,
    Enter,
    Backspace,
    Tab,
    /// Shift-Tab.
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
}

/// Writes keys back in Vim notation (`<Esc>`, `<C-r>`, `jk`).
pub fn key_notation(keys: &[Key]) -> String {
    keys.iter()
        .map(|key| match key {
            Key::Char('<') => "<lt>".to_owned(),
            Key::Char(c) => c.to_string(),
            Key::Ctrl(c) => format!("<C-{c}>"),
            Key::Alt(c) => format!("<A-{c}>"),
            Key::Esc => "<Esc>".to_owned(),
            Key::Enter => "<CR>".to_owned(),
            Key::Backspace => "<BS>".to_owned(),
            Key::Tab => "<Tab>".to_owned(),
            Key::BackTab => "<S-Tab>".to_owned(),
            Key::Up => "<Up>".to_owned(),
            Key::Down => "<Down>".to_owned(),
            Key::Left => "<Left>".to_owned(),
            Key::Right => "<Right>".to_owned(),
            Key::Home => "<Home>".to_owned(),
            Key::End => "<End>".to_owned(),
            Key::PageUp => "<PageUp>".to_owned(),
            Key::PageDown => "<PageDown>".to_owned(),
        })
        .collect()
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
        "s-tab" => Key::BackTab,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "lt" => Key::Char('<'),
        _ => {
            let (alt, ch) = match lower.strip_prefix("c-") {
                Some(ch) => (false, ch),
                None => (
                    true,
                    lower
                        .strip_prefix("a-")
                        .or_else(|| lower.strip_prefix("m-"))?,
                ),
            };
            let mut chars = ch.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            if alt { Key::Alt(c) } else { Key::Ctrl(c) }
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
