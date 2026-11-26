//! The app's key bindings: key sequences that run commands from the
//! registry ([`crate::commands`]). Defaults come from the commands
//! themselves; config.toml's `[keys]` table adds, moves or removes them.
//!
//! Bindings are written as in Vim: `<leader>f`, `<C-w>v` (⌃w then v),
//! `<C-w><Left>`, and `<D-k>` for ⌘K. Chords — `<D-S-t>` (⌘⇧T),
//! `<C-Tab>`, `<C-S-Tab>`, `<C-PageDown>`, `<C-PageUp>` — are single keys
//! too. A binding that starts with ⌘ or is a chord works in any mode; the
//! others work in NORMAL with nothing half typed, before Vim sees the keys. A sequence that's the start of a longer binding waits
//! for the next key.

use std::cell::RefCell;
use std::rc::Rc;

use mnote_vim::Key;

use crate::commands::Command;

/// One key in a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stroke {
    /// config.toml's `leader`.
    Leader,
    Key(Key),
    /// ⌘ with a letter or symbol (`⌘K` is `Cmd('k')`).
    Cmd(char),
    /// A key with modifiers Vim doesn't see, in any mode: ⌘⇧T, ⌃⇥, ⌃PageDown.
    Chord(Chord),
}

/// A key held with ⌘, ⌃ or ⇧ that the app takes in every mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub cmd: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub key: ChordKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChordKey {
    /// A letter, digit or symbol, unshifted and lower case (`t`, `]`).
    Char(char),
    Tab,
    PageUp,
    PageDown,
}

impl Chord {
    /// The chord for a key with these modifiers, if it's one (⌘ with ⇧,
    /// or ⌃ with ⇥ or Page Up/Down). Plain ⌘ keys are `Stroke::Cmd`.
    pub fn of(key: &str, cmd: bool, ctrl: bool, shift: bool) -> Option<Chord> {
        let key = match key.to_ascii_lowercase().as_str() {
            "tab" => ChordKey::Tab,
            "pageup" => ChordKey::PageUp,
            "pagedown" => ChordKey::PageDown,
            other => {
                let mut chars = other.chars();
                match (chars.next(), chars.next()) {
                    // ⇧ may come as the shifted symbol: ⌘⇧] as `}`.
                    (Some('{'), None) => ChordKey::Char('['),
                    (Some('}'), None) => ChordKey::Char(']'),
                    (Some(c), None) => ChordKey::Char(c),
                    _ => return None,
                }
            }
        };
        let chord = Chord {
            cmd,
            ctrl,
            shift,
            key,
        };
        let ok = match key {
            ChordKey::Char(_) => cmd && shift && !ctrl,
            ChordKey::Tab | ChordKey::PageUp | ChordKey::PageDown => ctrl && !cmd,
        };
        ok.then_some(chord)
    }
}

/// A key sequence and the command (registry id) it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub strokes: Vec<Stroke>,
    pub command: String,
}

/// What the keys typed so far are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// No binding starts with them.
    None,
    /// The start of a binding: wait for more.
    Prefix,
    /// A binding, and no longer one starts with it: run it.
    Exact(String),
    /// A binding that's also the start of a longer one: run it if nothing
    /// follows in a moment.
    ExactOrLonger(String),
}

/// Every binding.
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    bindings: Vec<Binding>,
}

impl Keymap {
    /// The commands' default bindings, then `overrides` from config.toml
    /// (`"<leader>d" = "daily"`; `"none"` removes a binding). Returns the
    /// keymap and what was wrong with any override.
    pub fn build(commands: &[Command], overrides: &[(String, String)]) -> (Keymap, Vec<String>) {
        let mut map = Keymap::default();
        for command in commands {
            for notation in &command.bind {
                if let Ok(strokes) = parse(notation) {
                    map.bind(strokes, &command.id);
                }
            }
        }
        let mut problems = Vec::new();
        for (keys, id) in overrides {
            let strokes = match parse(keys) {
                Ok(strokes) => strokes,
                Err(why) => {
                    problems.push(format!("{keys}: {why}"));
                    continue;
                }
            };
            if id == "none" {
                map.bindings.retain(|b| b.strokes != strokes);
            } else if commands.iter().any(|c| c.id == *id) {
                map.bind(strokes, id);
            } else {
                problems.push(format!("{keys}: there's no command called {id}"));
            }
        }
        (map, problems)
    }

    /// Binds `strokes` to `command`, replacing what they ran before.
    pub fn bind(&mut self, strokes: Vec<Stroke>, command: &str) {
        self.bindings.retain(|b| b.strokes != strokes);
        self.bindings.push(Binding {
            strokes,
            command: command.to_owned(),
        });
    }

    /// The keys typed so far (in NORMAL), against the bindings without ⌘.
    pub fn lookup(&self, typed: &[Key], leader: char) -> Lookup {
        let matches = |b: &Binding, n: usize| {
            b.strokes.len() >= n
                && b.strokes[..n]
                    .iter()
                    .zip(typed)
                    .all(|(s, k)| stroke_is(*s, *k, leader))
        };
        let n = typed.len();
        let exact = self
            .bindings
            .iter()
            .find(|b| b.strokes.len() == n && matches(b, n));
        let longer = self
            .bindings
            .iter()
            .any(|b| b.strokes.len() > n && matches(b, n));
        match (exact, longer) {
            (Some(b), false) => Lookup::Exact(b.command.clone()),
            (Some(b), true) => Lookup::ExactOrLonger(b.command.clone()),
            (None, true) => Lookup::Prefix,
            (None, false) => Lookup::None,
        }
    }

    /// The command bound to `chord`.
    pub fn chord(&self, chord: Chord) -> Option<&str> {
        self.bindings
            .iter()
            .find(|b| b.strokes == [Stroke::Chord(chord)])
            .map(|b| b.command.as_str())
    }

    /// The command bound to ⌘ with `key` (lower case).
    pub fn cmd(&self, key: char) -> Option<&str> {
        self.bindings
            .iter()
            .find(|b| b.strokes == [Stroke::Cmd(key)])
            .map(|b| b.command.as_str())
    }

    /// How the first binding of `command` is shown (`space f`, `⌃w v`).
    pub fn shown_for(&self, command: &str) -> Option<String> {
        self.bindings
            .iter()
            .find(|b| b.command == command)
            .map(|b| show(&b.strokes))
    }

    /// Every binding, in order.
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }
}

fn stroke_is(stroke: Stroke, key: Key, leader: char) -> bool {
    match stroke {
        Stroke::Leader => key == Key::Char(leader),
        Stroke::Key(k) => k == key,
        Stroke::Cmd(_) | Stroke::Chord(_) => false,
    }
}

/// A binding written as in Vim: `<leader>f`, `<C-w>v`, `<D-k>`.
pub fn parse(notation: &str) -> Result<Vec<Stroke>, String> {
    let mut strokes = Vec::new();
    let mut rest = notation.trim();
    if rest.is_empty() {
        return Err("no keys".into());
    }
    while !rest.is_empty() {
        let lower = rest.to_ascii_lowercase();
        if lower.starts_with("<leader>") {
            strokes.push(Stroke::Leader);
            rest = &rest["<leader>".len()..];
            continue;
        }
        if lower.starts_with("<d-s-")
            || lower.starts_with("<s-d-")
            || lower.starts_with("<c-tab>")
            || lower.starts_with("<c-s-tab>")
            || lower.starts_with("<c-page")
        {
            let end = rest.find('>').ok_or("an unclosed <…>")?;
            let inner = &lower[1..end];
            let parts: Vec<&str> = inner.split('-').collect();
            let (mods, key) = parts.split_at(parts.len() - 1);
            let has = |m: &str| mods.contains(&m);
            let key = if key[0].is_empty() { "-" } else { key[0] };
            match Chord::of(key, has("d"), has("c"), has("s")) {
                Some(chord) => strokes.push(Stroke::Chord(chord)),
                None => return Err(format!("{} isn't a key", &rest[..=end])),
            }
            rest = &rest[end + 1..];
            continue;
        }
        if lower.starts_with("<d-") {
            let end = rest.find('>').ok_or("an unclosed <D-…>")?;
            let mut chars = rest[3..end].chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => strokes.push(Stroke::Cmd(c.to_ascii_lowercase())),
                _ => return Err("<D-…> takes one key, like <D-k>".into()),
            }
            rest = &rest[end + 1..];
            continue;
        }
        // One Vim key: `<…>` or a character.
        let len = if rest.starts_with('<') {
            rest.find('>').map_or(1, |end| end + 1)
        } else {
            rest.chars().next().map_or(1, char::len_utf8)
        };
        let keys = mnote_vim::parse_keys(&rest[..len]);
        match keys.as_slice() {
            [key] => strokes.push(Stroke::Key(*key)),
            _ => return Err(format!("{} isn't a key", &rest[..len])),
        }
        rest = &rest[len..];
    }
    if strokes.len() > 1
        && strokes
            .iter()
            .any(|s| matches!(s, Stroke::Cmd(_) | Stroke::Chord(_)))
    {
        return Err("a ⌘ key or chord only works on its own".into());
    }
    Ok(strokes)
}

/// A binding as hints show it: `space f`, `⌃w v`, `⌘K`.
pub fn show(strokes: &[Stroke]) -> String {
    strokes
        .iter()
        .map(|s| match s {
            Stroke::Leader => mnote_core::config::leader_name(crate::commands::leader()),
            Stroke::Cmd(c) => format!("⌘{}", c.to_ascii_uppercase()),
            Stroke::Chord(chord) => {
                let mut out = String::new();
                if chord.ctrl {
                    out.push('⌃');
                }
                if chord.shift {
                    out.push('⇧');
                }
                if chord.cmd {
                    out.push('⌘');
                }
                match chord.key {
                    ChordKey::Char(c) => out.extend(c.to_uppercase()),
                    ChordKey::Tab => out.push('⇥'),
                    ChordKey::PageUp => out.push_str("PageUp"),
                    ChordKey::PageDown => out.push_str("PageDown"),
                }
                out
            }
            Stroke::Key(key) => match key {
                Key::Ctrl(c) => format!("⌃{c}"),
                Key::Alt(c) => format!("⌥{c}"),
                Key::Char(' ') => "space".into(),
                Key::Char(c) => c.to_string(),
                Key::Left => "←".into(),
                Key::Right => "→".into(),
                Key::Up => "↑".into(),
                Key::Down => "↓".into(),
                Key::Enter => "⏎".into(),
                Key::Esc => "esc".into(),
                Key::Tab => "⇥".into(),
                other => mnote_vim::key_notation(&[*other]),
            },
        })
        .collect::<Vec<_>>()
        .join(" ")
}

thread_local! {
    // The keymap in use (UI thread only).
    static KEYMAP: RefCell<Rc<Keymap>> = RefCell::new(Rc::new(Keymap::default()));
}

/// Puts `keymap` in use.
pub fn set(keymap: Keymap) {
    KEYMAP.with(|k| *k.borrow_mut() = Rc::new(keymap));
}

/// The keymap in use.
pub fn current() -> Rc<Keymap> {
    KEYMAP.with(|k| k.borrow().clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Run;

    fn command(id: &str, bind: &[&str]) -> Command {
        let mut c = Command::new(id, id, "Does it.", Run::Ex("noh".into()));
        for b in bind {
            c = c.bind(b);
        }
        c
    }

    fn keys(s: &str) -> Vec<Key> {
        mnote_vim::parse_keys(s)
    }

    #[test]
    fn notation_parses_and_shows() {
        assert_eq!(
            parse("<leader>f"),
            Ok(vec![Stroke::Leader, Stroke::Key(Key::Char('f'))])
        );
        assert_eq!(
            parse("<C-w><Left>"),
            Ok(vec![Stroke::Key(Key::Ctrl('w')), Stroke::Key(Key::Left)])
        );
        assert_eq!(parse("<D-K>"), Ok(vec![Stroke::Cmd('k')]));
        let chord = |cmd, ctrl, shift, key| {
            Ok(vec![Stroke::Chord(Chord {
                cmd,
                ctrl,
                shift,
                key,
            })])
        };
        assert_eq!(
            parse("<D-S-t>"),
            chord(true, false, true, ChordKey::Char('t'))
        );
        assert_eq!(
            parse("<S-D-]>"),
            chord(true, false, true, ChordKey::Char(']'))
        );
        assert_eq!(parse("<C-Tab>"), chord(false, true, false, ChordKey::Tab));
        assert_eq!(parse("<C-S-Tab>"), chord(false, true, true, ChordKey::Tab));
        assert_eq!(
            parse("<C-PageDown>"),
            chord(false, true, false, ChordKey::PageDown)
        );
        assert!(parse("<C-w><C-Tab>").is_err(), "a chord only on its own");
        assert_eq!(show(&parse("<D-S-t>").expect("ok")), "⇧⌘T");
        assert_eq!(show(&parse("<C-S-Tab>").expect("ok")), "⌃⇧⇥");
        assert_eq!(show(&parse("<C-PageUp>").expect("ok")), "⌃PageUp");
        // Typed keys map to the same chords; plain ⌘ keys aren't chords.
        assert_eq!(
            Chord::of("}", true, false, true),
            Some(Chord {
                cmd: true,
                ctrl: false,
                shift: true,
                key: ChordKey::Char(']')
            })
        );
        assert_eq!(Chord::of("t", true, false, false), None);
        assert_eq!(Chord::of("tab", false, false, false), None);
        assert!(parse("").is_err());
        assert!(parse("<D-kk>").is_err());
        assert!(parse("a<D-k>").is_err(), "⌘ only on its own");
        assert!(parse("<nope>").is_err());
        assert_eq!(show(&parse("<C-w>v").expect("ok")), "⌃w v");
        assert_eq!(show(&parse("<D-s>").expect("ok")), "⌘S");
        crate::commands::set_leader(' ');
        assert_eq!(show(&parse("<leader>,").expect("ok")), "space ,");
    }

    #[test]
    fn prefixes_wait_and_exact_bindings_run() {
        let commands = [
            command("find", &["<leader>f"]),
            command("split", &["<C-w>s", "<C-w><C-s>"]),
            command("palette", &["<D-k>"]),
        ];
        let (map, problems) = Keymap::build(&commands, &[]);
        assert!(problems.is_empty());
        assert_eq!(map.lookup(&keys(" "), ' '), Lookup::Prefix);
        assert_eq!(map.lookup(&keys(" f"), ' '), Lookup::Exact("find".into()));
        assert_eq!(map.lookup(&keys(",f"), ','), Lookup::Exact("find".into()));
        assert_eq!(map.lookup(&keys(" z"), ' '), Lookup::None);
        assert_eq!(
            map.lookup(&keys("<C-w><C-s>"), ' '),
            Lookup::Exact("split".into())
        );
        assert_eq!(map.lookup(&keys("x"), ' '), Lookup::None);
        assert_eq!(map.cmd('k'), Some("palette"));
        assert_eq!(map.shown_for("split").as_deref(), Some("⌃w s"));
    }

    #[test]
    fn config_overrides_add_move_and_remove() {
        let commands = [
            command("find", &["<leader>f"]),
            command("daily", &[]),
            command("explore", &["<leader>e"]),
        ];
        let overrides = [
            ("<leader>d".to_owned(), "daily".to_owned()),
            ("<leader>e".to_owned(), "none".to_owned()),
            ("<leader>f".to_owned(), "explore".to_owned()),
            ("<leader>x".to_owned(), "nope".to_owned()),
            ("<D-kk>".to_owned(), "find".to_owned()),
        ];
        let (map, problems) = Keymap::build(&commands, &overrides);
        assert_eq!(map.lookup(&keys(" d"), ' '), Lookup::Exact("daily".into()));
        assert_eq!(map.lookup(&keys(" e"), ' '), Lookup::None, "removed");
        assert_eq!(
            map.lookup(&keys(" f"), ' '),
            Lookup::Exact("explore".into()),
            "moved"
        );
        assert_eq!(problems.len(), 2);
        assert!(problems[0].contains("no command called nope"));
    }

    #[test]
    fn a_binding_that_starts_a_longer_one_waits() {
        let commands = [command("a", &["<leader>g"]), command("b", &["<leader>gg"])];
        let (map, _) = Keymap::build(&commands, &[]);
        assert_eq!(
            map.lookup(&keys(" g"), ' '),
            Lookup::ExactOrLonger("a".into())
        );
        assert_eq!(map.lookup(&keys(" gg"), ' '), Lookup::Exact("b".into()));
    }
}
