//! Every command mNote offers by name, for the command palette (`⌘K`) and the
//! finder's `>` scope: what it's called, the keys that run it and its ex
//! form, where it has them (decision D-002: the palette shows ex
//! equivalents; `:` stays the Vim command line).

/// How a command runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Run {
    /// An ex command, as if typed after `:`.
    Ex(&'static str),
    /// Opens the finder with this query (`/` for text, `#` for tags).
    Finder(&'static str),
    /// Types these keys in NORMAL mode (`q:`, `/`).
    Keys(&'static str),
}

/// A command in the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    pub label: &'static str,
    /// The key binding, as shown in hints (`space f`).
    pub keys: Option<&'static str>,
    /// The ex form, without the `:` (`set rnu!`).
    pub ex: Option<&'static str>,
    /// One sentence for the palette's footer.
    pub about: &'static str,
    pub run: Run,
}

impl Command {
    /// The quiet text at the right of its row: keys, then the ex form.
    pub fn detail(&self) -> String {
        let ex = self.ex.map(|ex| format!(":{ex}"));
        match (self.keys, ex) {
            (Some(keys), Some(ex)) => format!("{keys} · {ex}"),
            (Some(keys), None) => keys.to_owned(),
            (None, Some(ex)) => ex,
            (None, None) => String::new(),
        }
    }

    /// What a query matches: the label, the keys and the ex form.
    pub fn haystack(&self) -> String {
        format!("{} {}", self.label, self.detail())
    }
}

const fn cmd(
    label: &'static str,
    keys: Option<&'static str>,
    ex: Option<&'static str>,
    about: &'static str,
    run: Run,
) -> Command {
    Command {
        label,
        keys,
        ex,
        about,
        run,
    }
}

/// Every command, in the palette's order when nothing is typed.
pub const COMMANDS: &[Command] = &[
    cmd(
        "Find a note",
        Some("space f"),
        None,
        "Fuzzy-find notes by name, title or alias; a new name creates one.",
        Run::Finder(""),
    ),
    cmd(
        "Explore folders",
        Some("space e"),
        Some("explore"),
        "The vault folder by folder, with a preview; h goes up, l opens.",
        Run::Keys(" e"),
    ),
    cmd(
        "Search note text",
        Some("space /"),
        None,
        "Search every note's text; ⌃q keeps the hits as a results list.",
        Run::Finder("/"),
    ),
    cmd(
        "Go to a heading in this note",
        Some("space @"),
        None,
        "Lists this note's headings; ⏎ jumps to one.",
        Run::Finder("@"),
    ),
    cmd(
        "Browse tags",
        Some("space #"),
        None,
        "Lists every tag with its note count; ⏎ lists the tag's notes.",
        Run::Finder("#"),
    ),
    cmd(
        "Toggle task",
        Some("space x"),
        None,
        "Checks or unchecks the task on the cursor line.",
        Run::Keys(" x"),
    ),
    cmd(
        "Cycle view",
        Some("space m"),
        Some("view"),
        "Styled, source (every marker, syntax colours) or plain; :view source picks one.",
        Run::Keys(" m"),
    ),
    cmd(
        "Save note",
        None,
        Some("w"),
        "Saves now. Notes also save a second after you stop typing.",
        Run::Ex("w"),
    ),
    cmd(
        "Search in this note",
        Some("/"),
        None,
        "Vim search with live highlights; n and N step through matches.",
        Run::Keys("/"),
    ),
    cmd(
        "Clear search highlights",
        None,
        Some("noh"),
        "Hides the highlights of the last search until the next one.",
        Run::Ex("nohlsearch"),
    ),
    cmd(
        "Toggle relative line numbers",
        None,
        Some("set rnu!"),
        "Switches the gutter between distances from the cursor and line numbers.",
        Run::Ex("set relativenumber!"),
    ),
    cmd(
        "Toggle ignore case in search",
        None,
        Some("set ic!"),
        "With it on, lowercase searches ignore case; a capital still matches case.",
        Run::Ex("set ignorecase!"),
    ),
    cmd(
        "Sort lines",
        None,
        Some("%sort"),
        "Sorts the whole note by line. Select lines first and use :sort for part of it.",
        Run::Ex("%sort"),
    ),
    cmd(
        "Undo",
        Some("u"),
        Some("undo"),
        "Undoes the last change.",
        Run::Ex("undo"),
    ),
    cmd(
        "Redo",
        Some("⌃r"),
        Some("redo"),
        "Redoes the last undone change.",
        Run::Ex("redo"),
    ),
    cmd(
        "Show registers",
        None,
        Some("reg"),
        "Lists what each register holds.",
        Run::Ex("registers"),
    ),
    cmd(
        "Open the command line",
        Some(":"),
        None,
        "The Vim command line, with live :s preview, ⇥ completion and history.",
        Run::Keys(":"),
    ),
    cmd(
        "Command-line history",
        Some("q:"),
        None,
        "The command history as a buffer you can edit; ⏎ runs a line.",
        Run::Keys("q:"),
    ),
    cmd(
        "Show the results list",
        None,
        Some("copen"),
        "The places kept with ⌃q from a text search.",
        Run::Ex("copen"),
    ),
    cmd(
        "Next result",
        None,
        Some("cnext"),
        "Goes to the next place in the results list.",
        Run::Ex("cnext"),
    ),
    cmd(
        "Previous result",
        None,
        Some("cprevious"),
        "Goes to the previous place in the results list.",
        Run::Ex("cprevious"),
    ),
    cmd(
        "Quit",
        None,
        Some("q"),
        "Saves the note and closes mNote.",
        Run::Ex("q"),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details_show_keys_then_ex() {
        let find = COMMANDS
            .iter()
            .find(|c| c.label == "Find a note")
            .expect("listed");
        assert_eq!(find.detail(), "space f");
        let undo = COMMANDS.iter().find(|c| c.label == "Undo").expect("listed");
        assert_eq!(undo.detail(), "u · :undo");
        assert!(undo.haystack().contains(":undo"));
    }

    #[test]
    fn every_command_has_a_description_and_a_unique_label() {
        for (i, c) in COMMANDS.iter().enumerate() {
            assert!(c.about.ends_with('.'), "{}", c.label);
            assert!(
                !COMMANDS[i + 1..].iter().any(|o| o.label == c.label),
                "{}",
                c.label
            );
        }
    }

    #[test]
    fn ex_forms_parse() {
        let mut vim = mnote_vim::Vim::new();
        for c in COMMANDS {
            if let Run::Ex(ex) = c.run {
                let mut buf = mnote_core::Buffer::new("b\na\n");
                let result = vim.execute(ex, &mut buf);
                assert!(
                    !matches!(&result, Err(e) if e.code == 492),
                    "{ex} isn't a command"
                );
            }
        }
    }
}
