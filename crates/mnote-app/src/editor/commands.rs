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

/// `<C-w>x` and `<C-w><C-x>`, as Vim takes both.
fn pane(c: Command, key: char) -> Command {
    c.bind(&format!("<C-w>{key}"))
        .bind(&format!("<C-w><C-{key}>"))
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
    let mut all = vec![
        Command::new(
            "find",
            "Find a note",
            "Fuzzy-find notes by name, title or alias; a new name creates one.",
            finder(""),
        )
        .bind("<leader>f"),
        Command::new(
            "explore",
            "Explore folders",
            "The vault folder by folder, with a preview; h goes up, l opens.",
            act(|e, call, cx| e.open_explorer(call.args.clone(), cx)),
        )
        .bind("<leader>e")
        .shown_as("explore")
        .ex("explore", 3, ArgKind::Folder),
        Command::new(
            "search",
            "Search note text",
            "Search every note's text; ⌃q keeps the hits as a results list.",
            finder("/"),
        )
        .bind("<leader>/"),
        Command::new(
            "headings",
            "Go to a heading in this note",
            "Lists this note's headings; ⏎ jumps to one.",
            finder("@"),
        )
        .bind("<leader>@"),
        Command::new(
            "tags",
            "Browse tags",
            "Lists every tag with its note count; ⏎ lists the tag's notes.",
            finder("#"),
        )
        .bind("<leader>#"),
        Command::new(
            "backlinks",
            "Show backlinks",
            "Every line in other notes that links here; ⏎ opens one at the link.",
            act(|e, _, cx| e.open_backlinks(cx)),
        )
        .bind("<leader>b"),
        Command::new(
            "follow",
            "Follow link",
            "Opens the [[link]] under the cursor, at its #heading; a missing note is created.",
            act(|e, _, cx| e.follow_link(cx)),
        )
        .keys("gd")
        .ex("follow", 3, ArgKind::None),
        Command::new(
            "toggle-task",
            "Toggle task",
            "Checks or unchecks the task on the cursor line.",
            act(|e, _, cx| e.toggle_task(cx)),
        )
        .bind("<leader>x"),
        Command::new(
            "view",
            "Cycle view",
            "Styled, source (every marker, syntax colours) or plain; :view source picks one.",
            act(|e, call, cx| e.view_command(call.args.as_deref(), cx)),
        )
        .bind("<leader>m")
        .shown_as("view")
        .ex("view", 4, views),
        Command::new(
            "daily",
            "Open today's note",
            "The daily note (daily_note in settings), made if it doesn't exist yet.",
            act(|e, _, cx| e.open_daily(cx)),
        )
        .shown_as("daily")
        .ex("daily", 3, ArgKind::None),
        Command::new(
            "settings",
            "Open settings",
            "config.toml in mNote: vault, keys, theme, view and more. Saving applies it.",
            act(|e, _, cx| e.open_config(cx)),
        )
        .bind("<leader>,"),
        Command::new(
            "palette",
            "Command palette",
            "Every command by name, with its keys and ex form.",
            act(|e, _, cx| e.open_palette(cx)),
        )
        .bind("<D-k>"),
        Command::new(
            "write",
            "Save note",
            "Saves now. Notes also save a second after you stop typing.",
            act(|e, call, cx| e.write_command(call, cx)),
        )
        .bind("<D-s>")
        .shown_as("w")
        .ex("write", 1, ArgKind::Note),
        Command::new(
            "search-note",
            "Search in this note",
            "Vim search with live highlights; n and N step through matches.",
            keys("/"),
        )
        .keys("/"),
        Command::new(
            "nohlsearch",
            "Clear search highlights",
            "Hides the highlights of the last search until the next one.",
            ex("nohlsearch"),
        )
        .shown_as("noh"),
        Command::new(
            "toggle-relative-numbers",
            "Toggle relative line numbers",
            "Switches the gutter between distances from the cursor and line numbers.",
            ex("set relativenumber!"),
        )
        .shown_as("set rnu!"),
        Command::new(
            "toggle-ignore-case",
            "Toggle ignore case in search",
            "With it on, lowercase searches ignore case; a capital still matches case.",
            ex("set ignorecase!"),
        )
        .shown_as("set ic!"),
        Command::new(
            "sort",
            "Sort lines",
            "Sorts the whole note by line. Select lines first and use :sort for part of it.",
            ex("%sort"),
        )
        .shown_as("%sort"),
        Command::new("undo", "Undo", "Undoes the last change.", ex("undo"))
            .keys("u")
            .shown_as("undo"),
        Command::new("redo", "Redo", "Redoes the last undone change.", ex("redo"))
            .keys("⌃r")
            .shown_as("redo"),
        Command::new(
            "registers",
            "Show registers",
            "Lists what each register holds.",
            ex("registers"),
        )
        .shown_as("reg"),
        Command::new(
            "command-line",
            "Open the command line",
            "The Vim command line, with live :s preview, ⇥ completion and history.",
            keys(":"),
        )
        .keys(":"),
        Command::new(
            "command-history",
            "Command-line history",
            "The command history as a buffer you can edit; ⏎ runs a line.",
            keys("q:"),
        )
        .keys("q:"),
        Command::new(
            "copen",
            "Show the results list",
            "The places kept with ⌃q from a text search.",
            act(|e, _, cx| e.open_results_list(cx)),
        )
        .shown_as("copen")
        .ex("copen", 4, ArgKind::None),
        Command::new(
            "cnext",
            "Next result",
            "Goes to the next place in the results list.",
            act(|e, _, cx| e.list_step(1, cx)),
        )
        .shown_as("cnext")
        .ex("cnext", 2, ArgKind::None),
        Command::new(
            "cprevious",
            "Previous result",
            "Goes to the previous place in the results list.",
            act(|e, _, cx| e.list_step(-1, cx)),
        )
        .shown_as("cprevious")
        .ex("cprevious", 2, ArgKind::None),
        pane(
            Command::new(
                "vsplit",
                "Split side by side",
                "A new pane beside this one, on this note or the one named.",
                act(|e, call, cx| e.split_command(true, call.args.as_deref(), cx)),
            )
            .shown_as("vsplit")
            .ex("vsplit", 2, ArgKind::Note),
            'v',
        ),
        pane(
            Command::new(
                "split",
                "Split above and below",
                "A new pane below this one, on this note or the one named.",
                act(|e, call, cx| e.split_command(false, call.args.as_deref(), cx)),
            )
            .shown_as("split")
            .ex("split", 2, ArgKind::Note),
            's',
        ),
        pane(
            pane(
                Command::new(
                    "close",
                    "Close pane",
                    "Closes this pane; the last one stays.",
                    act(|e, _, cx| e.close_command(cx)),
                )
                .shown_as("close")
                .ex("close", 3, ArgKind::None),
                'c',
            ),
            'q',
        ),
        pane(
            Command::new(
                "only",
                "Close other panes",
                "Closes every pane but this one.",
                act(|e, _, cx| e.only_pane(cx)),
            )
            .shown_as("only")
            .ex("only", 2, ArgKind::None),
            'o',
        ),
        Command::new(
            "tab-new",
            "New tab",
            "A new tab after this one, on the note named or an untitled note.",
            act(|e, call, cx| e.tab_new(call.args.as_deref(), cx)),
        )
        .bind("<D-t>")
        .shown_as("tabnew")
        .ex("tabnew", 6, ArgKind::Note),
        Command::new(
            "tab-next",
            "Next tab",
            "Shows the next tab, round the window; :tabnext N shows tab N, # the last used.",
            act(|e, call, cx| e.tab_next(call.args.as_deref(), cx)),
        )
        .bind("<C-Tab>")
        .bind("<C-PageDown>")
        .bind("<D-S-]>")
        .shown_as("tabnext")
        .ex("tabnext", 4, ArgKind::Any),
        Command::new(
            "tab-previous",
            "Previous tab",
            "Shows the previous tab, round the window; :tabprevious N goes back N.",
            act(|e, call, cx| e.tab_previous(call.args.as_deref(), cx)),
        )
        .bind("<C-S-Tab>")
        .bind("<C-PageUp>")
        .bind("<D-S-[>")
        .shown_as("tabprevious")
        .ex("tabprevious", 4, ArgKind::Any),
        Command::new(
            "tab-close",
            "Close tab",
            "Closes this tab and its panes (or tab N); the last-used tab shows. :tabreopen brings it back.",
            act(|e, call, cx| e.tab_close(call.args.as_deref(), call.bang, cx)),
        )
        .bind("<D-S-w>")
        .shown_as("tabclose")
        .ex("tabclose", 4, ArgKind::Any),
        Command::new(
            "tab-only",
            "Close other tabs",
            "Closes every tab but this one.",
            act(|e, call, cx| e.tab_only(call.bang, cx)),
        )
        .shown_as("tabonly")
        .ex("tabonly", 4, ArgKind::None),
        Command::new(
            "tab-reopen",
            "Reopen closed tab",
            "Brings back the tab closed last, with its panes, where it was.",
            act(|e, _, cx| e.tab_reopen(cx)),
        )
        .bind("<D-S-t>")
        .shown_as("tabreopen")
        .ex("tabreopen", 6, ArgKind::None),
        Command::new(
            "tab-recent",
            "Last-used tab",
            "Goes back to the tab used before this one (g⇥, :tabnext #).",
            act(|e, _, cx| e.tab_next(Some("#"), cx)),
        ),
        Command::new(
            "tab-last",
            "Last tab",
            "Shows the last tab.",
            act(|e, _, cx| e.tab_edge(true, cx)),
        )
        .bind("<D-9>")
        .shown_as("tablast")
        .ex("tablast", 4, ArgKind::None),
        Command::new(
            "tab-move",
            "Move tab",
            "Moves this tab: to the end, after tab N (0 is first), or +N / -N places.",
            act(|e, call, cx| e.tab_move(call.args.as_deref(), cx)),
        )
        .shown_as("tabmove")
        .ex("tabmove", 4, ArgKind::Any),
        Command::new(
            "tab-to-pane",
            "Move tab to a new pane",
            "The active tab leaves this pane for a new one on the right.",
            act(|e, _, cx| e.tab_to_pane(cx)),
        )
        .bind("<C-w>T"),
        Command::new(
            "close-something",
            "Close",
            "Closes one thing: an overlay, else this pane, else this tab. It never quits mNote (⌘Q does).",
            act(|e, _, cx| e.close_something(cx)),
        )
        .bind("<D-w>")
        .over_overlays(),
        Command::new(
            "tab-list",
            "List tabs",
            "Every tab and the notes of its panes; > marks the focused pane, + unsaved text.",
            act(|e, _, _| e.tab_list()),
        )
        .shown_as("tabs")
        .ex("tabs", 4, ArgKind::None),
        Command::new(
            "save-as",
            "Save as",
            "Saves the note under another path, which it is from then on; an untitled note gets its name.",
            act(|e, call, cx| e.save_as(call.args.as_deref(), call.bang, cx)),
        )
        .bind("<D-S-s>")
        .shown_as("saveas")
        .ex("saveas", 3, ArgKind::Note),
        pane(
            Command::new(
                "pane-next",
                "Next pane",
                "Moves to the next pane, round the window.",
                act(|e, _, cx| e.cycle_pane(1, cx)),
            ),
            'w',
        ),
        Command::new(
            "pane-previous",
            "Previous pane",
            "Moves to the previous pane, round the window.",
            act(|e, _, cx| e.cycle_pane(-1, cx)),
        )
        .bind("<C-w>W"),
        pane(
            Command::new(
                "pane-left",
                "Pane to the left",
                "Moves to the pane on the left.",
                act(|e, _, cx| e.focus_dir(super::panes::Dir::Left, cx)),
            )
            .bind("<C-w><Left>"),
            'h',
        ),
        pane(
            Command::new(
                "pane-down",
                "Pane below",
                "Moves to the pane below.",
                act(|e, _, cx| e.focus_dir(super::panes::Dir::Down, cx)),
            )
            .bind("<C-w><Down>"),
            'j',
        ),
        pane(
            Command::new(
                "pane-up",
                "Pane above",
                "Moves to the pane above.",
                act(|e, _, cx| e.focus_dir(super::panes::Dir::Up, cx)),
            )
            .bind("<C-w><Up>"),
            'k',
        ),
        pane(
            Command::new(
                "pane-right",
                "Pane to the right",
                "Moves to the pane on the right.",
                act(|e, _, cx| e.focus_dir(super::panes::Dir::Right, cx)),
            )
            .bind("<C-w><Right>"),
            'l',
        ),
        Command::new(
            "copy",
            "Copy",
            "Copies the selection, or the cursor line, to the system clipboard.",
            act(|e, _, cx| {
                e.clipboard_shortcut("c", cx);
            }),
        )
        .bind("<D-c>")
        .over_overlays(),
        Command::new(
            "cut",
            "Cut",
            "Cuts the selection, or the cursor line, to the system clipboard.",
            act(|e, _, cx| {
                e.clipboard_shortcut("x", cx);
            }),
        )
        .bind("<D-x>")
        .over_overlays(),
        Command::new(
            "paste",
            "Paste",
            "Pastes the system clipboard at the cursor.",
            act(|e, _, cx| {
                e.clipboard_shortcut("v", cx);
            }),
        )
        .bind("<D-v>")
        .over_overlays(),
        Command::new(
            "select-all",
            "Select all",
            "Selects the whole note.",
            act(|e, _, cx| {
                e.clipboard_shortcut("a", cx);
            }),
        )
        .bind("<D-a>")
        .over_overlays(),
        Command::new(
            "keys",
            "Show key bindings",
            "Lists every key binding and the command it runs; change them in [keys] in settings.",
            act(|e, _, _| e.list_bindings()),
        )
        .shown_as("keys")
        .ex("keys", 3, ArgKind::None),
        Command::new(
            "plugins",
            "Show plugins",
            "Lists installed plugins: version, whether each runs, and its load time.",
            act(|e, _, _| e.list_plugins()),
        )
        .shown_as("plugins")
        .ex("plugins", 4, ArgKind::None),
        Command::new(
            "messages",
            "Show messages",
            "The messages shown so far (toasts, problems with plugins or language packs), oldest first.",
            act(|e, _, _| e.list_messages()),
        )
        .shown_as("messages")
        .ex("messages", 3, ArgKind::None),
        Command::new(
            "permissions",
            "Show permissions",
            "What mNote was allowed to run (plugins, later grammars and servers); pick one to take it back.",
            act(|e, _, cx| e.list_permissions(cx)),
        )
        .shown_as("permissions")
        .ex("permissions", 5, ArgKind::None),
        Command::new(
            "plugin-api",
            "Show the plugin API",
            "Every mnote function and event a plugin can use, in short.",
            act(|e, _, _| e.list_plugin_api()),
        )
        .shown_as("pluginapi")
        .ex("pluginapi", 7, ArgKind::None),
        Command::new(
            "quit",
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
            "tabedit",
            4,
            ArgKind::Note,
            "Opens a note in a new tab after this one.",
            act(|e, call, cx| e.tab_new(call.args.as_deref(), cx)),
        ),
        Command::ex_only(
            "tabNext",
            4,
            ArgKind::Any,
            "Shows the previous tab.",
            act(|e, call, cx| e.tab_previous(call.args.as_deref(), cx)),
        ),
        Command::ex_only(
            "tabfirst",
            6,
            ArgKind::None,
            "Shows the first tab.",
            act(|e, _, cx| e.tab_edge(false, cx)),
        ),
        Command::ex_only(
            "tabrewind",
            4,
            ArgKind::None,
            "Shows the first tab.",
            act(|e, _, cx| e.tab_edge(false, cx)),
        ),
        Command::ex_only(
            "qall",
            2,
            ArgKind::None,
            "Closes every tab and quits mNote.",
            act(|e, call, cx| e.quit_all(false, call.bang, cx)),
        ),
        Command::ex_only(
            "wqall",
            3,
            ArgKind::None,
            "Saves every note, closes every tab and quits mNote.",
            act(|e, call, cx| e.quit_all(true, call.bang, cx)),
        ),
        Command::ex_only(
            "xall",
            2,
            ArgKind::None,
            "Saves every note, closes every tab and quits mNote.",
            act(|e, call, cx| e.quit_all(true, call.bang, cx)),
        ),
        Command::ex_only(
            "edit",
            1,
            ArgKind::Note,
            "Opens a note or a file of the vault by its path (a note can leave out .md).",
            act(|e, call, cx| e.edit_path(call.args.as_deref(), cx)),
        ),
        Command::ex_only(
            "setfiletype",
            4,
            ArgKind::Any,
            "Parses and colours the buffer as another language (setf rust); with no name, says which it is.",
            act(|e, call, cx| e.set_filetype(call.args.as_deref(), cx)),
        ),
        Command::ex_only(
            "find",
            3,
            ArgKind::Note,
            "Finds a note.",
            act(|e, _, _| e.not_yet(":find")),
        )
        .with_id("find-command"),
        Command::ex_only(
            "cNext",
            2,
            ArgKind::None,
            "Goes to the previous place in the results list.",
            act(|e, _, cx| e.list_step(-1, cx)),
        ),
    ];
    // ⌘1 … ⌘8: tab N (⌘9 is the last tab, above).
    all.extend((1..=8).map(|n| {
        Command::new(
            &format!("tab-{n}"),
            &format!("Tab {n}"),
            &format!("Shows tab {n}, if there is one."),
            act(move |e, _, cx| e.tab_number(n, cx)),
        )
        .bind(&format!("<D-{n}>"))
    }));
    all
}
