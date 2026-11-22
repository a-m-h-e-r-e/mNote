//! mNote's settings, from `~/.config/mnote/config.toml` (or
//! `$XDG_CONFIG_HOME/mnote/config.toml`). Every setting has a default, so
//! the file can hold only what you change. A mistake (an unknown setting, a
//! wrong value) is reported with its line, and nothing from that file is
//! used until it's fixed.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::NotePath;

/// What the window follows for dark or light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    #[default]
    System,
    Dark,
    Light,
}

/// How notes are shown when they open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewChoice {
    #[default]
    Styled,
    Source,
    Plain,
}

/// A global key: modifiers and one key, as written in the config
/// (`option+n`, `ctrl+shift+space`, `⌥M`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySpec {
    pub option: bool,
    pub control: bool,
    pub command: bool,
    pub shift: bool,
    /// Lowercase: a letter, digit or punctuation key, or `space`.
    pub key: String,
}

impl KeySpec {
    /// Reads `option+n`, `alt+n`, `cmd+shift+space`, `⌥N`, `⌃⇧;`.
    pub fn parse(text: &str) -> Option<KeySpec> {
        let mut spec = KeySpec {
            option: false,
            control: false,
            command: false,
            shift: false,
            key: String::new(),
        };
        let text = text.trim();
        // Symbols first (`⌥N`), then words joined by `+`.
        let mut rest = text;
        while let Some(c) = rest.chars().next() {
            let flag = match c {
                '⌥' => &mut spec.option,
                '⌃' => &mut spec.control,
                '⌘' => &mut spec.command,
                '⇧' => &mut spec.shift,
                _ => break,
            };
            *flag = true;
            rest = &rest[c.len_utf8()..];
        }
        let parts: Vec<&str> = rest.split('+').map(str::trim).collect();
        let (key, mods) = parts.split_last()?;
        for m in mods {
            let flag = match m.to_ascii_lowercase().as_str() {
                "option" | "opt" | "alt" => &mut spec.option,
                "control" | "ctrl" => &mut spec.control,
                "command" | "cmd" => &mut spec.command,
                "shift" => &mut spec.shift,
                _ => return None,
            };
            *flag = true;
        }
        let key = key.to_lowercase();
        let one_char = key.chars().count() == 1 && key.chars().all(|c| c.is_ascii_graphic());
        if !(one_char || key == "space") {
            return None;
        }
        // A global key needs a modifier, or it would take over typing.
        if !(spec.option || spec.control || spec.command) {
            return None;
        }
        spec.key = key;
        Some(spec)
    }

    /// As hints show it: `⌥N`.
    pub fn label(&self) -> String {
        let mut out = String::new();
        for (on, symbol) in [
            (self.control, '⌃'),
            (self.option, '⌥'),
            (self.shift, '⇧'),
            (self.command, '⌘'),
        ] {
            if on {
                out.push(symbol);
            }
        }
        if self.key == "space" {
            out.push_str("Space");
        } else {
            out.push_str(&self.key.to_uppercase());
        }
        out
    }
}

/// Every setting, with defaults for what the file leaves out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The notes folder, used when mNote starts without one.
    pub vault: Option<PathBuf>,
    /// Brings the window forward from any app.
    pub summon: KeySpec,
    /// Opens quick capture from any app.
    pub capture: KeySpec,
    pub theme: ThemeChoice,
    pub view: ViewChoice,
    pub relative_numbers: bool,
    /// Today's note, with `{date}` (or `{year}`, `{month}`, `{day}`).
    pub daily_note: String,
    /// Who last changed the cursor line, and when, from git.
    pub git_blame: bool,
    /// The note's text font (and the code, inputs and status line).
    pub font: String,
    /// Headings and the interface.
    pub ui_font: String,
    /// The note text's size in pixels; headings and code scale with it.
    pub font_size: u32,
    /// The interface's text size in pixels (menus, pickers, bars, hints).
    pub ui_font_size: u32,
    /// The key before mNote's own commands (`space f` finds notes).
    pub leader: char,
    /// `y`, `d` and `p` use the system clipboard (`clipboard = "system"`),
    /// or keep to Vim's registers (`"vim"`; only `"+` reaches it).
    pub system_clipboard: bool,
    /// `j` / `k` move by screen row through wrapped lines, as `gj` / `gk`.
    pub wrap_motion: bool,
    /// Vim mappings (`"nnoremap j gj"`), run at start and on save; they
    /// replace the built-in `jk` → `<Esc>`. `None` keeps the built-in.
    pub mappings: Option<Vec<String>>,
}

/// The commands `mappings` accepts.
pub const MAPPING_COMMANDS: &[&str] = &[
    "map", "noremap", "unmap", "nmap", "nnoremap", "nunmap", "vmap", "vnoremap", "vunmap", "imap",
    "inoremap", "iunmap",
];

/// The interface sizes `ui_font_size` accepts.
pub const UI_FONT_SIZES: std::ops::RangeInclusive<u32> = 11..=24;

/// The leader as config.toml and hints write it: `space`, or the key.
pub fn leader_name(leader: char) -> String {
    if leader == ' ' {
        "space".into()
    } else {
        leader.to_string()
    }
}

/// The font sizes `font_size` accepts.
pub const FONT_SIZES: std::ops::RangeInclusive<u32> = 10..=32;

impl Default for Config {
    fn default() -> Self {
        Config {
            vault: None,
            summon: KeySpec::parse("option+n").expect("a valid default key"),
            capture: KeySpec::parse("option+m").expect("a valid default key"),
            theme: ThemeChoice::System,
            view: ViewChoice::Styled,
            relative_numbers: true,
            daily_note: "journal/{date}.md".into(),
            git_blame: false,
            font: "IBM Plex Mono".into(),
            ui_font: "IBM Plex Sans".into(),
            font_size: 16,
            ui_font_size: 15,
            leader: ' ',
            system_clipboard: true,
            wrap_motion: false,
            mappings: None,
        }
    }
}

/// A mistake in the config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// 1-based, when known.
    pub line: Option<usize>,
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "config.toml line {line}: {}", self.message),
            None => write!(f, "config.toml: {}", self.message),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Where the config file lives, if there's a home folder.
    pub fn path() -> Option<PathBuf> {
        let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
        };
        Some(base.join("mnote").join("config.toml"))
    }

    /// Reads the config file. `Ok(None)` means there's none yet (first run).
    pub fn load(path: &Path) -> Result<Option<Config>, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text).map(Some),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(ConfigError {
                line: None,
                message: err.to_string(),
            }),
        }
    }

    /// Reads settings from config text.
    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        let table: toml::Table = text.parse().map_err(|err: toml::de::Error| ConfigError {
            line: err.span().map(|span| line_at(text, span.start)),
            message: err.message().trim().to_owned(),
        })?;
        let mut config = Config::default();
        for (key, value) in &table {
            let line = find_key_line(text, key);
            let wrong = |expected: &str| ConfigError {
                line,
                message: format!("{key} should be {expected}"),
            };
            match key.as_str() {
                "vault" => {
                    let path = value
                        .as_str()
                        .ok_or_else(|| wrong("a folder path in quotes"))?;
                    config.vault = Some(expand_home(path));
                }
                "summon" | "capture" => {
                    let spec = value.as_str().and_then(KeySpec::parse).ok_or_else(|| {
                        wrong("a key with option, control or command, like \"option+n\"")
                    })?;
                    if key == "summon" {
                        config.summon = spec;
                    } else {
                        config.capture = spec;
                    }
                }
                "theme" => {
                    config.theme = match value.as_str() {
                        Some("system") => ThemeChoice::System,
                        Some("dark") => ThemeChoice::Dark,
                        Some("light") => ThemeChoice::Light,
                        _ => return Err(wrong("\"system\", \"dark\" or \"light\"")),
                    }
                }
                "view" => {
                    config.view = match value.as_str() {
                        Some("styled") => ViewChoice::Styled,
                        Some("source") => ViewChoice::Source,
                        Some("plain") => ViewChoice::Plain,
                        _ => return Err(wrong("\"styled\", \"source\" or \"plain\"")),
                    }
                }
                "font" | "ui_font" => {
                    let name = value
                        .as_str()
                        .map(str::trim)
                        .filter(|n| !n.is_empty())
                        .ok_or_else(|| wrong("a font name in quotes"))?;
                    if key == "font" {
                        config.font = name.to_owned();
                    } else {
                        config.ui_font = name.to_owned();
                    }
                }
                "font_size" => {
                    config.font_size = value
                        .as_integer()
                        .and_then(|n| u32::try_from(n).ok())
                        .filter(|n| FONT_SIZES.contains(n))
                        .ok_or_else(|| wrong("a whole number from 10 to 32"))?;
                }
                "ui_font_size" => {
                    config.ui_font_size = value
                        .as_integer()
                        .and_then(|n| u32::try_from(n).ok())
                        .filter(|n| UI_FONT_SIZES.contains(n))
                        .ok_or_else(|| wrong("a whole number from 11 to 24"))?;
                }
                "leader" => {
                    let text = value.as_str().unwrap_or("");
                    let mut chars = text.chars();
                    config.leader = match (text, chars.next(), chars.next()) {
                        ("space", _, _) => ' ',
                        (_, Some(c), None) if c.is_ascii_graphic() => c,
                        _ => return Err(wrong("\"space\" or one key, like \",\"")),
                    };
                }
                "clipboard" => {
                    config.system_clipboard = match value.as_str() {
                        Some("system") => true,
                        Some("vim") => false,
                        _ => return Err(wrong("\"system\" or \"vim\"")),
                    }
                }
                "mappings" => {
                    let lines = value
                        .as_array()
                        .and_then(|items| {
                            items
                                .iter()
                                .map(|i| i.as_str().map(str::to_owned))
                                .collect::<Option<Vec<_>>>()
                        })
                        .ok_or_else(|| {
                            wrong("a list of mapping lines, like [\"nnoremap j gj\"]")
                        })?;
                    config.mappings = Some(lines);
                }
                "wrap_motion" => {
                    config.wrap_motion = value.as_bool().ok_or_else(|| wrong("true or false"))?;
                }
                "git_blame" => {
                    config.git_blame = value.as_bool().ok_or_else(|| wrong("true or false"))?;
                }
                "relative_numbers" => {
                    config.relative_numbers =
                        value.as_bool().ok_or_else(|| wrong("true or false"))?;
                }
                "daily_note" => {
                    let pattern = value.as_str().ok_or_else(|| wrong("a path in quotes"))?;
                    daily_note_path(pattern, 2026, 1, 1).map_err(|_| {
                        wrong("a note path inside the vault ending in .md, like \"journal/{date}.md\"")
                    })?;
                    config.daily_note = pattern.to_owned();
                }
                other => {
                    return Err(ConfigError {
                        line,
                        message: format!("there's no setting called {other}"),
                    });
                }
            }
        }
        Ok(config)
    }

    /// Today's note for this config.
    pub fn daily_note(&self, year: i32, month: u32, day: u32) -> NotePath {
        daily_note_path(&self.daily_note, year, month, day)
            .unwrap_or_else(|_| crate::capture::daily_note(year, month, day))
    }

    /// A commented config file holding these settings, for first run and
    /// for `space ,` when there's no file yet.
    pub fn template(&self) -> String {
        let vault = self
            .vault
            .as_ref()
            .map_or_else(|| "~/notes".to_owned(), |v| collapse_home(v));
        let theme = match self.theme {
            ThemeChoice::System => "system",
            ThemeChoice::Dark => "dark",
            ThemeChoice::Light => "light",
        };
        let view = match self.view {
            ViewChoice::Styled => "styled",
            ViewChoice::Source => "source",
            ViewChoice::Plain => "plain",
        };
        format!(
            "# mNote settings. Saving this note applies them.\n\
             \n\
             # Your notes folder, used when mNote starts without one.\n\
             vault = \"{vault}\"\n\
             \n\
             # Global keys: option, control, command and shift with a letter,\n\
             # digit, punctuation key or space.\n\
             summon = \"{summon}\"\n\
             capture = \"{capture}\"\n\
             \n\
             # \"system\", \"dark\" or \"light\".\n\
             theme = \"{theme}\"\n\
             \n\
             # How notes open: \"styled\", \"source\" or \"plain\" (space m cycles).\n\
             view = \"{view}\"\n\
             \n\
             # Line numbers count from the cursor.\n\
             relative_numbers = {rnu}\n\
             \n\
             # Today's note for :daily and quick capture. {{date}} is 2026-11-22;\n\
             # {{year}}, {{month}} and {{day}} work too.\n\
             daily_note = \"{daily}\"\n\
             \n\
             # Who last changed the cursor line, and when, if the vault is in git.\n\
             git_blame = {blame}\n\
             \n\
             # The note's font, and the font for headings and the interface\n\
             # (any installed font; IBM Plex comes with mNote).\n\
             font = \"{font}\"\n\
             ui_font = \"{ui_font}\"\n\
             \n\
             # Note text size in pixels, 10 to 32; headings and code follow.\n\
             font_size = {font_size}\n\
             \n\
             # Interface text size in pixels, 11 to 24: menus, pickers, bars.\n\
             ui_font_size = {ui_font_size}\n\
             \n\
             # The key before mNote's commands (\"space\" or one key, like \",\").\n\
             leader = \"{leader}\"\n\
             \n\
             # \"system\": y, d and p use the system clipboard (and ⌘C ⌘V).\n\
             # \"vim\": they keep to Vim's registers; \"+ reaches the clipboard.\n\
             clipboard = \"{clipboard}\"\n\
             \n\
             # j and k move by screen row through wrapped lines (as gj and gk).\n\
             wrap_motion = {wrap_motion}\n\
             \n\
             # Vim mappings, like in a vimrc (map, noremap, nmap, nnoremap, vmap,\n\
             # vnoremap, imap, inoremap and the unmap commands).\n\
             mappings = [{mappings}]\n",
            summon = key_words(&self.summon),
            capture = key_words(&self.capture),
            rnu = self.relative_numbers,
            daily = self.daily_note,
            blame = self.git_blame,
            font = self.font,
            ui_font = self.ui_font,
            font_size = self.font_size,
            ui_font_size = self.ui_font_size,
            leader = leader_name(self.leader),
            clipboard = if self.system_clipboard {
                "system"
            } else {
                "vim"
            },
            wrap_motion = self.wrap_motion,
            mappings = self
                .mappings
                .clone()
                .unwrap_or_else(|| vec!["inoremap jk <Esc>".to_owned()])
                .iter()
                .map(|m| format!("{m:?}"))
                .collect::<Vec<_>>()
                .join(", "),
        )
    }
}

/// A key as the template writes it: `option+n`.
fn key_words(spec: &KeySpec) -> String {
    let mut parts = Vec::new();
    for (on, word) in [
        (spec.control, "control"),
        (spec.option, "option"),
        (spec.shift, "shift"),
        (spec.command, "command"),
    ] {
        if on {
            parts.push(word.to_owned());
        }
    }
    parts.push(spec.key.clone());
    parts.join("+")
}

/// Writes the config file atomically (a temporary file renamed over it),
/// making its folder if needed.
pub fn write_file(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".config.toml.mnote-tmp-{}", std::process::id()));
    let written = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The daily note's path for a date.
pub fn daily_note_path(pattern: &str, year: i32, month: u32, day: u32) -> crate::Result<NotePath> {
    let path = pattern
        .replace("{date}", &format!("{year:04}-{month:02}-{day:02}"))
        .replace("{year}", &format!("{year:04}"))
        .replace("{month}", &format!("{month:02}"))
        .replace("{day}", &format!("{day:02}"));
    NotePath::new(path)
}

/// `~/notes` → `/Users/you/notes`.
pub fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ if path == "~" => {
            std::env::var_os("HOME").map_or_else(|| PathBuf::from(path), PathBuf::from)
        }
        _ => PathBuf::from(path),
    }
}

/// `/Users/you/notes` → `~/notes`.
pub fn collapse_home(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

fn line_at(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].matches('\n').count() + 1
}

/// The line a top-level key is set on, for errors about its value.
fn find_key_line(text: &str, key: &str) -> Option<usize> {
    text.lines()
        .position(|line| {
            let line = line.trim_start();
            line.strip_prefix(key)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        })
        .map(|i| i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_is_all_defaults() {
        assert_eq!(Config::parse(""), Ok(Config::default()));
        let config = Config::default();
        assert_eq!(config.summon.label(), "⌥N");
        assert_eq!(config.capture.label(), "⌥M");
    }

    #[test]
    fn the_template_reads_back_as_the_same_settings() {
        let config = Config {
            vault: Some(PathBuf::from("/tmp/notes")),
            theme: ThemeChoice::Light,
            view: ViewChoice::Source,
            relative_numbers: false,
            git_blame: true,
            font: "Menlo".into(),
            font_size: 18,
            ui_font_size: 17,
            leader: ',',
            system_clipboard: false,
            wrap_motion: true,
            mappings: Some(vec!["nnoremap j gj".into(), "inoremap jk <Esc>".into()]),
            ..Config::default()
        };
        assert_eq!(Config::parse(&config.template()), Ok(config));
    }

    #[test]
    fn settings_are_read() {
        let config = Config::parse(
            "theme = \"dark\"\nsummon = \"ctrl+shift+space\"\ndaily_note = \"days/{year}/{month}-{day}.md\"\n",
        )
        .expect("valid");
        assert_eq!(config.theme, ThemeChoice::Dark);
        assert_eq!(config.summon.label(), "⌃⇧Space");
        assert_eq!(
            config.daily_note(2026, 11, 2).to_string(),
            "days/2026/11-02.md"
        );
    }

    #[test]
    fn mistakes_name_their_line() {
        let err = Config::parse("view = \"styled\"\nthme = \"dark\"\n").expect_err("typo");
        assert_eq!(
            err.to_string(),
            "config.toml line 2: there's no setting called thme"
        );
        let err = Config::parse("\n\nrelative_numbers = \"yes\"\n").expect_err("wrong type");
        assert_eq!(err.line, Some(3));
        let err = Config::parse("summon = \"n\"").expect_err("no modifier");
        assert!(err.message.contains("option"));
        let err = Config::parse("daily_note = \"../out.md\"").expect_err("leaves the vault");
        assert_eq!(err.line, Some(1));
        let err = Config::parse("font_size = 64").expect_err("too big");
        assert!(err.message.contains("10 to 32"));
        let err = Config::parse("font = \"\"").expect_err("no name");
        assert_eq!(err.line, Some(1));
        let err = Config::parse("leader = \"ab\"").expect_err("two keys");
        assert!(err.message.contains("one key"));
        assert_eq!(
            Config::parse("leader = \"space\"").map(|c| c.leader),
            Ok(' ')
        );
        let err = Config::parse("ui_font_size = 40").expect_err("too big");
        assert!(err.message.contains("11 to 24"));
        let err = Config::parse("\nmappings = [1]").expect_err("not strings");
        assert_eq!(err.line, Some(2));
        assert_eq!(
            Config::parse("mappings = [\"nnoremap j gj\"]").map(|c| c.mappings),
            Ok(Some(vec!["nnoremap j gj".to_owned()]))
        );
        let err = Config::parse("view = ").expect_err("not toml");
        assert_eq!(err.line, Some(1));
    }

    #[test]
    fn writing_makes_the_folder_and_replaces_the_file() {
        let dir = std::env::temp_dir().join(format!("mnote-config-{}", std::process::id()));
        let path = dir.join("mnote").join("config.toml");
        write_file(&path, "theme = \"dark\"\n").expect("write");
        write_file(&path, "theme = \"light\"\n").expect("write again");
        let config = Config::load(&path).expect("valid").expect("exists");
        assert_eq!(config.theme, ThemeChoice::Light);
        assert_eq!(Config::load(&dir.join("none.toml")), Ok(None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_in_symbols_and_words() {
        let a = KeySpec::parse("⌥M").expect("symbols");
        let b = KeySpec::parse("Option+M").expect("words");
        assert_eq!(a, b);
        assert_eq!(
            KeySpec::parse("cmd+escape"),
            None,
            "named keys other than space aren't supported"
        );
        assert_eq!(KeySpec::parse("hyper+m"), None);
    }
}
