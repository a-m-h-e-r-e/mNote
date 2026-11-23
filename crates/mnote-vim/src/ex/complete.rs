//! `⇥` completion on the command line: command names, `:set` options,
//! `:view` modes and note paths. Pure functions over the typed text, so the
//! popup's contents are easy to test.

use super::parse;
use super::{ArgKind, Options};

/// One completion: what gets inserted and a short description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The text that replaces the word being completed (`sort u`).
    pub text: String,
    /// The quiet description at the right of the row (`sort lines`).
    pub detail: String,
}

/// The completion popup while it's open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// Byte offset in the line where the completed word starts.
    pub start: usize,
    /// What was typed there before completing, restored after cycling past
    /// the last candidate.
    pub original: String,
    pub items: Vec<Candidate>,
    /// The candidate in the line now, or `None` for the original text.
    pub selected: Option<usize>,
}

/// Rows the popup shows at once (design rule: at most eight).
pub const VISIBLE_ROWS: usize = 8;

impl Completion {
    /// The rows to show, as `(first index, rows)`, keeping the selection in
    /// view.
    pub fn visible(&self) -> (usize, &[Candidate]) {
        let n = self.items.len();
        let first = self
            .selected
            .map_or(0, |s| (s + 1).saturating_sub(VISIBLE_ROWS))
            .min(n.saturating_sub(VISIBLE_ROWS));
        (first, &self.items[first..(first + VISIBLE_ROWS).min(n)])
    }
}

/// Commands offered by completion, with what each does. Abbreviations that
/// are only aliases (`t`, `display`, `xit`) are left out.
const COMMANDS: &[(&str, &str)] = &[
    ("copy", "copy lines below an address"),
    ("delete", "delete lines"),
    ("global", "run a command on matching lines"),
    ("move", "move lines below an address"),
    ("nohlsearch", "clear search highlights"),
    ("normal", "run normal-mode keys on lines"),
    ("put", "put a register below a line"),
    ("redo", "redo a change"),
    ("registers", "list registers"),
    ("set", "set an option"),
    ("sort", "sort lines"),
    ("sort!", "sort, reversed"),
    ("sort i", "sort, ignoring case"),
    ("sort n", "sort by the first number"),
    ("sort u", "sort, dropping duplicates"),
    ("substitute", "replace matches"),
    ("undo", "undo a change"),
    ("vglobal", "run a command on other lines"),
    ("yank", "yank lines"),
];

/// Paths offered for one `⇥` at most; more makes a list nobody reads.
const MAX_PATHS: usize = 50;

/// Completions for the word before byte `cursor` in an ex command line, and
/// where that word starts. `paths` lists the vault's notes; it's only called
/// when a path is being completed.
pub fn candidates(
    line: &str,
    cursor: usize,
    options: &Options,
    paths: impl FnOnce() -> Vec<String>,
) -> Option<(usize, Vec<Candidate>)> {
    let before = &line[..cursor];
    let lead = before.len() - before.trim_start_matches([' ', ':']).len();
    let (_, rest) = parse::parse_range(&before[lead..]).ok()?;
    let rest_at = cursor - rest.len();
    let trimmed = rest.trim_start();
    let name_at = rest_at + (rest.len() - trimmed.len());

    // Still typing the command's name: Vim's and the app's, by name.
    if trimmed.chars().all(|c| c.is_ascii_alphabetic()) {
        let app = parse::app_commands();
        let mut items: Vec<Candidate> = COMMANDS
            .iter()
            .map(|(name, detail)| (*name, *detail))
            .chain(app.iter().map(|c| (c.name, c.spec.detail.as_str())))
            .filter(|(name, _)| name.starts_with(trimmed))
            .map(|(name, detail)| Candidate {
                text: name.to_owned(),
                detail: detail.to_owned(),
            })
            .collect();
        // By name; variants of one command (`sort`, `sort!`) keep their order.
        items.sort_by_key(|c| {
            let end = c.text.find(|ch: char| !ch.is_ascii_alphabetic());
            c.text[..end.unwrap_or(c.text.len())].to_owned()
        });
        return Some((name_at, items));
    }

    let name_len = trimmed
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(trimmed.len());
    let name = parse::lookup(&trimmed[..name_len])?;
    let args = trimmed[name_len..]
        .strip_prefix('!')
        .unwrap_or(&trimmed[name_len..]);
    if !args.starts_with(' ') {
        return None;
    }
    let word_at = args.rfind(' ').map_or(0, |i| i + 1);
    let word = &args[word_at..];
    let start = cursor - word.len();

    let items = match name {
        "set" => option_candidates(word, options),
        name => match parse::app_command(name)?.args {
            ArgKind::Choice(choices) => choices
                .into_iter()
                .filter(|(v, _)| v.starts_with(word))
                .map(|(text, detail)| Candidate { text, detail })
                .collect(),
            ArgKind::Note => path_candidates(word, paths(), false),
            ArgKind::Folder => path_candidates(word, paths(), true),
            ArgKind::None | ArgKind::Any => return None,
        },
    };
    Some((start, items))
}

fn option_candidates(word: &str, options: &Options) -> Vec<Candidate> {
    let on_off = |on: bool| if on { "on" } else { "off" }.to_owned();
    let all = [
        (
            "shiftwidth",
            format!("indent step, now {}", options.shiftwidth),
        ),
        ("ignorecase", on_off(options.ignorecase)),
        ("smartcase", on_off(options.smartcase)),
        ("relativenumber", on_off(options.relativenumber)),
        ("wrap", on_off(options.wrap)),
        ("noignorecase", "match case".to_owned()),
        ("nosmartcase", "ignore case even with capitals".to_owned()),
        ("norelativenumber", "absolute line numbers".to_owned()),
        ("nowrap", "one row per line, scroll sideways".to_owned()),
    ];
    all.into_iter()
        // `no…` only shows once it's being typed.
        .filter(|(name, _)| {
            name.starts_with(word) && (word.starts_with("no") || !name.starts_with("no"))
        })
        .map(|(name, detail)| Candidate {
            text: name.to_owned(),
            detail,
        })
        .collect()
}

/// Notes (or, with `folders`, their folders) whose path contains `word`,
/// case-insensitively: those starting with it first, then shorter paths.
fn path_candidates(word: &str, paths: Vec<String>, folders: bool) -> Vec<Candidate> {
    let needle = word.to_lowercase();
    let mut found: Vec<String> = if folders {
        let mut dirs: Vec<String> = paths
            .iter()
            .filter_map(|p| p.rsplit_once('/').map(|(dir, _)| format!("{dir}/")))
            .collect();
        dirs.sort_unstable();
        dirs.dedup();
        dirs
    } else {
        paths
    };
    found.retain(|p| p.to_lowercase().contains(&needle));
    found.sort_by_key(|p| (!p.to_lowercase().starts_with(&needle), p.len(), p.clone()));
    found.truncate(MAX_PATHS);
    found
        .into_iter()
        .map(|p| {
            let detail = if folders { "folder" } else { "note" }.to_owned();
            Candidate { text: p, detail }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(line: &str) -> Vec<String> {
        let (_, items) =
            candidates(line, line.len(), &Options::default(), Vec::new).expect("completes");
        items.into_iter().map(|c| c.text).collect()
    }

    #[test]
    fn command_names_after_a_range() {
        crate::ex::tests::test_app_commands();
        assert_eq!(
            names("'<,'>so"),
            ["sort", "sort!", "sort i", "sort n", "sort u"]
        );
        let (start, _) = candidates("'<,'>so", 7, &Options::default(), Vec::new).expect("ok");
        assert_eq!(start, 5, "the name starts after the range");
        assert!(names("").len() > 20, "an empty line lists every command");
        assert!(names("zz").is_empty());
    }

    #[test]
    fn options_views_and_paths() {
        crate::ex::tests::test_app_commands();
        assert_eq!(names("set ig"), ["ignorecase"]);
        assert_eq!(
            names("set no"),
            ["noignorecase", "nosmartcase", "norelativenumber", "nowrap"]
        );
        assert_eq!(names("se sw=4 rel"), ["relativenumber"]);
        assert_eq!(names("view s"), ["styled", "source"]);
        let paths = || {
            vec![
                "inbox.md".to_owned(),
                "projects/mnote/ui-design.md".to_owned(),
                "design.md".to_owned(),
            ]
        };
        let (start, items) = candidates("e des", 5, &Options::default(), paths).expect("ok");
        assert_eq!(start, 2);
        let got: Vec<&str> = items.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(got, ["design.md", "projects/mnote/ui-design.md"]);
        let (_, items) = candidates("explore ", 8, &Options::default(), paths).expect("ok");
        assert_eq!(items[0].text, "projects/mnote/");
        assert!(candidates("sort u", 6, &Options::default(), paths).is_none());
    }

    #[test]
    fn the_popup_scrolls_to_the_selection() {
        let items = (0..12)
            .map(|i| Candidate {
                text: i.to_string(),
                detail: String::new(),
            })
            .collect();
        let mut c = Completion {
            start: 0,
            original: String::new(),
            items,
            selected: Some(2),
        };
        assert_eq!(c.visible().0, 0);
        c.selected = Some(10);
        assert_eq!(c.visible(), (3, &c.items[3..11]));
    }
}
