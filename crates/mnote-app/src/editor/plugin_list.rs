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
//! mNote asks, since a plugin can do anything you can; the answer goes in
//! the permission store (`mnote_core::permissions`), which took over the old
//! `plugins/.trusted` file. A plugin for another API version is skipped, a broken
//! one is reported and the rest still load, and one that keeps failing is
//! turned off for the session (see `plugins`). `:plugins` lists them all with
//! how long each took to load. Your own `init.lua` loads last, unasked.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use gpui::Context;
use mnote_core::config::Config;

use mnote_core::permissions::{Kind, Permissions, Scope};

use super::Editor;
use super::plugins::LuaHost;

/// What a permission prompt does with the answer (`None`: not now).
pub(super) type PermissionAnswer = Rc<dyn Fn(&mut Editor, Option<Scope>, &mut Context<Editor>)>;

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

/// A fingerprint of a plugin's code (plugin.toml and init.lua), so its OK
/// is asked again when they change. The same one `plugins/.trusted` kept,
/// so plugins allowed there stay allowed.
pub fn fingerprint(dir: &Path) -> u64 {
    mnote_core::permissions::fingerprint_files(&[&dir.join("plugin.toml"), &dir.join("init.lua")])
}

/// The permission store, carrying over the old `plugins/.trusted` the first
/// time; in memory only without a config folder.
pub fn load_permissions() -> Permissions {
    match Permissions::default_path() {
        Some(path) => {
            let legacy = plugins_dir().map(|d| d.join(".trusted"));
            Permissions::load(path, legacy.as_deref())
        }
        None => Permissions::in_memory(),
    }
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
                    } else if !self
                        .permissions
                        .allowed(Kind::Plugin, &name, fingerprint(&path))
                    {
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
        let title = if about.is_empty() {
            format!("Run the plugin {name}?")
        } else {
            format!("Run the plugin {name}? {about}")
        };
        let what = match &self.plugins[ix].dir {
            Some(dir) => format!("its code in {}: it can do anything you can", dir.display()),
            None => "it can do anything you can".into(),
        };
        self.ask_permission(
            title,
            what,
            Rc::new(move |e, scope, cx| e.answer_plugin(ix, scope, cx)),
            cx,
        );
    }

    /// The answer about plugin `ix`: run it (remembered for `scope`), or
    /// not now (`None`).
    fn answer_plugin(&mut self, ix: usize, scope: Option<Scope>, cx: &mut Context<Self>) {
        let Some(entry) = self.plugins.get_mut(ix) else {
            return;
        };
        match scope {
            Some(scope) => {
                if let Some(dir) = entry.dir.clone() {
                    let name = entry.name.clone();
                    self.permissions
                        .allow(Kind::Plugin, &name, fingerprint(&dir), scope);
                }
                let config = self.config.clone();
                self.start_plugin(ix, &config, cx);
            }
            None => entry.status = Status::Failed("not allowed to run yet".into()),
        }
        self.ask_next_plugin(cx);
        cx.notify();
    }

    /// Asks whether something may run, in one picker for everything that
    /// needs an OK: "Allow always", "Allow this session", "Not now".
    /// `what` says exactly what would run or be read.
    pub(super) fn ask_permission(
        &mut self,
        title: String,
        what: String,
        answer: PermissionAnswer,
        cx: &mut Context<Self>,
    ) {
        let items = vec![
            crate::picker::custom::Item {
                text: "Allow always".into(),
                detail: Some(what),
            },
            crate::picker::custom::Item {
                text: "Allow this session".into(),
                detail: Some("asked again when mNote starts next".into()),
            },
            crate::picker::custom::Item {
                text: "Not now".into(),
                detail: Some("asked again next time".into()),
            },
        ];
        let picker = crate::picker::custom::CustomPicker::new(
            title,
            items,
            Rc::new(move |choice| {
                let answer = answer.clone();
                let scope = match choice {
                    0 => Some(Scope::Always),
                    1 => Some(Scope::Session),
                    _ => None,
                };
                crate::picker::PickerCallback(Rc::new(move |e, cx| answer(e, scope, cx)))
            }),
        );
        self.open_picker(crate::picker::Picker::new(Box::new(picker)), cx);
    }

    /// `:permissions`: everything allowed to run; picking one takes its OK
    /// back (it's asked for again next time).
    pub(super) fn list_permissions(&mut self, cx: &mut Context<Self>) {
        let grants = self.permissions.grants();
        if grants.is_empty() {
            self.vim
                .show_message("Nothing has been allowed to run yet.".to_owned());
            return;
        }
        let items: Vec<crate::picker::custom::Item> = grants
            .iter()
            .map(|g| crate::picker::custom::Item {
                text: format!("{} {}", g.kind, g.name),
                detail: Some(format!(
                    "{}, code {:08x}",
                    match g.scope {
                        Scope::Always => "always",
                        Scope::Session => "this session",
                        Scope::Once => "once",
                    },
                    g.fingerprint >> 32
                )),
            })
            .collect();
        let picker = crate::picker::custom::CustomPicker::new(
            "Permissions: pick one to take it back".into(),
            items,
            Rc::new(move |index| {
                let grant = grants.get(index).cloned();
                crate::picker::PickerCallback(Rc::new(move |e, cx| {
                    if let Some(g) = &grant
                        && e.permissions.revoke(g.kind, &g.name)
                    {
                        e.show_toast(
                            format!(
                                "{} {} will ask again next time (what's running keeps running)",
                                g.kind, g.name
                            ),
                            false,
                            cx,
                        );
                    }
                }))
            }),
        );
        self.open_picker(crate::picker::Picker::new(Box::new(picker)), cx);
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
    fn the_fingerprint_is_the_one_trusted_before() {
        // plugins/.trusted's fingerprint, as it was computed: FNV-1a over
        // plugin.toml then init.lua, each followed by 0xff.
        fn old(dir: &Path) -> u64 {
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
        let dir = std::env::temp_dir().join(format!("mnote-print-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("plugin.toml"), "name = \"x\"").expect("write");
        std::fs::write(dir.join("init.lua"), "-- x").expect("write");
        assert_eq!(fingerprint(&dir), old(&dir));
        let _ = std::fs::remove_dir_all(&dir);
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
