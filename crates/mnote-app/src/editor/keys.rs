//! Turns gpui keystrokes into Vim keys.

use gpui::Keystroke;
use mnote_vim::Key;

/// The Vim key for a keystroke, or `None` for keys Vim doesn't see (`⌘`
/// shortcuts and function keys), which go on to the app.
pub fn vim_key(keystroke: &Keystroke) -> Option<Key> {
    let m = &keystroke.modifiers;
    if m.platform {
        return None;
    }
    match keystroke.key.as_str() {
        "escape" => return Some(Key::Esc),
        "enter" => return Some(Key::Enter),
        "backspace" => return Some(Key::Backspace),
        "tab" if !m.control && m.shift => return Some(Key::BackTab),
        "tab" if !m.control => return Some(Key::Tab),
        "up" => return Some(Key::Up),
        "down" => return Some(Key::Down),
        "left" => return Some(Key::Left),
        "right" => return Some(Key::Right),
        "space" if !m.control => return Some(Key::Char(' ')),
        _ => {}
    }
    if m.control {
        // ⌃[ is Escape in Vim.
        if keystroke.key == "[" {
            return Some(Key::Esc);
        }
        return single_char(&keystroke.key).map(|c| Key::Ctrl(c.to_ascii_lowercase()));
    }
    // The typed character, with Shift and Option applied ("A", "$", "ß").
    keystroke
        .key_char
        .as_deref()
        .and_then(single_char)
        .or_else(|| (!m.alt).then(|| single_char(&keystroke.key)).flatten())
        .map(Key::Char)
}

fn single_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_control() => Some(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn ks(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: key_char.map(Into::into),
        }
    }

    #[test]
    fn maps_typed_characters_and_named_keys() {
        let none = Modifiers::default();
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(vim_key(&ks("a", Some("a"), none)), Some(Key::Char('a')));
        assert_eq!(vim_key(&ks("4", Some("$"), shift)), Some(Key::Char('$')));
        assert_eq!(vim_key(&ks("space", Some(" "), none)), Some(Key::Char(' ')));
        assert_eq!(vim_key(&ks("escape", None, none)), Some(Key::Esc));
        assert_eq!(vim_key(&ks("enter", None, none)), Some(Key::Enter));
        assert_eq!(vim_key(&ks("backspace", None, none)), Some(Key::Backspace));
    }

    #[test]
    fn control_keys_and_shortcuts() {
        let ctrl = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        let cmd = Modifiers {
            platform: true,
            ..Modifiers::default()
        };
        assert_eq!(vim_key(&ks("r", None, ctrl)), Some(Key::Ctrl('r')));
        assert_eq!(vim_key(&ks("[", None, ctrl)), Some(Key::Esc));
        assert_eq!(vim_key(&ks("k", None, cmd)), None, "⌘K is the app's");
        assert_eq!(
            vim_key(&ks("left", None, Modifiers::default())),
            Some(Key::Left)
        );
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(vim_key(&ks("tab", None, shift)), Some(Key::BackTab));
        assert_eq!(vim_key(&ks("f1", None, Modifiers::default())), None);
    }
}
