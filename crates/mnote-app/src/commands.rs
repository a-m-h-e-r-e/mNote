//! Every command mNote offers, in one registry: what it's called in the
//! command palette (`⌘K`) and the finder's `>` scope, the keys and ex form
//! shown beside it, the app's ex command it registers with Vim (`:daily`,
//! `:split`), its leader key, and what it does (decision D-002: the palette
//! shows ex equivalents; `:` stays the Vim command line).
//!
//! The built-in commands are registered in [`crate::editor::commands`];
//! adding one is one entry there. Vim learns the app's ex commands from
//! here ([`install`]), so parsing, abbreviations, completion and colours
//! follow the registry.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::Context;
use mnote_vim::ex::{ArgKind, CommandSpec};

use crate::editor::Editor;

/// How a command was called: its argument and `!`, from the command line
/// (nothing from the palette or a key).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Invocation {
    pub args: Option<String>,
    pub bang: bool,
}

/// Rust code a command runs.
pub type Action = Rc<dyn Fn(&mut Editor, &Invocation, &mut Context<Editor>)>;

/// How a command runs.
#[derive(Clone)]
pub enum Run {
    /// An ex command, as if typed after `:` (Vim's own, like `undo`).
    Ex(String),
    /// Opens the finder with this query (`/` for text, `#` for tags).
    Finder(String),
    /// Types these keys in NORMAL mode (`q:`, `/`); a leading space is the
    /// leader.
    Keys(String),
    /// Code in the app.
    Action(Action),
}

impl std::fmt::Debug for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Run::Ex(ex) => write!(f, "Ex({ex:?})"),
            Run::Finder(q) => write!(f, "Finder({q:?})"),
            Run::Keys(k) => write!(f, "Keys({k:?})"),
            Run::Action(_) => f.write_str("Action"),
        }
    }
}

/// The app ex command a command adds to Vim (`:daily`, `:split`).
#[derive(Debug, Clone)]
pub struct ExName {
    pub name: String,
    /// The shortest abbreviation accepted (`split` takes `sp`: 2).
    pub abbrev: usize,
    pub args: ArgKind,
}

/// A command in the registry.
#[derive(Debug, Clone)]
pub struct Command {
    /// Its name in the palette; `None` keeps it out (ex-only, like `:wq`).
    pub label: Option<String>,
    /// The key binding, as shown in hints (`space f`). A `space …` binding
    /// is the leader key that runs it.
    pub keys: Option<String>,
    /// The ex form shown beside it, without the `:` (`set rnu!`).
    pub ex_form: Option<String>,
    /// The ex command it registers with Vim, if any.
    pub ex: Option<ExName>,
    /// One sentence, for the palette's footer and ex completion.
    pub about: String,
    pub run: Run,
}

impl Command {
    /// A palette command.
    pub fn new(label: &str, about: &str, run: Run) -> Self {
        Command {
            label: Some(label.to_owned()),
            keys: None,
            ex_form: None,
            ex: None,
            about: about.to_owned(),
            run,
        }
    }

    /// An ex command that isn't in the palette (`:wq`, `:vsplit`).
    pub fn ex_only(name: &str, abbrev: usize, args: ArgKind, about: &str, run: Run) -> Self {
        Command {
            label: None,
            keys: None,
            ex_form: None,
            ex: Some(ExName {
                name: name.to_owned(),
                abbrev,
                args,
            }),
            about: about.to_owned(),
            run,
        }
    }

    /// Its key binding.
    pub fn keys(mut self, keys: &str) -> Self {
        self.keys = Some(keys.to_owned());
        self
    }

    /// The ex form shown beside it.
    pub fn shown_as(mut self, ex_form: &str) -> Self {
        self.ex_form = Some(ex_form.to_owned());
        self
    }

    /// Registers `:name` with Vim for it (shown as `shown`, if given).
    pub fn ex(mut self, name: &str, abbrev: usize, args: ArgKind) -> Self {
        self.ex = Some(ExName {
            name: name.to_owned(),
            abbrev,
            args,
        });
        self
    }

    /// The quiet text at the right of its row: keys, then the ex form.
    pub fn detail(&self) -> String {
        let ex = self.ex_form.as_ref().map(|ex| format!(":{ex}"));
        let keys = self.keys.as_deref().map(with_leader);
        match (keys, ex) {
            (Some(keys), Some(ex)) => format!("{keys} · {ex}"),
            (Some(keys), None) => keys,
            (None, Some(ex)) => ex,
            (None, None) => String::new(),
        }
    }

    /// What a query matches: the label, the keys and the ex form.
    pub fn haystack(&self) -> String {
        format!("{} {}", self.label.as_deref().unwrap_or(""), self.detail())
    }
}

thread_local! {
    // The leader key from config.toml, for hints and replayed keys (UI
    // thread only).
    static LEADER: std::cell::Cell<char> = const { std::cell::Cell::new(' ') };
    // Every command, in the palette's order (UI thread only). Commands are
    // only added, so an index stays valid.
    static REGISTRY: RefCell<Rc<Vec<Command>>> = RefCell::new(Rc::new(Vec::new()));
}

/// Makes `commands` the registry, and tells Vim their ex commands.
pub fn install(commands: Vec<Command>) {
    REGISTRY.with(|r| *r.borrow_mut() = Rc::new(commands));
    sync_vim();
}

/// Adds a command (for plugins, later): it shows in the palette and its ex
/// command parses at once.
pub fn register(command: Command) {
    REGISTRY.with(|r| Rc::make_mut(&mut r.borrow_mut()).push(command));
    sync_vim();
}

fn sync_vim() {
    let specs = all()
        .iter()
        .filter_map(|c| {
            let ex = c.ex.as_ref()?;
            Some(CommandSpec {
                name: ex.name.clone(),
                abbrev: ex.abbrev,
                detail: short_detail(c),
                args: ex.args.clone(),
            })
        })
        .collect();
    mnote_vim::ex::set_app_commands(specs);
}

/// What an ex command does, for completion: its palette label, or the
/// first sentence of its description, in lower case first.
fn short_detail(c: &Command) -> String {
    let text = c.label.clone().unwrap_or_else(|| {
        c.about
            .split(". ")
            .next()
            .unwrap_or("")
            .trim_end_matches('.')
            .to_owned()
    });
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_lowercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Every command, in the palette's order.
pub fn all() -> Rc<Vec<Command>> {
    REGISTRY.with(|r| r.borrow().clone())
}

/// The command whose ex command is `name` (its full name).
pub fn by_ex(name: &str) -> Option<usize> {
    all()
        .iter()
        .position(|c| c.ex.as_ref().is_some_and(|ex| ex.name == name))
}

/// The command bound to `keys` (`space f`).
pub fn by_keys(keys: &str) -> Option<usize> {
    all().iter().position(|c| c.keys.as_deref() == Some(keys))
}

/// Sets the leader key (config.toml's `leader`).
pub fn set_leader(leader: char) {
    LEADER.with(|l| l.set(leader));
}

/// The leader key in use.
pub fn leader() -> char {
    LEADER.with(std::cell::Cell::get)
}

/// `keys` as shown, with `space …` bindings under the real leader
/// (`space f` → `, f` when the leader is `,`).
pub fn with_leader(keys: &str) -> String {
    match keys.strip_prefix("space ") {
        Some(rest) => format!("{} {rest}", mnote_core::config::leader_name(leader())),
        None => keys.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(label: &str) -> Command {
        install(crate::editor::commands::builtin());
        all()
            .iter()
            .find(|c| c.label.as_deref() == Some(label))
            .cloned()
            .expect("listed")
    }

    #[test]
    fn details_show_keys_then_ex() {
        assert_eq!(find("Find a note").detail(), "space f");
        let undo = find("Undo");
        assert_eq!(undo.detail(), "u · :undo");
        assert!(undo.haystack().contains(":undo"));
    }

    #[test]
    fn leader_bindings_show_the_real_leader() {
        assert_eq!(with_leader("space f"), "space f");
        set_leader(',');
        assert_eq!(with_leader("space f"), ", f");
        assert_eq!(with_leader("gd"), "gd");
        set_leader(' ');
    }

    #[test]
    fn every_command_is_described_once() {
        install(crate::editor::commands::builtin());
        let all = all();
        for (i, c) in all.iter().enumerate() {
            assert!(c.about.ends_with('.'), "{:?}", c.label);
            let rest = &all[i + 1..];
            if let Some(label) = &c.label {
                assert!(
                    !rest.iter().any(|o| o.label.as_ref() == Some(label)),
                    "{label}"
                );
            }
            if let Some(ex) = &c.ex {
                assert!(
                    !rest
                        .iter()
                        .any(|o| o.ex.as_ref().is_some_and(|e| e.name == ex.name)),
                    ":{}",
                    ex.name
                );
            }
            if let Some(keys) = &c.keys {
                assert!(
                    !rest.iter().any(|o| o.keys.as_ref() == Some(keys)),
                    "{keys}"
                );
            }
        }
    }

    #[test]
    fn lookups_by_ex_name_and_keys() {
        install(crate::editor::commands::builtin());
        let all = all();
        let daily = by_ex("daily").expect(":daily");
        assert_eq!(all[daily].label.as_deref(), Some("Open today's note"));
        assert!(all[by_ex("vsplit").expect(":vsplit")].label.is_none());
        let f = by_keys("space f").expect("space f");
        assert_eq!(all[f].label.as_deref(), Some("Find a note"));
        assert_eq!(by_ex("nope"), None);
    }

    #[test]
    fn vim_learns_the_ex_commands_with_their_abbreviations() {
        install(crate::editor::commands::builtin());
        let mut vim = mnote_vim::Vim::new();
        let mut buf = mnote_core::Buffer::new("x\n");
        let app = |vim: &mut mnote_vim::Vim, buf: &mut mnote_core::Buffer, line: &str| {
            vim.execute(line, buf)
                .unwrap_or_else(|e| panic!("{line}: {e}"))
                .app
                .map(|a| (a.name, a.args, a.bang))
        };
        assert_eq!(
            app(&mut vim, &mut buf, "w"),
            Some(("write".into(), String::new(), false))
        );
        assert_eq!(
            app(&mut vim, &mut buf, "dai"),
            Some(("daily".into(), String::new(), false))
        );
        assert_eq!(
            app(&mut vim, &mut buf, "vs ideas"),
            Some(("vsplit".into(), "ideas".into(), false))
        );
        assert_eq!(
            app(&mut vim, &mut buf, "q!"),
            Some(("quit".into(), String::new(), true))
        );
        // Completion lists them with what they do.
        let names = mnote_vim::ex::complete::candidates("dai", 3, &Default::default(), Vec::new)
            .expect("completes")
            .1;
        assert_eq!(names[0].text, "daily");
        assert!(!names[0].detail.is_empty());
        // Adding one makes it parse at once.
        register(Command::ex_only(
            "hello",
            3,
            ArgKind::Any,
            "Says hello.",
            Run::Ex("noh".into()),
        ));
        assert_eq!(
            app(&mut vim, &mut buf, "hel there"),
            Some(("hello".into(), "there".into(), false))
        );
    }

    #[test]
    fn ex_forms_parse() {
        install(crate::editor::commands::builtin());
        let mut vim = mnote_vim::Vim::new();
        for c in all().iter() {
            for ex in [
                c.ex_form.as_deref(),
                match &c.run {
                    Run::Ex(ex) => Some(ex.as_str()),
                    _ => None,
                },
            ]
            .into_iter()
            .flatten()
            {
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
