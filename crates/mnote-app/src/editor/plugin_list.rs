//! Installed plugins: folders in `~/.config/mnote/plugins/<name>/`, each a
//! `plugin.toml` and an `init.lua` (and its own modules under `lua/`).
//!
//! A plugin runs when config.toml turns it on:
//!
//! ```toml
//! [plugins.tasks]
//! enabled = true
//! ```
//!
//! The first time an enabled plugin would run (or after its code changed)
//! mNote asks, since a plugin can do anything you can; the answer is kept in
//! `plugins/.trusted`. A plugin for another API version is skipped, a broken
//! one is reported and the rest still load, and one that keeps failing is
//! turned off for the session (see `plugins`). `:plugins` lists them all with
//! how long each took to load. Your own `init.lua` loads last, unasked.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use gpui::Context;
use mnote_core::config::Config;

use super::Editor;
use super::plugins::LuaHost;

/// The plugin API this mNote offers: plugin.toml's `api`.
pub const PLUGIN_API: i64 = 1;

/// A plugin's `plugin.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub api: i64,
    pub description: String,
}

impl Manifest {
    /// Reads plugin.toml: `name`, `version`, `api` and `description`.
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let table: toml::Table = text
            .parse()
            .map_err(|e: toml::de::Error| format!("plugin.toml: {}", e.message().trim()))?;
        let text_of = |key: &str| {
            table
                .get(key)
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("plugin.toml needs {key} = \"…\""))
        };
        Ok(Manifest {
            name: text_of("name")?,
            version: text_of("version")?,
            api: table
                .get("api")
                .and_then(toml::Value::as_integer)
                .ok_or_else(|| format!("plugin.toml needs api = {PLUGIN_API}"))?,
            description: table
                .get("description")
                .and_then(toml::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
    }
}

/// Where a plugin stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Loaded,
    /// Not turned on in config.toml.
    Off,
    /// Turned on, waiting for the user's OK to run.
    Waiting,
    /// Written for another API version.
    WrongApi(i64),
    /// Couldn't load: why.
    Failed(String),
}

/// A plugin found, and what became of it.
pub struct Entry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub dir: Option<PathBuf>,
    pub status: Status,
    /// How long loading took, in milliseconds.
    pub load_ms: f32,
    pub host: Option<Rc<LuaHost>>,
}

/// `~/.config/mnote/plugins`.
pub fn plugins_dir() -> Option<PathBuf> {
    Some(Config::path()?.parent()?.join("plugins"))
}

/// The plugin folders in `dir`, by name.
pub fn discover(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut found: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (!name.starts_with('.')).then(|| (name, e.path()))
        })
        .collect();
    found.sort();
    found
}

/// A fingerprint of a plugin's code (plugin.toml and init.lua), so trust is
/// asked again when they change. FNV-1a: stable across runs and builds.
pub fn fingerprint(dir: &Path) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for file in ["plugin.toml", "init.lua"] {
        for byte in std::fs::read(dir.join(file)).unwrap_or_default() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0xff;
    }
    hash
}

/// Whether plugin `name` with this fingerprint was allowed to run, in the
/// trust file's lines (`name fingerprint`).
pub fn trusted(trust: &str, name: &str, print: u64) -> bool {
    let want = format!("{name} {print:016x}");
    trust.lines().any(|l| l.trim() == want)
}

/// The trust file with plugin `name` allowed at this fingerprint (its
/// older lines dropped).
pub fn trust(trust: &str, name: &str, print: u64) -> String {
    let mut lines: Vec<String> = trust
        .lines()
        .filter(|l| l.split_whitespace().next() != Some(name))
        .map(str::to_owned)
        .collect();
    lines.push(format!("{name} {print:016x}"));
    lines.join("\n") + "\n"
}

fn trust_file() -> Option<PathBuf> {
    Some(plugins_dir()?.join(".trusted"))
}

impl Editor {
    /// Finds the installed plugins and loads those turned on and allowed,
    /// then `init.lua`; asks about turned-on plugins not yet allowed. Runs
    /// once; later, plugins only see the new config.
    pub(super) fn load_plugins(&mut self, config: &Config, cx: &mut Context<Self>) {
        if self.plugins_tried {
            for host in self.plugins.iter().filter_map(|p| p.host.as_ref()) {
                host.set_plugins(config);
            }
            return;
        }
        self.plugins_tried = true;
        let trust = trust_file()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .unwrap_or_default();
        let dir = plugins_dir();
        for (name, path) in dir.as_deref().map(discover).unwrap_or_default() {
            let manifest = std::fs::read_to_string(path.join("plugin.toml"))
                .map_err(|_| "no plugin.toml".to_owned())
                .and_then(|text| Manifest::parse(&text));
            let enabled = config
                .plugins
                .get(&name)
                .and_then(|t| t.get("enabled"))
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);
            let (version, description, status) = match manifest {
                Err(why) => (String::new(), String::new(), Status::Failed(why)),
                Ok(m) => {
                    let status = if !enabled {
                        Status::Off
                    } else if m.api != PLUGIN_API {
                        Status::WrongApi(m.api)
                    } else if !trusted(&trust, &name, fingerprint(&path)) {
                        Status::Waiting
                    } else {
                        Status::Loaded
                    };
                    (m.version, m.description, status)
                }
            };
            self.plugins.push(Entry {
                name,
                version,
                description,
                dir: Some(path),
                status,
                load_ms: 0.,
                host: None,
            });
        }
        // Your own init.lua: last, so it can change what plugins set up.
        if super::plugins::init_path().is_some_and(|p| p.is_file()) {
            self.plugins.push(Entry {
                name: "init.lua".into(),
                version: String::new(),
                description: "your own".into(),
                dir: None,
                status: Status::Loaded,
                load_ms: 0.,
                host: None,
            });
        }
        for ix in 0..self.plugins.len() {
            if self.plugins[ix].status == Status::Loaded {
                self.start_plugin(ix, config, cx);
            }
        }
        for entry in &self.plugins {
            if let Status::WrongApi(api) = entry.status {
                self.notice = Some(
                    format!(
                        "Plugin {} is for plugin API {api}; this mNote has {PLUGIN_API}",
                        entry.name
                    )
                    .into(),
                );
            }
        }
        self.ask_next_plugin(cx);
    }

    /// Loads plugin `ix` (timed): its commands, keys and handlers join the
    /// editor. A failure is shown and the rest carry on.
    fn start_plugin(&mut self, ix: usize, config: &Config, cx: &mut Context<Self>) {
        let started = Instant::now();
        let loaded = match &self.plugins[ix].dir {
            Some(dir) => LuaHost::load_plugin(&self.plugins[ix].name, dir, config),
            None => match super::plugins::init_path() {
                Some(path) => LuaHost::load(&path, config),
                None => Err("no config folder".into()),
            },
        };
        self.plugins[ix].load_ms = started.elapsed().as_secs_f32() * 1000.;
        match loaded {
            Ok(host) => {
                let host = Rc::new(host);
                self.plugins[ix].status = Status::Loaded;
                self.plugins[ix].host = Some(host.clone());
                self.install_plugin(&host);
                self.start_requests(&host, cx);
                self.show_plugin_notices(&host, cx);
            }
            Err(why) => {
                self.plugins[ix].status = Status::Failed(why.clone());
                self.show_toast(why, false, cx);
            }
        }
        if std::env::var_os("MNOTE_LATENCY").is_some() {
            eprintln!(
                "mnote plugin {}: {:.2} ms ({})",
                self.plugins[ix].name,
                self.plugins[ix].load_ms,
                if self.plugins[ix].host.is_some() {
                    "loaded"
                } else {
                    "failed"
                }
            );
        }
        // New keys and commands: rebuild the keymap with the config's.
        let (keymap, _) = crate::keymap::Keymap::build(&crate::commands::all(), &config.keys);
        crate::keymap::set(keymap);
    }

    /// Asks about the next turned-on plugin that hasn't been allowed yet.
    fn ask_next_plugin(&mut self, cx: &mut Context<Self>) {
        if self.picker.is_some() {
            return;
        }
        let Some(ix) = self
            .plugins
            .iter()
            .position(|p| p.status == Status::Waiting)
        else {
            return;
        };
        let name = self.plugins[ix].name.clone();
        let about = self.plugins[ix].description.clone();
        let items = vec![
            crate::picker::custom::Item {
                text: "Run it".into(),
                detail: Some("it can do anything you can".into()),
            },
            crate::picker::custom::Item {
                text: "Not now".into(),
                detail: Some("ask again next time".into()),
            },
        ];
        let title = if about.is_empty() {
            format!("Run the plugin {name}?")
        } else {
            format!("Run the plugin {name}? {about}")
        };
        let picker = crate::picker::custom::CustomPicker::new(
            title,
            items,
            Rc::new(move |choice| {
                crate::picker::PickerCallback(Rc::new(move |e, cx| {
                    e.answer_plugin(ix, choice == 0, cx)
                }))
            }),
        );
        self.open_picker(crate::picker::Picker::new(Box::new(picker)), cx);
    }

    /// The answer about plugin `ix`: run it (and remember), or not now.
    fn answer_plugin(&mut self, ix: usize, run: bool, cx: &mut Context<Self>) {
        let Some(entry) = self.plugins.get_mut(ix) else {
            return;
        };
        if run {
            if let (Some(dir), Some(file)) = (entry.dir.clone(), trust_file()) {
                let old = std::fs::read_to_string(&file).unwrap_or_default();
                let new = trust(&old, &entry.name, fingerprint(&dir));
                let _ = mnote_core::config::write_file(&file, &new);
            }
            let config = self.config.clone();
            self.start_plugin(ix, &config, cx);
        } else {
            entry.status = Status::Failed("not allowed to run yet".into());
        }
        self.ask_next_plugin(cx);
        cx.notify();
    }

    /// `:plugins`: each plugin, its version, how it stands and its load time.
    pub(super) fn list_plugins(&mut self) {
        if self.plugins.is_empty() {
            let dir = plugins_dir().map_or_else(String::new, |d| d.display().to_string());
            self.vim.show_message(format!(
                "No plugins. Put them in {dir}, then turn one on in settings."
            ));
            return;
        }
        let mut lines =
            vec!["Plugin          Version   Status                     Load".to_owned()];
        for p in &self.plugins {
            let status = match &p.status {
                Status::Loaded if p.host.as_ref().is_some_and(|h| h.is_off()) => {
                    "turned off (errors)".to_owned()
                }
                Status::Loaded => "loaded".to_owned(),
                Status::Off => "off: enabled = true turns it on".to_owned(),
                Status::Waiting => "waiting for your OK".to_owned(),
                Status::WrongApi(api) => format!("needs plugin API {api}"),
                Status::Failed(why) => why.clone(),
            };
            let load = if p.load_ms > 0. {
                format!("{:.1} ms", p.load_ms)
            } else {
                String::new()
            };
            lines.push(format!(
                "{:<15} {:<9} {:<26} {load}",
                p.name, p.version, status
            ));
        }
        self.vim.show_message(lines.join("\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifests_need_a_name_version_and_api() {
        let m = Manifest::parse(
            "name = \"tasks\"\nversion = \"1.0\"\napi = 1\ndescription = \"Tasks\"",
        )
        .expect("valid");
        assert_eq!((m.name.as_str(), m.api), ("tasks", 1));
        assert!(
            Manifest::parse("name = \"x\"\napi = 1").is_err(),
            "no version"
        );
        assert!(
            Manifest::parse("name = \"x\"\nversion = \"1\"").is_err(),
            "no api"
        );
        assert!(Manifest::parse("not toml =").is_err());
    }

    #[test]
    fn trust_is_per_plugin_and_fingerprint() {
        let file = trust("", "tasks", 7);
        assert!(trusted(&file, "tasks", 7));
        assert!(!trusted(&file, "tasks", 8), "its code changed");
        assert!(!trusted(&file, "other", 7));
        let file = trust(&file, "tasks", 8);
        assert!(
            trusted(&file, "tasks", 8) && !trusted(&file, "tasks", 7),
            "replaced"
        );
        let file = trust(&file, "wordgoal", 1);
        assert!(trusted(&file, "tasks", 8) && trusted(&file, "wordgoal", 1));
    }

    #[test]
    fn plugin_folders_are_found_and_fingerprinted() {
        let dir = std::env::temp_dir().join(format!("mnote-plugins-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for name in ["b", "a", ".hidden"] {
            std::fs::create_dir_all(dir.join(name)).expect("dir");
        }
        std::fs::write(dir.join("a/init.lua"), "-- a").expect("write");
        let found: Vec<String> = discover(&dir).into_iter().map(|(n, _)| n).collect();
        assert_eq!(found, ["a", "b"]);
        let before = fingerprint(&dir.join("a"));
        assert_eq!(before, fingerprint(&dir.join("a")), "stable");
        std::fs::write(dir.join("a/init.lua"), "-- changed").expect("write");
        assert_ne!(before, fingerprint(&dir.join("a")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
