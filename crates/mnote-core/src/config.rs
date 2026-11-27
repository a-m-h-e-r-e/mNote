//! mNote's settings, from `~/.config/mnote/config.toml` (or
//! `$XDG_CONFIG_HOME/mnote/config.toml`). Every setting has a default, so
//! the file can hold only what you change. A mistake (an unknown setting, a
//! wrong value) is reported with its line, and nothing from that file is
//! used until it's fixed.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::NotePath;

/// The colours: the system's dark or light, one of them always, or a
/// theme file (`~/.config/mnote/themes/<name>.toml`, see [`ThemeFile`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    #[default]
    System,
    Dark,
    Light,
    Named(String),
}

/// A theme file: a base (`dark` or `light`) and colours over it, by token
/// name. Tokens it doesn't set come from the base.
///
/// ```toml
/// base = "dark"
/// [colors]
/// paper = "#1e1e2e"
/// match = "#f9e2af55"
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThemeFile {
    /// `true` for a dark base.
    pub dark: bool,
    /// Token names and colours (`0xRRGGBBAA`).
    pub colors: Vec<(String, u32)>,
    /// `[syntax]`: highlight capture names (`keyword.function`, without the
    /// `@`) and what they're drawn in: a colour token's name or a colour.
    pub syntax: Vec<(String, SyntaxColor)>,
}

/// What a highlight capture is drawn in, in a theme file's `[syntax]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxColor {
    /// A colour token (`syn_keyword`, `ink_3`).
    Token(String),
    /// A colour of its own (`0xRRGGBBAA`).
    Color(u32),
}

impl ThemeFile {
    /// Reads a theme file.
    pub fn parse(text: &str) -> Result<ThemeFile, ConfigError> {
        let table: toml::Table = text.parse().map_err(|err: toml::de::Error| ConfigError {
            line: err.span().map(|span| line_at(text, span.start)),
            message: err.message().trim().to_owned(),
        })?;
        let mut theme = ThemeFile {
            dark: true,
            colors: Vec::new(),
            syntax: Vec::new(),
        };
        for (key, value) in &table {
            let line = find_key_line(text, key);
            let wrong = |message: String| ConfigError { line, message };
            match key.as_str() {
                "base" => {
                    theme.dark = match value.as_str() {
                        Some("dark") => true,
                        Some("light") => false,
                        _ => return Err(wrong("base should be \"dark\" or \"light\"".into())),
                    }
                }
                "colors" => {
                    let colors = value
                        .as_table()
                        .ok_or_else(|| wrong("colors should be a [colors] table".into()))?;
                    for (name, color) in colors {
                        let value = color.as_str().and_then(parse_color).ok_or_else(|| {
                            ConfigError {
                                line: find_key_line(text, name),
                                message: format!(
                                    "{name} should be a colour like \"#1e1e2e\" or \"#1e1e2e80\""
                                ),
                            }
                        })?;
                        theme.colors.push((name.clone(), value));
                    }
                }
                "syntax" => {
                    let table = value
                        .as_table()
                        .ok_or_else(|| wrong("syntax should be a [syntax] table".into()))?;
                    for (capture, color) in table {
                        let bad = || ConfigError {
                            line: find_key_line(text, capture),
                            message: format!(
                                "{capture} should be a colour token like \"syn_keyword\" or a colour like \"#c678dd\""
                            ),
                        };
                        let text_value = color.as_str().ok_or_else(bad)?;
                        let value = if text_value.starts_with('#') {
                            SyntaxColor::Color(parse_color(text_value).ok_or_else(bad)?)
                        } else if !text_value.is_empty() {
                            SyntaxColor::Token(text_value.to_owned())
                        } else {
                            return Err(bad());
                        };
                        theme
                            .syntax
                            .push((capture.trim_start_matches('@').to_owned(), value));
                    }
                }
                other => return Err(wrong(format!("there's no theme setting called {other}"))),
            }
        }
        Ok(theme)
    }

    /// The folder theme files live in: `~/.config/mnote/themes`.
    pub fn dir() -> Option<PathBuf> {
        Config::path()?.parent().map(|p| p.join("themes"))
    }

    /// The file for theme `name`.
    pub fn path(name: &str) -> Option<PathBuf> {
        Some(Self::dir()?.join(format!("{name}.toml")))
    }
}

/// `#rrggbb` or `#rrggbbaa` as `0xRRGGBBAA`.
pub fn parse_color(text: &str) -> Option<u32> {
    let hex = text.trim().strip_prefix('#')?;
    let value = u32::from_str_radix(hex, 16).ok()?;
    match hex.len() {
        6 => Some((value << 8) | 0xff),
        8 => Some(value),
        _ => None,
    }
}

/// How notes are shown when they open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewChoice {
    #[default]
    Styled,
    Source,
    Plain,
}

/// When panes show their tab rows (`show_tabs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabsChoice {
    /// When any pane has two or more tabs, or there are two or more panes.
    #[default]
    Auto,
    Always,
    Never,
}

impl TabsChoice {
    pub fn name(self) -> &'static str {
        match self {
            TabsChoice::Auto => "auto",
            TabsChoice::Always => "always",
            TabsChoice::Never => "never",
        }
    }

    /// Whether tab rows show, with `panes` panes and at most `tabs` tabs
    /// in one of them.
    pub fn shows(self, panes: usize, tabs: usize) -> bool {
        match self {
            TabsChoice::Auto => panes > 1 || tabs > 1,
            TabsChoice::Always => true,
            TabsChoice::Never => false,
        }
    }
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

/// Checks a plugin setting's value: `Err(why)` for one it doesn't take.
pub type SettingCheck = std::sync::Arc<dyn Fn(&toml::Value) -> Result<(), String> + Send + Sync>;

/// A plugin's setting (or a built-in feature's), under `[plugins.<plugin>]`:
/// checked when config.toml is read, and written into it with its comment,
/// values and default like the built-in settings.
#[derive(Clone)]
pub struct PluginSetting {
    pub plugin: String,
    pub key: String,
    /// What it does and the values it takes (each line becomes a `#` line).
    pub help: String,
    pub default: toml::Value,
    /// `Err(why)` for a value it doesn't take.
    pub check: SettingCheck,
}

impl std::fmt::Debug for PluginSetting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PluginSetting({}.{})", self.plugin, self.key)
    }
}

fn plugin_settings() -> &'static std::sync::RwLock<Vec<PluginSetting>> {
    static SETTINGS: std::sync::OnceLock<std::sync::RwLock<Vec<PluginSetting>>> =
        std::sync::OnceLock::new();
    SETTINGS.get_or_init(Default::default)
}

/// Registers a plugin setting (replacing one with the same plugin and key).
pub fn register_plugin_setting(setting: PluginSetting) {
    if let Ok(mut settings) = plugin_settings().write() {
        settings.retain(|s| !(s.plugin == setting.plugin && s.key == setting.key));
        settings.push(setting);
    }
}

fn registered(plugin: &str) -> Vec<PluginSetting> {
    plugin_settings()
        .read()
        .map(|s| s.iter().filter(|s| s.plugin == plugin).cloned().collect())
        .unwrap_or_default()
}

/// Every setting, with defaults for what the file leaves out.
#[derive(Debug, Clone, PartialEq)]
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
    /// Lines kept on screen above and below the cursor (`:set scrolloff`).
    pub scrolloff: u32,
    /// Use Neovim's tree-sitter queries (and later its parsers) when it's
    /// installed.
    pub reuse_neovim: bool,
    /// List the vault's code files in the finder and explorer, beside notes.
    pub show_code_files: bool,
    /// When panes show their tab rows.
    pub show_tabs: TabsChoice,
    /// Notes only looked at (the explorer, results, links) open in one
    /// preview tab that the next replaces; `false` keeps every one.
    pub preview_tabs: bool,
    /// Reopen the vault's panes and tabs as they were left.
    pub restore_session: bool,
    /// In code, an opening bracket or quote typed adds its closer.
    pub auto_pairs: bool,
    /// Show the window's close, minimize and zoom buttons (ADR-0015: they
    /// are hidden, and the tab rows reach the window's top edge).
    pub window_buttons: bool,
    /// Above this many megabytes, code fences and other injections aren't
    /// parsed.
    pub large_file_injections_mb: u32,
    /// Above this many megabytes, nothing is highlighted.
    pub large_file_highlight_mb: u32,
    /// Long lines wrap (`:set wrap`); off, the note scrolls sideways.
    pub wrap: bool,
    /// Vim mappings (`"nnoremap j gj"`), run at start and on save; they
    /// replace the built-in `jk` → `<Esc>`. `None` keeps the built-in.
    pub mappings: Option<Vec<String>>,
    /// Key bindings from the `[keys]` table: keys in Vim notation, and the
    /// command they run (`"<leader>d" = "daily"`; `"none"` removes one).
    pub keys: Vec<(String, String)>,
    /// `[plugins.<name>]` tables, kept as written (see [`PluginSetting`]).
    pub plugins: std::collections::BTreeMap<String, toml::Table>,
    /// The corner radius of pickers, modals, the capture panel, the command
    /// line and toasts, in pixels. `None` keeps the design's radii.
    pub panel_radius: Option<u32>,
    /// Pickers, modals and toasts carry their shadow.
    pub panel_shadow: bool,
    /// Notes save a moment after typing stops. Off, they save on `:w` and
    /// `⌘S`, and still whenever you leave a note, the window or mNote.
    pub autosave: bool,
}

/// The radii `panel_radius` accepts.
pub const PANEL_RADII: std::ops::RangeInclusive<u32> = 0..=24;

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
            font: "Lilex".into(),
            ui_font: "IBM Plex Sans".into(),
            font_size: 15,
            ui_font_size: 15,
            leader: ' ',
            system_clipboard: true,
            wrap_motion: false,
            scrolloff: 0,
            reuse_neovim: true,
            show_code_files: true,
            show_tabs: TabsChoice::Auto,
            preview_tabs: true,
            restore_session: true,
            auto_pairs: true,
            window_buttons: false,
            large_file_injections_mb: 4,
            large_file_highlight_mb: 16,
            wrap: true,
            mappings: None,
            keys: Vec::new(),
            plugins: Default::default(),
            panel_radius: None,
            panel_shadow: true,
            autosave: true,
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
                        Some(name)
                            if !name.is_empty()
                                && name.chars().all(|c| {
                                    c.is_alphanumeric() || matches!(c, '-' | '_' | '.')
                                })
                                && !name.starts_with('.') =>
                        {
                            ThemeChoice::Named(name.to_owned())
                        }
                        _ => {
                            return Err(wrong(
                                "\"system\", \"dark\", \"light\" or a theme file's name",
                            ));
                        }
                    }
                }
                "auto_pairs" => {
                    config.auto_pairs = value.as_bool().ok_or_else(|| wrong("true or false"))?;
                }
                "restore_session" => {
                    config.restore_session =
                        value.as_bool().ok_or_else(|| wrong("true or false"))?;
                }
                "preview_tabs" => {
                    config.preview_tabs = value.as_bool().ok_or_else(|| wrong("true or false"))?;
                }
                "window_buttons" => {
                    config.window_buttons =
                        value.as_bool().ok_or_else(|| wrong("true or false"))?;
                }
                "show_tabs" => {
                    config.show_tabs = match value.as_str() {
                        Some("auto") => TabsChoice::Auto,
                        Some("always") => TabsChoice::Always,
                        Some("never") => TabsChoice::Never,
                        _ => return Err(wrong("\"auto\", \"always\" or \"never\"")),
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
                "plugins" => {
                    let plugins = value
                        .as_table()
                        .ok_or_else(|| wrong("tables, one per plugin: [plugins.<name>]"))?;
                    for (name, table) in plugins {
                        let table = table.as_table().ok_or_else(|| ConfigError {
                            line: find_key_line(text, name),
                            message: format!("plugins.{name} should be a table: [plugins.{name}]"),
                        })?;
                        for setting in registered(name) {
                            if let Some(value) = table.get(&setting.key) {
                                (setting.check)(value).map_err(|why| ConfigError {
                                    line: find_key_line(text, &setting.key),
                                    message: format!("plugins.{name}.{}: {why}", setting.key),
                                })?;
                            }
                        }
                        config.plugins.insert(name.clone(), table.clone());
                    }
                }
                "keys" => {
                    let table = value
                        .as_table()
                        .ok_or_else(|| wrong("a table: [keys] with \"<leader>d\" = \"daily\""))?;
                    for (keys, command) in table {
                        let command = command.as_str().ok_or_else(|| ConfigError {
                            line: find_key_line(text, &format!("\"{keys}\""))
                                .or_else(|| find_key_line(text, keys)),
                            message: format!(
                                "keys: \"{keys}\" should name a command in quotes, like \"daily\""
                            ),
                        })?;
                        config.keys.push((keys.clone(), command.to_owned()));
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
                "panel_radius" => {
                    config.panel_radius = if value.as_str() == Some("default") {
                        None
                    } else {
                        Some(
                            value
                                .as_integer()
                                .and_then(|n| u32::try_from(n).ok())
                                .filter(|n| PANEL_RADII.contains(n))
                                .ok_or_else(|| {
                                    wrong("a whole number from 0 to 24, or \"default\"")
                                })?,
                        )
                    };
                }
                "panel_shadow" => {
                    config.panel_shadow = value.as_bool().ok_or_else(|| wrong("true or false"))?;
                }
                "autosave" => {
                    config.autosave = value.as_bool().ok_or_else(|| wrong("true or false"))?;
                }
                "wrap" => {
                    config.wrap = value.as_bool().ok_or_else(|| wrong("true or false"))?;
                }
                "wrap_motion" => {
                    config.wrap_motion = value.as_bool().ok_or_else(|| wrong("true or false"))?;
                }
                "large_file_injections_mb" | "large_file_highlight_mb" => {
                    let mb = value
                        .as_integer()
                        .and_then(|n| u32::try_from(n).ok())
                        .filter(|n| (1..=1024).contains(n))
                        .ok_or_else(|| wrong("a whole number of megabytes from 1 to 1024"))?;
                    if key == "large_file_injections_mb" {
                        config.large_file_injections_mb = mb;
                    } else {
                        config.large_file_highlight_mb = mb;
                    }
                }
                "show_code_files" => {
                    config.show_code_files =
                        value.as_bool().ok_or_else(|| wrong("true or false"))?;
                }
                "reuse_neovim" => {
                    config.reuse_neovim = value.as_bool().ok_or_else(|| wrong("true or false"))?;
                }
                "scrolloff" => {
                    config.scrolloff = value
                        .as_integer()
                        .and_then(|n| u32::try_from(n).ok())
                        .filter(|n| *n <= 99)
                        .ok_or_else(|| wrong("a whole number from 0 to 99"))?;
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

    /// Every setting as config.toml writes it: what it does and the values
    /// it takes (in `help`), and its value in these settings.
    fn settings(&self) -> Vec<Setting> {
        let d = Config::default();
        let on_off = |b: bool| if b { "true" } else { "false" };
        let quoted = |s: &str| format!("\"{s}\"");
        fn theme(t: &ThemeChoice) -> &str {
            match t {
                ThemeChoice::System => "system",
                ThemeChoice::Dark => "dark",
                ThemeChoice::Light => "light",
                ThemeChoice::Named(name) => name.as_str(),
            }
        }
        let view = |v: ViewChoice| match v {
            ViewChoice::Styled => "styled",
            ViewChoice::Source => "source",
            ViewChoice::Plain => "plain",
        };
        let clipboard = |system: bool| if system { "system" } else { "vim" };
        let radius = |r: Option<u32>| r.map_or_else(|| quoted("default"), |r| r.to_string());
        let mappings = |m: &Option<Vec<String>>| {
            let lines = m
                .clone()
                .unwrap_or_else(|| vec!["inoremap jk <Esc>".to_owned()]);
            let items: Vec<String> = lines.iter().map(|m| format!("{m:?}")).collect();
            format!("[{}]", items.join(", "))
        };
        let vault = self
            .vault
            .as_ref()
            .map_or_else(|| "~/notes".to_owned(), |v| collapse_home(v));
        vec![
            Setting::new(
                "vault",
                "Your notes folder, used when mNote starts without one.\n\
                 Values: a folder path in quotes (~ is your home folder).",
                quoted(&vault),
            ),
            Setting::new(
                "summon",
                &format!(
                    "The global key that shows and hides mNote, from any app.\n\
                     Values: option, control, command and shift, joined with +, and a\n\
                     letter, digit, punctuation key or space, like \"control+shift+space\".\n\
                     Default: {}",
                    quoted(&key_words(&d.summon))
                ),
                quoted(&key_words(&self.summon)),
            ),
            Setting::new(
                "capture",
                &format!(
                    "The global key for quick capture into today's note.\n\
                     Values: as for summon. Default: {}",
                    quoted(&key_words(&d.capture))
                ),
                quoted(&key_words(&self.capture)),
            ),
            Setting::new(
                "theme",
                &format!(
                    "Colours. Values: \"system\" (follows macOS), \"dark\", \"light\",\n\
                     or a theme's name: ~/.config/mnote/themes/<name>.toml, with\n\
                     base = \"dark\" or \"light\" and a [colors] table of tokens, like\n\
                     paper = \"#1e1e2e\". Saving the file applies it. Default: {}",
                    quoted(theme(&d.theme))
                ),
                quoted(theme(&self.theme)),
            ),
            Setting::new(
                "view",
                &format!(
                    "How notes open (space m cycles). Values: \"styled\" (Markdown\n\
                     formatted, markers hidden), \"source\" (every character, coloured),\n\
                     \"plain\" (every character, no colour). Default: {}",
                    quoted(view(d.view))
                ),
                quoted(view(self.view)),
            ),
            Setting::new(
                "relative_numbers",
                &format!(
                    "Line numbers count from the cursor line (:set rnu).\n\
                     Values: true, false. Default: {}",
                    on_off(d.relative_numbers)
                ),
                on_off(self.relative_numbers).into(),
            ),
            Setting::new(
                "daily_note",
                &format!(
                    "Today's note, for :daily and quick capture.\n\
                     Values: a path in the vault ending in .md; {{date}} is 2026-11-22,\n\
                     and {{year}}, {{month}} and {{day}} work too. Default: {}",
                    quoted(&d.daily_note)
                ),
                quoted(&self.daily_note),
            ),
            Setting::new(
                "git_blame",
                &format!(
                    "Shows who last changed the cursor line, and when, if the vault\n\
                     is in git. Values: true, false. Default: {}",
                    on_off(d.git_blame)
                ),
                on_off(self.git_blame).into(),
            ),
            Setting::new(
                "font",
                &format!(
                    "The note's font, also for code, inputs and the status line.\n\
                     Values: any installed font's name. Default: {}",
                    quoted(&d.font)
                ),
                quoted(&self.font),
            ),
            Setting::new(
                "ui_font",
                &format!(
                    "The font for headings and the interface.\n\
                     Values: any installed font's name. Default: {}",
                    quoted(&d.ui_font)
                ),
                quoted(&self.ui_font),
            ),
            Setting::new(
                "font_size",
                &format!(
                    "Note text size in pixels; headings and code follow it.\n\
                     Values: 10 to 32. Default: {}",
                    d.font_size
                ),
                self.font_size.to_string(),
            ),
            Setting::new(
                "ui_font_size",
                &format!(
                    "Interface text size in pixels: menus, pickers, bars, hints.\n\
                     Values: 11 to 24. Default: {}",
                    d.ui_font_size
                ),
                self.ui_font_size.to_string(),
            ),
            Setting::new(
                "leader",
                &format!(
                    "The key before mNote's own commands (leader f finds notes).\n\
                     Values: \"space\" or one key, like \",\". Default: {}",
                    quoted(&leader_name(d.leader))
                ),
                quoted(&leader_name(self.leader)),
            ),
            Setting::new(
                "clipboard",
                &format!(
                    "Where yanks and pastes go.\n\
                     Values: \"system\" (y, d and p use the system clipboard, as do\n\
                     ⌘C ⌘X ⌘V), \"vim\" (they keep to Vim's registers; \"+ reaches the\n\
                     clipboard). Default: {}",
                    quoted(clipboard(d.system_clipboard))
                ),
                quoted(clipboard(self.system_clipboard)),
            ),
            Setting::new(
                "wrap",
                &format!(
                    "Long lines wrap onto more rows (:set wrap / :set nowrap); with\n\
                     false each line is one row and the note scrolls sideways.\n\
                     Values: true, false. Default: {}",
                    on_off(d.wrap)
                ),
                on_off(self.wrap).into(),
            ),
            Setting::new(
                "wrap_motion",
                &format!(
                    "j and k move by screen row through wrapped lines, as gj and gk.\n\
                     Values: true, false. Default: {}",
                    on_off(d.wrap_motion)
                ),
                on_off(self.wrap_motion).into(),
            ),
            Setting::new(
                "reuse_neovim",
                &format!(
                    "Use Neovim's tree-sitter queries (and later its parsers and language\n\
                     servers) when it's installed, so languages look the same as there.\n\
                     Language packs in ~/.config/mnote/languages/ override them.\n\
                     Values: true, false. Default: {}",
                    on_off(d.reuse_neovim)
                ),
                on_off(self.reuse_neovim).into(),
            ),
            Setting::new(
                "show_code_files",
                &format!(
                    "List the vault's code files (.rs, .py, .lua…) in the finder and\n\
                     explorer beside notes. They open as code; notes stay the only\n\
                     files with links and backlinks.\n\
                     Values: true, false. Default: {}",
                    on_off(d.show_code_files)
                ),
                on_off(self.show_code_files).into(),
            ),
            Setting::new(
                "show_tabs",
                &format!(
                    "When panes show their tab rows. Values: \"auto\" (when a pane has two\n\
                     or more tabs, or there are two or more panes), \"always\", \"never\"\n\
                     (:tabs lists them). Default: {}",
                    quoted(d.show_tabs.name())
                ),
                quoted(self.show_tabs.name()),
            ),
            Setting::new(
                "auto_pairs",
                &format!(
                    "In code files and code blocks, typing an opening bracket or quote\n\
                     adds its closer, typing the closer steps over it, and Backspace\n\
                     between them deletes both. Values: true, false. Default: {}",
                    on_off(d.auto_pairs)
                ),
                on_off(self.auto_pairs).into(),
            ),
            Setting::new(
                "restore_session",
                &format!(
                    "Reopen the vault's panes and tabs as they were left, with their\n\
                     cursors and unsaved untitled notes. Kept in mNote's config folder.\n\
                     Values: true, false. Default: {}",
                    on_off(d.restore_session)
                ),
                on_off(self.restore_session).into(),
            ),
            Setting::new(
                "preview_tabs",
                &format!(
                    "Notes you only look at (the explorer, search results, backlinks,\n\
                     links) open in one italic preview tab that the next replaces;\n\
                     editing or space k keeps it. false: every note gets a tab.\n\
                     Values: true, false. Default: {}",
                    on_off(d.preview_tabs)
                ),
                on_off(self.preview_tabs).into(),
            ),
            Setting::new(
                "window_buttons",
                &format!(
                    "Show the window's close, minimize and zoom buttons. Without them the\n\
                     tab rows reach the top edge; ⌘Q, ⌘M and ⌃⌘F do what they did.\n\
                     Values: true, false. Default: {}",
                    on_off(d.window_buttons)
                ),
                on_off(self.window_buttons).into(),
            ),
            Setting::new(
                "large_file_injections_mb",
                &format!(
                    "Notes and files larger than this many megabytes don't parse their\n\
                     code fences and other embedded languages, so they stay quick to edit.\n\
                     Values: a whole number from 1 to 1024. Default: {}",
                    d.large_file_injections_mb
                ),
                self.large_file_injections_mb.to_string(),
            ),
            Setting::new(
                "large_file_highlight_mb",
                &format!(
                    "Files larger than this many megabytes aren't coloured at all.\n\
                     Values: a whole number from 1 to 1024. Default: {}",
                    d.large_file_highlight_mb
                ),
                self.large_file_highlight_mb.to_string(),
            ),
            Setting::new(
                "scrolloff",
                &format!(
                    "Lines kept on screen above and below the cursor as it moves, as\n\
                     Vim's scrolloff (:set so=N changes it until the next start).\n\
                     Values: a whole number from 0 to 99. Default: {}",
                    d.scrolloff
                ),
                self.scrolloff.to_string(),
            ),
            Setting::new(
                "autosave",
                &format!(
                    "Notes save a moment after you stop typing. With false they save\n\
                     on :w and ⌘S, and still when you leave a note, the window or mNote.\n\
                     Values: true, false. Default: {}",
                    on_off(d.autosave)
                ),
                on_off(self.autosave).into(),
            ),
            Setting::new(
                "panel_radius",
                &format!(
                    "Corner radius of pickers, modals, the capture panel, the command\n\
                     line and toasts. Values: 0 to 24 pixels (0 is square), or \"default\"\n\
                     (10 for pickers and modals, 6 for the rest). Default: {}",
                    radius(d.panel_radius)
                ),
                radius(self.panel_radius),
            ),
            Setting::new(
                "panel_shadow",
                &format!(
                    "The shadow under pickers, modals and toasts.\n\
                     Values: true, false. Default: {}",
                    on_off(d.panel_shadow)
                ),
                on_off(self.panel_shadow).into(),
            ),
            Setting::new(
                "mappings",
                &format!(
                    "Vim mappings, as in a vimrc. Values: a list of lines using map,\n\
                     noremap, nmap, nnoremap, vmap, vnoremap, imap, inoremap or their\n\
                     unmap commands, like \"nnoremap j gj\". They replace the built-in\n\
                     jk to leave INSERT, so keep that line to keep it.\n\
                     Default: {}",
                    mappings(&d.mappings)
                ),
                mappings(&self.mappings),
            ),
        ]
    }

    /// A commented config file holding these settings, for first run and
    /// for `space ,` when there's no file yet: every setting, what it does,
    /// the values it takes and its default.
    pub fn template(&self) -> String {
        let mut out = String::from(
            "# mNote settings. Saving this note applies them.\n\
             # Each setting says what it does, the values it takes and its default.\n",
        );
        for setting in self.settings() {
            out.push('\n');
            out.push_str(&setting.block());
        }
        out.push('\n');
        out.push_str(&self.keys_block());
        out.push_str(&self.plugins_block());
        out
    }

    /// The `[keys]` table, with how it works. It's a TOML table, so it
    /// comes after every other setting.
    /// A plugin setting's value: config.toml's, else its default.
    pub fn plugin_value(&self, plugin: &str, key: &str) -> Option<toml::Value> {
        self.plugins
            .get(plugin)
            .and_then(|t| t.get(key).cloned())
            .or_else(|| {
                registered(plugin)
                    .into_iter()
                    .find(|s| s.key == key)
                    .map(|s| s.default)
            })
    }

    /// A `[plugins.<name>]` table for each plugin with settings, registered
    /// or written: each registered setting with its comment and value (or
    /// default), then anything else the table holds, as it was.
    fn plugins_block(&self) -> String {
        let mut names: Vec<String> = plugin_settings()
            .read()
            .map(|s| s.iter().map(|s| s.plugin.clone()).collect())
            .unwrap_or_default();
        names.extend(self.plugins.keys().cloned());
        names.sort();
        names.dedup();
        let mut out = String::new();
        for name in names {
            let table = self.plugins.get(&name);
            let settings = registered(&name);
            out.push_str(&format!("\n[plugins.{name}]\n"));
            for setting in &settings {
                for line in setting.help.lines() {
                    out.push_str(&format!("# {}\n", line.trim_start()));
                }
                out.push_str(&format!("# Default: {}\n", toml_value(&setting.default)));
                let value = table
                    .and_then(|t| t.get(&setting.key))
                    .unwrap_or(&setting.default);
                out.push_str(&format!("{} = {}\n", setting.key, toml_value(value)));
            }
            for (key, value) in table.into_iter().flatten() {
                if !settings.iter().any(|s| s.key == *key) {
                    out.push_str(&format!("{key} = {}\n", toml_value(value)));
                }
            }
        }
        out
    }

    fn keys_block(&self) -> String {
        let mut out = String::from(KEYS_HELP);
        out.push_str("[keys]\n");
        if self.keys.is_empty() {
            out.push_str("# \"<leader>d\" = \"daily\"\n");
        }
        for (keys, command) in &self.keys {
            out.push_str(&format!("{keys:?} = {command:?}\n"));
        }
        out
    }

    /// The settings missing from config text, at their defaults with their
    /// comments, to add to the end of an older file. `None` when nothing is
    /// missing, or the text doesn't parse (it's left as it is). The vault
    /// is never added: without one, mNote asks for it.
    ///
    /// Returns where to insert them (a byte offset: before the first
    /// `[table]`, since plain settings can't follow one, else the end) and
    /// the text. A file without `[keys]` gets that too, at the end.
    pub fn missing_settings(text: &str) -> Option<(usize, String)> {
        let table: toml::Table = text.parse().ok()?;
        let missing: Vec<String> = Config::default()
            .settings()
            .into_iter()
            .filter(|s| s.key != "vault" && !table.contains_key(s.key))
            .map(|s| s.block())
            .collect();
        let keys = !table.contains_key("keys");
        if missing.is_empty() && !keys {
            return None;
        }
        let first_table = text
            .match_indices('\n')
            .map(|(i, _)| i + 1)
            .chain(std::iter::once(0))
            .filter(|&at| text[at..].starts_with('['))
            .min();
        let mut added = String::new();
        if !missing.is_empty() {
            added.push_str("\n# Settings this file didn't have yet, added at their defaults.\n\n");
            added.push_str(&missing.join("\n"));
        }
        match first_table {
            Some(at) => {
                added.push('\n');
                Some((at, added))
            }
            None => {
                if keys {
                    added.push('\n');
                    added.push_str(&Config::default().keys_block());
                }
                Some((text.len(), added))
            }
        }
    }
}

/// A TOML value as config.toml writes it (inline: `"text"`, `3`, `[1, 2]`,
/// `{ a = 1 }`).
fn toml_value(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => format!("{s:?}"),
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Float(f) => {
            let text = f.to_string();
            if text.contains(['.', 'e', 'i', 'N']) {
                text
            } else {
                format!("{text}.0")
            }
        }
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Datetime(d) => d.to_string(),
        toml::Value::Array(items) => format!(
            "[{}]",
            items.iter().map(toml_value).collect::<Vec<_>>().join(", ")
        ),
        toml::Value::Table(table) => format!(
            "{{ {} }}",
            table
                .iter()
                .map(|(k, v)| format!("{k} = {}", toml_value(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// How `[keys]` works, above the table.
const KEYS_HELP: &str = "# Key bindings: the keys, then the command they run (:keys lists every\n\
# binding and command). Keys are written as in Vim: <leader> is the leader,\n\
# <C-w>v is ⌃w then v, <D-k> is ⌘K. \"none\" removes a binding.\n";

/// One setting in config.toml: its comment and its line.
struct Setting {
    key: &'static str,
    help: String,
    value: String,
}

impl Setting {
    fn new(key: &'static str, help: &str, value: String) -> Self {
        Setting {
            key,
            help: help.to_owned(),
            value,
        }
    }

    fn block(&self) -> String {
        let mut out = String::new();
        for line in self.help.lines() {
            out.push_str("# ");
            out.push_str(line.trim_start());
            out.push('\n');
        }
        out.push_str(&format!("{} = {}\n", self.key, self.value));
        out
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
    // Named after the file, so two files written in one folder don't share
    // a temporary name.
    let name = path
        .file_name()
        .map_or_else(|| "file".into(), |n| n.to_string_lossy());
    let tmp = dir.join(format!(".{name}.mnote-tmp-{}", std::process::id()));
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
            scrolloff: 5,
            reuse_neovim: false,
            show_code_files: false,
            show_tabs: TabsChoice::Auto,
            preview_tabs: true,
            restore_session: true,
            auto_pairs: true,
            window_buttons: false,
            large_file_injections_mb: 2,
            large_file_highlight_mb: 8,
            wrap: false,
            panel_radius: Some(0),
            panel_shadow: false,
            autosave: false,
            mappings: Some(vec!["nnoremap j gj".into(), "inoremap jk <Esc>".into()]),
            ..Config::default()
        };
        // Plugin settings are process-wide; another test may register some.
        let mut back = Config::parse(&config.template()).expect("reads back");
        back.plugins.clear();
        assert_eq!(back, config);
    }

    #[test]
    fn the_template_lists_every_setting_with_values_and_default() {
        let template = Config::default().template();
        let mut back = Config::parse(&template);
        // Plugin settings are process-wide; another test may register some.
        if let Ok(back) = &mut back {
            back.plugins.clear();
        }
        assert_eq!(
            back,
            Ok(Config {
                vault: Some(expand_home("~/notes")),
                // The built-in mapping, written out.
                mappings: Some(vec!["inoremap jk <Esc>".into()]),
                ..Config::default()
            })
        );
        for setting in Config::default().settings() {
            let line = format!("\n{} = ", setting.key);
            assert!(template.contains(&line), "{} is written", setting.key);
            assert!(
                setting.help.contains("Values"),
                "{} lists its values",
                setting.key
            );
            if setting.key != "vault" {
                assert!(
                    setting.help.contains("Default: "),
                    "{} says its default",
                    setting.key
                );
            }
        }
        assert!(template.contains("panel_radius = \"default\""));
        assert!(template.contains("# Values: true, false. Default: true\nautosave = true"));
    }

    #[test]
    fn missing_settings_are_added_at_their_defaults() {
        let insert = |text: &str| -> String {
            let (at, added) = Config::missing_settings(text).expect("some are missing");
            format!("{}{added}{}", &text[..at], &text[at..])
        };
        let old = "vault = \"~/n\"\ntheme = \"dark\"\n";
        let (at, added) = Config::missing_settings(old).expect("some are missing");
        assert_eq!(at, old.len(), "no table: at the end");
        assert!(added.contains("\nautosave = true\n"));
        assert!(added.contains("\nwrap = true\n"));
        assert!(added.contains("\n[keys]\n"), "the keys table too");
        assert!(!added.contains("\ntheme = "), "theme is already there");
        assert!(!added.contains("\nvault = "), "the vault is never added");
        // The file with them added keeps its own settings.
        let full = insert(old);
        let config = Config::parse(&full).expect("still valid");
        assert_eq!(config.theme, ThemeChoice::Dark);
        assert_eq!(Config::missing_settings(&full), None);
        assert_eq!(
            Config::missing_settings("theme = "),
            None,
            "broken: left alone"
        );
        // Settings go before a table, where they still count as settings.
        let with_keys = "theme = \"dark\"\n\n[keys]\n\"<leader>d\" = \"daily\"\n";
        let full = insert(with_keys);
        let config = Config::parse(&full).expect("still valid");
        assert!(config.autosave);
        assert_eq!(config.keys, [("<leader>d".to_owned(), "daily".to_owned())]);
        assert!(full.ends_with("[keys]\n\"<leader>d\" = \"daily\"\n"));
    }

    #[test]
    fn keys_are_read_and_written() {
        let config = Config::parse("[keys]\n\"<leader>d\" = \"daily\"\n\"<C-w>v\" = \"none\"\n")
            .expect("valid");
        assert_eq!(
            config.keys,
            [
                ("<C-w>v".to_owned(), "none".to_owned()),
                ("<leader>d".to_owned(), "daily".to_owned())
            ],
            "by key (a key appears once, so order doesn't matter)"
        );
        let back = Config::parse(&config.template()).expect("the template reads back");
        assert_eq!(back.keys, config.keys);
        let err = Config::parse("[keys]\n\"<leader>d\" = 3\n").expect_err("not a name");
        assert_eq!(err.line, Some(2));
        let err = Config::parse("keys = 3").expect_err("not a table");
        assert!(err.message.contains("[keys]"));
        assert!(
            Config::default()
                .template()
                .contains("[keys]\n# \"<leader>d\" = \"daily\"\n")
        );
    }

    #[test]
    fn themes_by_name_and_theme_files() {
        assert_eq!(
            Config::parse("theme = \"catppuccin\"").map(|c| c.theme),
            Ok(ThemeChoice::Named("catppuccin".into()))
        );
        assert!(Config::parse("theme = \"../x\"").is_err());
        assert!(Config::parse("theme = \".hidden\"").is_err());
        let config = Config {
            theme: ThemeChoice::Named("mine".into()),
            ..Config::default()
        };
        assert_eq!(
            Config::parse(&config.template()).map(|c| c.theme),
            Ok(ThemeChoice::Named("mine".into()))
        );
        let file =
            ThemeFile::parse("base = \"light\"\n[colors]\nink = \"#102030\"\n").expect("valid");
        assert!(!file.dark);
        assert_eq!(file.colors, [("ink".to_owned(), 0x102030ff)]);
        assert_eq!(parse_color("#10203040"), Some(0x10203040));
        assert_eq!(parse_color("102030"), None);
        let err = ThemeFile::parse("[colors]\n\nink = \"blue\"\n").expect_err("not a colour");
        assert_eq!(err.line, Some(3));
        assert!(ThemeFile::parse("base = \"grey\"").is_err());
        assert!(ThemeFile::parse("colours = 1").is_err());
    }

    #[test]
    fn plugin_tables_are_kept_and_registered_settings_checked_and_written() {
        let text = "[plugins.wordgoal]\ngoal = 500\nnote = \"x\"\n";
        let config = Config::parse(text).expect("an unknown plugin's table is kept");
        assert_eq!(
            config.plugins["wordgoal"]
                .get("goal")
                .and_then(toml::Value::as_integer),
            Some(500)
        );
        register_plugin_setting(PluginSetting {
            plugin: "wordgoal".into(),
            key: "goal".into(),
            help: "Words to write each day.\nValues: 1 to 100000.".into(),
            default: toml::Value::Integer(750),
            check: std::sync::Arc::new(|v| {
                v.as_integer()
                    .filter(|n| (1..=100_000).contains(n))
                    .map(|_| ())
                    .ok_or_else(|| "a whole number from 1 to 100000".to_owned())
            }),
        });
        let err = Config::parse("[plugins.wordgoal]\ngoal = 0\n").expect_err("checked");
        assert_eq!(err.line, Some(2));
        assert!(err.message.contains("1 to 100000"));
        let config = Config::parse(text).expect("valid");
        assert_eq!(
            config.plugin_value("wordgoal", "goal"),
            Some(toml::Value::Integer(500))
        );
        assert_eq!(
            Config::default().plugin_value("wordgoal", "goal"),
            Some(toml::Value::Integer(750)),
            "the default"
        );
        let written = config.template();
        assert!(written.contains(
            "[plugins.wordgoal]\n# Words to write each day.\n# Values: 1 to 100000.\n# Default: 750\ngoal = 500\nnote = \"x\"\n"
        ));
        let back = Config::parse(&written).expect("reads back");
        assert_eq!(back.plugins, config.plugins);
        assert!(Config::parse("plugins = 3").is_err());
        assert!(Config::parse("[plugins]\nx = 3").is_err());
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
        assert_eq!(
            Config::parse("panel_radius = \"default\"").map(|c| c.panel_radius),
            Ok(None)
        );
        let err = Config::parse("panel_radius = 30").expect_err("too round");
        assert!(err.message.contains("0 to 24"));
        let err = Config::parse("autosave = \"no\"").expect_err("not a bool");
        assert!(err.message.contains("true or false"));
        let config = Config::parse("panel_radius = 4\npanel_shadow = false\nautosave = false")
            .expect("valid");
        assert_eq!(
            (config.panel_radius, config.panel_shadow, config.autosave),
            (Some(4), false, false)
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
    fn show_tabs_and_window_buttons_parse() {
        assert_eq!(Config::default().show_tabs, TabsChoice::Auto);
        assert!(!Config::default().window_buttons);
        let config = Config::parse("show_tabs = \"always\"\nwindow_buttons = true").expect("valid");
        assert_eq!(config.show_tabs, TabsChoice::Always);
        assert!(config.window_buttons);
        assert!(config.template().contains("show_tabs = \"always\""));
        assert!(config.template().contains("window_buttons = true"));
        assert!(Config::parse("show_tabs = \"sometimes\"").is_err());
        assert!(Config::default().preview_tabs);
        assert!(Config::default().restore_session);
        assert!(
            !Config::parse("restore_session = false")
                .expect("valid")
                .restore_session
        );
        assert!(
            !Config::parse("preview_tabs = false")
                .expect("valid")
                .preview_tabs
        );
        assert!(!TabsChoice::Auto.shows(1, 1));
        assert!(TabsChoice::Auto.shows(2, 1) && TabsChoice::Auto.shows(1, 2));
        assert!(TabsChoice::Always.shows(1, 1) && !TabsChoice::Never.shows(5, 5));
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
