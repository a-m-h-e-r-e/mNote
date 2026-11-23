//! mNote's built-in commands, registered in [`crate::commands`]. Each entry
//! is everything about one command: its palette label, keys, ex command and
//! what it does. Adding a command is adding an entry here.

use std::rc::Rc;

use gpui::Context;
use mnote_vim::ex::ArgKind;

use super::Editor;
use crate::commands::{Command, Invocation, Run};

/// Runs `f` on the editor.
fn act(f: impl Fn(&mut Editor, &Invocation, &mut Context<Editor>) + 'static) -> Run {
    Run::Action(Rc::new(f))
}

fn ex(line: &str) -> Run {
    Run::Ex(line.to_owned())
}

fn finder(query: &str) -> Run {
    Run::Finder(query.to_owned())
}

fn keys(keys: &str) -> Run {
    Run::Keys(keys.to_owned())
}

/// Every built-in command, in the palette's order when nothing is typed;
/// ex-only commands last.
pub fn builtin() -> Vec<Command> {
    let views = ArgKind::Choice(
        ["styled", "source", "plain"]
            .iter()
            .map(|v| ((*v).to_owned(), format!("{v} view")))
            .collect(),
    );
    vec![
        Command::new(
            "Find a note",
            "Fuzzy-find notes by name, title or alias; a new name creates one.",
            finder(""),
        )
        .keys("space f"),
        Command::new(
            "Explore folders",
            "The vault folder by folder, with a preview; h goes up, l opens.",
            act(|e, call, cx| e.open_explorer(call.args.clone(), cx)),
        )
        .keys("space e")
        .shown_as("explore")
        .ex("explore", 3, ArgKind::Folder),
        Command::new(
            "Search note text",
            "Search every note's text; ⌃q keeps the hits as a results list.",
            finder("/"),
        )
        .keys("space /"),
        Command::new(
            "Go to a heading in this note",
            "Lists this note's headings; ⏎ jumps to one.",
            finder("@"),
        )
        .keys("space @"),
        Command::new(
            "Browse tags",
            "Lists every tag with its note count; ⏎ lists the tag's notes.",
            finder("#"),
        )
        .keys("space #"),
        Command::new(
            "Show backlinks",
            "Every line in other notes that links here; ⏎ opens one at the link.",
            act(|e, _, cx| e.open_backlinks(cx)),
        )
        .keys("space b"),
        Command::new(
            "Follow link",
            "Opens the [[link]] under the cursor, at its #heading; a missing note is created.",
            act(|e, _, cx| e.follow_link(cx)),
        )
        .keys("gd")
        .ex("follow", 3, ArgKind::None),
        Command::new(
            "Toggle task",
            "Checks or unchecks the task on the cursor line.",
            act(|e, _, cx| e.toggle_task(cx)),
        )
        .keys("space x"),
        Command::new(
            "Cycle view",
            "Styled, source (every marker, syntax colours) or plain; :view source picks one.",
            act(|e, call, cx| e.view_command(call.args.as_deref(), cx)),
        )
        .keys("space m")
        .shown_as("view")
        .ex("view", 4, views),
        Command::new(
            "Open today's note",
            "The daily note (daily_note in settings), made if it doesn't exist yet.",
            act(|e, _, cx| e.open_daily(cx)),
        )
        .shown_as("daily")
        .ex("daily", 3, ArgKind::None),
        Command::new(
            "Open settings",
            "config.toml in mNote: vault, keys, theme, view and more. Saving applies it.",
            act(|e, _, cx| e.open_config(cx)),
        )
        .keys("space ,"),
        Command::new(
            "Save note",
            "Saves now. Notes also save a second after you stop typing.",
            act(|e, call, cx| e.write_command(call, cx)),
        )
        .keys("⌘S")
        .shown_as("w")
        .ex("write", 1, ArgKind::Note),
        Command::new(
            "Search in this note",
            "Vim search with live highlights; n and N step through matches.",
            keys("/"),
        )
        .keys("/"),
        Command::new(
            "Clear search highlights",
            "Hides the highlights of the last search until the next one.",
            ex("nohlsearch"),
        )
        .shown_as("noh"),
        Command::new(
            "Toggle relative line numbers",
            "Switches the gutter between distances from the cursor and line numbers.",
            ex("set relativenumber!"),
        )
        .shown_as("set rnu!"),
        Command::new(
            "Toggle ignore case in search",
            "With it on, lowercase searches ignore case; a capital still matches case.",
            ex("set ignorecase!"),
        )
        .shown_as("set ic!"),
        Command::new(
            "Sort lines",
            "Sorts the whole note by line. Select lines first and use :sort for part of it.",
            ex("%sort"),
        )
        .shown_as("%sort"),
        Command::new("Undo", "Undoes the last change.", ex("undo"))
            .keys("u")
            .shown_as("undo"),
        Command::new("Redo", "Redoes the last undone change.", ex("redo"))
            .keys("⌃r")
            .shown_as("redo"),
        Command::new(
            "Show registers",
            "Lists what each register holds.",
            ex("registers"),
        )
        .shown_as("reg"),
        Command::new(
            "Open the command line",
            "The Vim command line, with live :s preview, ⇥ completion and history.",
            keys(":"),
        )
        .keys(":"),
        Command::new(
            "Command-line history",
            "The command history as a buffer you can edit; ⏎ runs a line.",
            keys("q:"),
        )
        .keys("q:"),
        Command::new(
            "Show the results list",
            "The places kept with ⌃q from a text search.",
            act(|e, _, cx| e.open_results_list(cx)),
        )
        .shown_as("copen")
        .ex("copen", 4, ArgKind::None),
        Command::new(
            "Next result",
            "Goes to the next place in the results list.",
            act(|e, _, cx| e.list_step(1, cx)),
        )
        .shown_as("cnext")
        .ex("cnext", 2, ArgKind::None),
        Command::new(
            "Previous result",
            "Goes to the previous place in the results list.",
            act(|e, _, cx| e.list_step(-1, cx)),
        )
        .shown_as("cprevious")
        .ex("cprevious", 2, ArgKind::None),
        Command::new(
            "Quit",
            "Saves the note and closes mNote.",
            act(|e, call, cx| e.quit(false, call.bang, cx)),
        )
        .shown_as("q")
        .ex("quit", 1, ArgKind::None),
        // Ex commands that aren't in the palette.
        Command::ex_only(
            "wq",
            2,
            ArgKind::None,
            "Saves and closes mNote.",
            act(|e, call, cx| e.quit(true, call.bang, cx)),
        ),
        Command::ex_only(
            "xit",
            1,
            ArgKind::None,
            "Saves and closes mNote.",
            act(|e, call, cx| e.quit(true, call.bang, cx)),
        ),
        Command::ex_only(
            "edit",
            1,
            ArgKind::Note,
            "Opens a note.",
            act(|e, _, _| e.not_yet(":edit")),
        ),
        Command::ex_only(
            "find",
            3,
            ArgKind::Note,
            "Finds a note.",
            act(|e, _, _| e.not_yet(":find")),
        ),
        Command::ex_only(
            "cNext",
            2,
            ArgKind::None,
            "Goes to the previous place in the results list.",
            act(|e, _, cx| e.list_step(-1, cx)),
        ),
        Command::ex_only(
            "split",
            2,
            ArgKind::Note,
            "Splits the pane, on that note or this one.",
            act(|e, call, cx| e.split_command(false, call.args.as_deref(), cx)),
        ),
        Command::ex_only(
            "vsplit",
            2,
            ArgKind::Note,
            "Splits the pane side by side, on that note or this one.",
            act(|e, call, cx| e.split_command(true, call.args.as_deref(), cx)),
        ),
        Command::ex_only(
            "close",
            3,
            ArgKind::None,
            "Closes this pane.",
            act(|e, _, cx| e.close_command(cx)),
        ),
        Command::ex_only(
            "only",
            2,
            ArgKind::None,
            "Closes every other pane.",
            act(|e, _, cx| e.only_pane(cx)),
        ),
    ]
}
