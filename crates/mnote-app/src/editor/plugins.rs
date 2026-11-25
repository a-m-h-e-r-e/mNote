//! The plugin host: Lua 5.4 (through `mlua`), loaded from
//! `~/.config/mnote/init.lua`, with an `mnote` API over the editor's
//! registries, in Neovim's style:
//!
//! ```lua
//! mnote.command.create("Today", function(call)
//!   mnote.buf.insert(os.date("%Y-%m-%d"))
//! end, { desc = "Insert today's date" })
//! mnote.keymap.set("n", "<leader>t", "Today")
//! ```
//!
//! - `mnote.command.create(name, fn, { desc })`: an ex command (`:Today`)
//!   and a palette entry. `fn` gets `{ args = "…", bang = false }`.
//! - `mnote.keymap.set(mode, keys, fn | "command" | ":ex line", { desc })`:
//!   `"n"` binds in NORMAL through the keymap; `"i"` and `"v"` through
//!   Vim mappings.
//! - `mnote.buf`: `get_lines(first, last)` (0-based, `last` exclusive,
//!   negative from the end), `set_lines(first, last, lines)`, `get_text()`,
//!   `line_count()`, `cursor()` (line, column), `set_cursor(line, column)`
//!   and `insert(text)`. Edits go through Vim: a whole command is one undo
//!   step, and `.` repeats a command that changed the text.
//! - `mnote.note()`, `mnote.mode()`: the open note's path and Vim's mode.
//! - `mnote.notify(text)`: a toast. `mnote.config(name)`: the
//!   `[plugins.<name>]` table from config.toml.
//!
//! Every call into Lua is protected: an error becomes a toast naming where
//! it came from, and mNote carries on. Commands and keys run on the
//! keystroke, so each is timed and one over 2 ms says so. Without an
//! `init.lua` nothing is loaded and nothing runs.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder;
use gpui::{Context, IntoElement, Styled};
use mlua::{Function, Lua, RegistryKey, Table, Value};
use mnote_core::NotePath;
use mnote_core::config::Config;
use mnote_vim::ex::ArgKind;
use mnote_vim::text::{clamp_normal, line_count, line_of, line_start};

use super::Editor;
use super::events::{EditorEvent, EventKind};
use crate::commands::{Command, Invocation, Run};

/// A command or key slower than this says so.
const SLOW: Duration = Duration::from_millis(2);

/// What a key binding runs.
enum Rhs {
    Callback(usize),
    Command(String),
    Ex(String),
}

/// Something a script asked the editor to start (it has no editor or
/// window while it runs).
enum Request {
    Picker {
        title: String,
        items: Vec<crate::picker::custom::Item>,
        on_select: usize,
    },
    Job {
        cmd: Vec<String>,
        on_stdout: Option<usize>,
        on_exit: Option<usize>,
    },
    /// `mnote.open`: a note, and the line to put the cursor on.
    Open { note: NotePath, line: Option<usize> },
    /// `mnote.feedkeys`: keys to run as typed.
    Keys(Vec<mnote_vim::Key>),
    /// `mnote.cmd`: an Ex line.
    Ex(String),
    /// `mnote.apply_edits`: line edits to several notes, as one undo step.
    ApplyEdits {
        edits: Vec<(NotePath, Vec<LineEdit>)>,
        label: String,
    },
}

/// Lines `first..last` (as `mnote.buf.set_lines` counts them) become `lines`.
struct LineEdit {
    first: i64,
    last: i64,
    lines: Vec<String>,
}

/// A row a block renderer returned: text, in a theme colour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRow {
    pub text: String,
    pub color: Option<String>,
}

/// Rendered blocks by language and text; `None` while one renders.
type BlockCache = std::collections::HashMap<(String, String), Option<Rc<Vec<BlockRow>>>>;

/// What `init.lua` asked for while it ran, for the editor to install.
#[derive(Default)]
struct HostState {
    callbacks: Vec<RegistryKey>,
    commands: Vec<(String, String, usize)>,
    bindings: Vec<(char, String, Rhs, Option<String>)>,
    /// `mnote.on`: the event, a note pattern, the callback.
    subscriptions: Vec<(EventKind, Option<String>, usize)>,
    /// Pickers and jobs to start.
    requests: Vec<Request>,
    /// `mnote.blocks.register`: a fence language and its render callback.
    blocks: Vec<(String, usize)>,
    /// `mnote.notify` texts waiting to be shown.
    notices: Vec<String>,
    /// config.toml's `[plugins.*]` tables, for `mnote.config`.
    plugins: std::collections::BTreeMap<String, toml::Table>,
}

/// A running Lua state and what its script registered.
pub struct LuaHost {
    lua: Lua,
    state: Rc<RefCell<HostState>>,
    /// Rendered blocks by language and text; `None` while one renders.
    block_rows: RefCell<BlockCache>,
    /// Where the code came from, for messages (`init.lua`, a plugin's name).
    source: String,
    /// Errors so far; at [`MAX_ERRORS`] the plugin is turned off.
    errors: std::cell::Cell<u32>,
}

/// A plugin that fails this many times is turned off for the session.
const MAX_ERRORS: u32 = 3;

/// The functions of `mnote.buf` and friends that need the editor: outside a
/// command or key they explain that instead.
const SCOPED: &[(&str, &str)] = &[
    ("buf", "get_lines"),
    ("buf", "set_lines"),
    ("buf", "get_text"),
    ("buf", "line_count"),
    ("buf", "cursor"),
    ("buf", "set_cursor"),
    ("buf", "insert"),
    ("", "note"),
    ("", "mode"),
    ("", "decorate"),
    ("", "undecorate"),
    ("clipboard", "get"),
    ("clipboard", "set"),
];

impl LuaHost {
    /// The origin its edits are recorded with: `plugin:<name>`, or
    /// `plugin:init.lua` for the user's own script.
    fn origin(&self) -> mnote_core::text::Origin {
        mnote_core::text::Origin::Plugin(self.source.as_str().into())
    }

    /// Runs `path` (`init.lua`) with the `mnote` API.
    pub fn load(path: &Path, config: &Config) -> Result<LuaHost, String> {
        let source = path
            .file_name()
            .map_or_else(|| "init.lua".into(), |n| n.to_string_lossy().into_owned());
        Self::run(path, None, source, config)
    }

    /// Runs plugin `name`'s `init.lua` from its folder; `require` finds the
    /// plugin's own modules in its `lua/` folder.
    pub fn load_plugin(name: &str, dir: &Path, config: &Config) -> Result<LuaHost, String> {
        Self::run(&dir.join("init.lua"), Some(dir), name.to_owned(), config)
    }

    fn run(
        path: &Path,
        dir: Option<&Path>,
        source: String,
        config: &Config,
    ) -> Result<LuaHost, String> {
        let code = std::fs::read_to_string(path)
            .map_err(|err| format!("{source}: couldn't read {}: {err}", path.display()))?;
        let host = LuaHost {
            lua: Lua::new(),
            state: Rc::default(),
            block_rows: RefCell::default(),
            source,
            errors: std::cell::Cell::new(0),
        };
        host.set_plugins(config);
        host.install_api().map_err(|err| host.message(&err))?;
        if let Some(dir) = dir {
            let lua_dir = dir.join("lua");
            let package: Table = host
                .lua
                .globals()
                .get("package")
                .map_err(|e| host.message(&e))?;
            let path: String = package.get("path").map_err(|e| host.message(&e))?;
            let own = format!("{0}/?.lua;{0}/?/init.lua;{path}", lua_dir.to_string_lossy());
            package.set("path", own).map_err(|e| host.message(&e))?;
        }
        host.lua
            .load(&code)
            .set_name(format!("@{}", host.source))
            .exec()
            .map_err(|err| host.message(&err))?;
        Ok(host)
    }

    /// Turned off after repeated errors.
    pub fn is_off(&self) -> bool {
        self.errors.get() >= MAX_ERRORS
    }

    /// Counts an error; returns whether that turned the plugin off.
    fn failed(&self) -> bool {
        self.errors.set(self.errors.get() + 1);
        self.errors.get() == MAX_ERRORS
    }

    /// Keeps config.toml's `[plugins.*]` tables for `mnote.config`.
    pub fn set_plugins(&self, config: &Config) {
        self.state.borrow_mut().plugins = config.plugins.clone();
    }

    /// An error as a toast says it: where, and its first line.
    fn message(&self, err: &mlua::Error) -> String {
        let text = err.to_string();
        let first = text.lines().next().unwrap_or("").trim();
        format!("{}: {first}", self.source)
    }

    fn install_api(&self) -> mlua::Result<()> {
        let lua = &self.lua;
        let mnote = lua.create_table()?;

        let command = lua.create_table()?;
        let state = self.state.clone();
        command.set(
            "create",
            lua.create_function(
                move |lua, (name, f, opts): (String, Function, Option<Table>)| {
                    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphabetic()) {
                        return Err(mlua::Error::runtime(format!(
                            "command names are letters only, like \"Today\" (got {name:?})"
                        )));
                    }
                    let desc = opts
                        .and_then(|o| o.get::<Option<String>>("desc").ok().flatten())
                        .unwrap_or_else(|| name.clone());
                    let mut s = state.borrow_mut();
                    s.callbacks.push(lua.create_registry_value(f)?);
                    let ix = s.callbacks.len() - 1;
                    s.commands.push((name, desc, ix));
                    Ok(())
                },
            )?,
        )?;
        mnote.set("command", command)?;

        let keymap = lua.create_table()?;
        let state = self.state.clone();
        keymap.set(
            "set",
            lua.create_function(
                move |lua, (mode, keys, rhs, opts): (String, String, Value, Option<Table>)| {
                    let mode = match mode.as_str() {
                        "n" | "i" | "v" => mode.chars().next().unwrap_or('n'),
                        other => {
                            return Err(mlua::Error::runtime(format!(
                                "keymap modes are \"n\", \"i\" or \"v\" (got {other:?})"
                            )));
                        }
                    };
                    if mode == 'n'
                        && let Err(why) = crate::keymap::parse(&keys)
                    {
                        return Err(mlua::Error::runtime(format!("{keys}: {why}")));
                    }
                    let desc = opts.and_then(|o| o.get::<Option<String>>("desc").ok().flatten());
                    let mut s = state.borrow_mut();
                    let rhs = match rhs {
                        Value::Function(f) => {
                            s.callbacks.push(lua.create_registry_value(f)?);
                            Rhs::Callback(s.callbacks.len() - 1)
                        }
                        Value::String(text) => {
                            let text = text.to_str()?.to_string();
                            match text.strip_prefix(':') {
                                Some(line) => Rhs::Ex(line.to_owned()),
                                None => Rhs::Command(text),
                            }
                        }
                        _ => {
                            return Err(mlua::Error::runtime(
                                "a key runs a function, a command's name or an \":ex line\"",
                            ));
                        }
                    };
                    s.bindings.push((mode, keys, rhs, desc));
                    Ok(())
                },
            )?,
        )?;
        mnote.set("keymap", keymap)?;

        let state = self.state.clone();
        mnote.set(
            "notify",
            lua.create_function(move |_, text: String| {
                state.borrow_mut().notices.push(text);
                Ok(())
            })?,
        )?;

        let state = self.state.clone();
        mnote.set(
            "config",
            lua.create_function(move |lua, name: String| {
                let table = lua.create_table()?;
                if let Some(plugin) = state.borrow().plugins.get(&name) {
                    for (key, value) in plugin {
                        table.set(key.as_str(), toml_to_lua(lua, value)?)?;
                    }
                }
                Ok(table)
            })?,
        )?;

        let state = self.state.clone();
        mnote.set(
            "on",
            lua.create_function(
                move |lua, (name, a, b): (String, Value, Option<Function>)| {
                    let kind = event_kind(&name).ok_or_else(|| {
                        mlua::Error::runtime(format!("there's no event called {name}"))
                    })?;
                    let (pattern, f) = match (a, b) {
                        (Value::Function(f), None) => (None, f),
                        (Value::Table(opts), Some(f)) => {
                            (opts.get::<Option<String>>("pattern")?, f)
                        }
                        _ => {
                            return Err(mlua::Error::runtime(
                                "mnote.on(event, fn) or mnote.on(event, { pattern = \"…\" }, fn)",
                            ));
                        }
                    };
                    let mut s = state.borrow_mut();
                    s.callbacks.push(lua.create_registry_value(f)?);
                    let ix = s.callbacks.len() - 1;
                    s.subscriptions.push((kind, pattern, ix));
                    Ok(())
                },
            )?,
        )?;

        let picker = lua.create_table()?;
        let state = self.state.clone();
        picker.set(
            "open",
            lua.create_function(move |lua, spec: Table| {
                let title = spec
                    .get::<Option<String>>("title")?
                    .unwrap_or_else(|| "Pick".into());
                let on_select: Function = spec.get("on_select").map_err(|_| {
                    mlua::Error::runtime("a picker needs on_select = function(item, index)")
                })?;
                let mut items = Vec::new();
                for item in spec.get::<Table>("items")?.sequence_values::<Value>() {
                    items.push(match item? {
                        Value::Table(t) => crate::picker::custom::Item {
                            text: t.get("text")?,
                            detail: t.get("detail")?,
                        },
                        other => crate::picker::custom::Item {
                            text: lua
                                .coerce_string(other)?
                                .map_or_else(String::new, |s| s.to_string_lossy()),
                            detail: None,
                        },
                    });
                }
                let mut s = state.borrow_mut();
                s.callbacks.push(lua.create_registry_value(on_select)?);
                let on_select = s.callbacks.len() - 1;
                s.requests.push(Request::Picker {
                    title,
                    items,
                    on_select,
                });
                Ok(())
            })?,
        )?;
        mnote.set("picker", picker)?;

        let job = lua.create_table()?;
        let state = self.state.clone();
        job.set(
            "start",
            lua.create_function(move |lua, (cmd, opts): (Vec<String>, Option<Table>)| {
                if cmd.is_empty() {
                    return Err(mlua::Error::runtime(
                        "a job needs a command, like {\"git\", \"log\"}",
                    ));
                }
                let mut s = state.borrow_mut();
                let mut callback = |name: &str| -> mlua::Result<Option<usize>> {
                    let f = opts
                        .as_ref()
                        .map(|o| o.get::<Option<Function>>(name))
                        .transpose()?
                        .flatten();
                    Ok(match f {
                        Some(f) => {
                            s.callbacks.push(lua.create_registry_value(f)?);
                            Some(s.callbacks.len() - 1)
                        }
                        None => None,
                    })
                };
                let on_stdout = callback("on_stdout")?;
                let on_exit = callback("on_exit")?;
                s.requests.push(Request::Job {
                    cmd,
                    on_stdout,
                    on_exit,
                });
                Ok(())
            })?,
        )?;
        mnote.set("job", job)?;

        let state = self.state.clone();
        mnote.set(
            "open",
            lua.create_function(move |_, (path, opts): (String, Option<Table>)| {
                let line = opts
                    .map(|o| o.get::<Option<usize>>("line"))
                    .transpose()?
                    .flatten();
                let note = NotePath::new(&path)
                    .or_else(|_| NotePath::new(format!("{path}.md")))
                    .map_err(|_| {
                        mlua::Error::runtime(format!("{path} isn't a note path in the vault"))
                    })?;
                state
                    .borrow_mut()
                    .requests
                    .push(Request::Open { note, line });
                Ok(())
            })?,
        )?;

        let state = self.state.clone();
        mnote.set(
            "feedkeys",
            lua.create_function(move |_, keys: String| {
                let keys = mnote_vim::parse_keys(&keys);
                state.borrow_mut().requests.push(Request::Keys(keys));
                Ok(())
            })?,
        )?;

        let state = self.state.clone();
        mnote.set(
            "cmd",
            lua.create_function(move |_, line: String| {
                let line = line.trim().trim_start_matches(':').to_owned();
                if !line.is_empty() {
                    state.borrow_mut().requests.push(Request::Ex(line));
                }
                Ok(())
            })?,
        )?;

        let state = self.state.clone();
        mnote.set(
            "apply_edits",
            lua.create_function(move |_, (list, opts): (Table, Option<Table>)| {
                let label = opts
                    .map(|o| o.get::<Option<String>>("label"))
                    .transpose()?
                    .flatten()
                    .unwrap_or_else(|| "Edit notes".into());
                let mut edits: Vec<(NotePath, Vec<LineEdit>)> = Vec::new();
                for item in list.sequence_values::<Table>() {
                    let item = item?;
                    let path: String = item.get("note")?;
                    let note = NotePath::new(&path)
                        .or_else(|_| NotePath::new(format!("{path}.md")))
                        .map_err(|_| {
                            mlua::Error::runtime(format!("{path} isn't a note path in the vault"))
                        })?;
                    let edit = LineEdit {
                        first: item.get("first")?,
                        last: item.get("last")?,
                        lines: item
                            .get::<Option<Vec<String>>>("lines")?
                            .unwrap_or_default(),
                    };
                    match edits.iter_mut().find(|(n, _)| *n == note) {
                        Some((_, list)) => list.push(edit),
                        None => edits.push((note, vec![edit])),
                    }
                }
                if !edits.is_empty() {
                    state
                        .borrow_mut()
                        .requests
                        .push(Request::ApplyEdits { edits, label });
                }
                Ok(())
            })?,
        )?;

        // Filled in for each call (they need the window); stubs meanwhile.
        mnote.set("clipboard", lua.create_table()?)?;

        let blocks = lua.create_table()?;
        let state = self.state.clone();
        blocks.set(
            "register",
            lua.create_function(move |lua, (lang, opts): (String, Table)| {
                let render: Function = opts
                    .get("render")
                    .map_err(|_| mlua::Error::runtime("a block needs render = function(lines)"))?;
                let mut s = state.borrow_mut();
                s.callbacks.push(lua.create_registry_value(render)?);
                let ix = s.callbacks.len() - 1;
                s.blocks.push((lang, ix));
                Ok(())
            })?,
        )?;
        mnote.set("blocks", blocks)?;

        mnote.set("buf", lua.create_table()?)?;
        lua.globals().set("mnote", mnote)?;
        self.install_stubs()
    }

    /// The editor functions, outside a command or key: they explain.
    fn install_stubs(&self) -> mlua::Result<()> {
        let mnote: Table = self.lua.globals().get("mnote")?;
        for (table, name) in SCOPED {
            let target: Table = if table.is_empty() {
                mnote.clone()
            } else {
                mnote.get(*table)?
            };
            let full = if table.is_empty() {
                format!("mnote.{name}")
            } else {
                format!("mnote.{table}.{name}")
            };
            target.set(
                *name,
                self.lua.create_function(move |_, ()| -> mlua::Result<()> {
                    Err(mlua::Error::runtime(format!(
                        "{full} works inside a command or a key, not while init.lua loads"
                    )))
                })?,
            )?;
        }
        Ok(())
    }

    /// Calls callback `ix` with what `args` makes, `mnote.buf` and friends
    /// bound to `editor` for the call; returns what it returned first.
    fn call_with(
        &self,
        editor: &mut Editor,
        cx: &mut Context<Editor>,
        ix: usize,
        args: impl FnOnce(&Lua) -> mlua::Result<mlua::MultiValue>,
    ) -> mlua::Result<Value> {
        if self.is_off() {
            return Ok(Value::Nil);
        }
        let f: Function = {
            let state = self.state.borrow();
            let key = state
                .callbacks
                .get(ix)
                .ok_or_else(|| mlua::Error::runtime("that function is gone"))?;
            self.lua.registry_value(key)?
        };
        let args = args(&self.lua)?;
        let editor = RefCell::new(editor);
        let window = RefCell::new(cx);
        let result = self.lua.scope(|scope| {
            let mnote: Table = self.lua.globals().get("mnote")?;
            let buf: Table = mnote.get("buf")?;
            let clipboard: Table = mnote.get("clipboard")?;
            let e = &editor;
            let w = &window;
            clipboard.set(
                "get",
                scope.create_function(move |_, ()| {
                    Ok(w.borrow_mut()
                        .read_from_clipboard()
                        .and_then(|item| item.text()))
                })?,
            )?;
            clipboard.set(
                "set",
                scope.create_function(move |_, text: String| {
                    w.borrow_mut()
                        .write_to_clipboard(gpui::ClipboardItem::new_string(text));
                    Ok(())
                })?,
            )?;
            buf.set(
                "get_lines",
                scope.create_function(move |_, (first, last): (i64, i64)| {
                    let e = e.borrow();
                    let count = line_count(e.buffer.text());
                    let (first, last) = (index(first, count), index(last, count));
                    Ok((first..last.max(first))
                        .map(|l| line_text(&e.buffer, l))
                        .collect::<Vec<_>>())
                })?,
            )?;
            buf.set(
                "set_lines",
                scope.create_function(
                    move |_, (first, last, lines): (i64, i64, Vec<String>)| {
                        let mut e = e.borrow_mut();
                        let e = &mut **e;
                        let (range, new) = line_edit(e.buffer.text(), first, last, &lines);
                        e.vim.edit_text(&mut e.buffer, range, &new);
                        Ok(())
                    },
                )?,
            )?;
            buf.set(
                "get_text",
                scope.create_function(move |_, ()| Ok(e.borrow().buffer.to_string()))?,
            )?;
            buf.set(
                "line_count",
                scope.create_function(move |_, ()| Ok(line_count(e.borrow().buffer.text())))?,
            )?;
            buf.set(
                "cursor",
                scope.create_function(move |_, ()| {
                    let e = e.borrow();
                    let text = e.buffer.text();
                    let line = line_of(text, e.vim.cursor());
                    Ok((line, e.vim.cursor() - line_start(text, line)))
                })?,
            )?;
            buf.set(
                "set_cursor",
                scope.create_function(move |_, (line, col): (usize, usize)| {
                    let mut e = e.borrow_mut();
                    let text = e.buffer.text();
                    let line = line.min(line_count(text).saturating_sub(1));
                    let pos = clamp_normal(text, line_start(text, line) + col);
                    e.vim.set_cursor(pos);
                    Ok(())
                })?,
            )?;
            buf.set(
                "insert",
                scope.create_function(move |_, text: String| {
                    let mut e = e.borrow_mut();
                    let e = &mut **e;
                    let at = e.vim.cursor().min(e.buffer.text().len_chars());
                    e.vim.edit_text(&mut e.buffer, at..at, &text);
                    let end = at + text.chars().count();
                    let pos = clamp_normal(e.buffer.text(), end.saturating_sub(1).max(at));
                    e.vim.set_cursor(pos);
                    Ok(())
                })?,
            )?;
            mnote.set(
                "note",
                scope.create_function(move |_, ()| {
                    Ok(e.borrow().source.key().map(|n| n.to_string()))
                })?,
            )?;
            mnote.set(
                "mode",
                scope.create_function(move |_, ()| Ok(e.borrow().vim.mode().label()))?,
            )?;
            mnote.set(
                "decorate",
                scope.create_function(move |_, (ns, opts): (String, Table)| {
                    let mut e = e.borrow_mut();
                    let (range, kind, start_bias, end_bias) = decoration(&e.buffer, &opts)?;
                    Ok(e.buffer
                        .marks_mut()
                        .add_with_bias(&ns, range, kind, start_bias, end_bias))
                })?,
            )?;
            mnote.set(
                "undecorate",
                scope.create_function(move |_, ns: String| {
                    e.borrow_mut().buffer.marks_mut().clear(&ns);
                    Ok(())
                })?,
            )?;
            f.call::<Value>(args)
        });
        // Outside the call, the stubs explain again.
        let _ = self.install_stubs();
        result
    }
}

/// A line index from Lua: from the start, or negative from the end (`-1`
/// is just past the last line, as in Neovim), within `0..=count`.
fn index(i: i64, count: usize) -> usize {
    let count = count as i64;
    let i = if i < 0 { count + 1 + i } else { i };
    i.clamp(0, count) as usize
}

fn line_text(buf: &mnote_core::Buffer, line: usize) -> String {
    let text = buf.text();
    let start = line_start(text, line);
    let len = mnote_vim::text::line_len(text, line);
    text.slice(start..start + len).to_string()
}

/// What replacing lines `first..last` with `lines` changes: a character
/// range and its new text, keeping a note without a final newline without
/// one.
fn line_edit(
    text: &mnote_core::Rope,
    first: i64,
    last: i64,
    lines: &[String],
) -> (std::ops::Range<usize>, String) {
    let count = line_count(text);
    let (first, last) = (index(first, count), index(last, count));
    let last = last.max(first);
    let len = text.len_chars();
    let at = |l: usize| if l < count { line_start(text, l) } else { len };
    let (from, to) = (at(first), at(last));
    let mut new: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let ends_open = len > 0 && text.char(len - 1) != '\n';
    if to == len && ends_open && !new.is_empty() {
        new.pop();
        if from == len {
            new.insert(0, '\n');
        }
    }
    (from..to, new)
}

/// A config.toml value as Lua sees it.
fn toml_to_lua(lua: &Lua, value: &toml::Value) -> mlua::Result<Value> {
    Ok(match value {
        toml::Value::String(s) => Value::String(lua.create_string(s)?),
        toml::Value::Integer(n) => Value::Integer(*n),
        toml::Value::Float(f) => Value::Number(*f),
        toml::Value::Boolean(b) => Value::Boolean(*b),
        toml::Value::Datetime(d) => Value::String(lua.create_string(d.to_string())?),
        toml::Value::Array(items) => {
            let table = lua.create_table()?;
            for (i, item) in items.iter().enumerate() {
                table.set(i + 1, toml_to_lua(lua, item)?)?;
            }
            Value::Table(table)
        }
        toml::Value::Table(t) => {
            let table = lua.create_table()?;
            for (k, v) in t {
                table.set(k.as_str(), toml_to_lua(lua, v)?)?;
            }
            Value::Table(table)
        }
    })
}

/// `~/.config/mnote/init.lua`.
pub fn init_path() -> Option<PathBuf> {
    Some(Config::path()?.parent()?.join("init.lua"))
}

impl Editor {
    /// Puts what the script registered into the command registry, the
    /// keymap's defaults and Vim's mappings.
    pub(super) fn install_plugin(&mut self, host: &Rc<LuaHost>) {
        let mut state = host.state.borrow_mut();
        for (name, desc, ix) in std::mem::take(&mut state.commands) {
            let h = host.clone();
            let repeat = name.clone();
            crate::commands::register(
                Command::new(
                    &name,
                    &desc,
                    &format!("{desc} (from {}).", host.source),
                    Run::Action(Rc::new(move |e, call, cx| {
                        e.run_lua(&h, ix, call, Some(&repeat), cx)
                    })),
                )
                .shown_as(&name)
                .ex(&name, name.len(), ArgKind::Any),
            );
        }
        let bindings = std::mem::take(&mut state.bindings);
        let subscriptions = std::mem::take(&mut state.subscriptions);
        let blocks = std::mem::take(&mut state.blocks);
        drop(state);
        for (kind, pattern, ix) in subscriptions {
            let h = host.clone();
            self.events.on(
                &[kind],
                Rc::new(move |e, event, cx| {
                    let note = e.source.key().map(|n| n.to_string());
                    if let Some(pattern) = &pattern
                        && !note.as_deref().is_some_and(|n| glob(pattern, n))
                    {
                        return;
                    }
                    let event = event.clone();
                    e.run_lua_with(&h, ix, None, cx, move |lua| {
                        Ok(mlua::MultiValue::from_vec(vec![Value::Table(event_table(
                            lua,
                            &event,
                            note.as_deref(),
                        )?)]))
                    });
                    // What the handler changed doesn't come back to it.
                    let now = e.snapshot();
                    e.events.resync(now);
                }),
            );
        }
        for (lang, ix) in blocks {
            self.register_lua_block(host, lang, ix);
        }
        for (mode, keys, rhs, desc) in bindings {
            // A hidden command for the key; `i` / `v` keys reach it through
            // a Vim mapping to its ex name (letters only).
            let ex = format!("Luakey{}", letters(next_key_id()));
            let run = match rhs {
                Rhs::Callback(ix) => {
                    let h = host.clone();
                    Run::Action(Rc::new(move |e, call, cx| {
                        e.run_lua(&h, ix, call, None, cx)
                    }))
                }
                Rhs::Ex(line) => Run::Ex(line),
                Rhs::Command(id) => Run::Action(Rc::new(move |e, _, cx| {
                    if let Some(ix) = crate::commands::find(&id) {
                        e.run_command(ix, cx);
                    }
                })),
            };
            let about = desc.unwrap_or_else(|| format!("A key from {}.", host.source));
            let mut command = Command::ex_only(&ex, ex.len(), ArgKind::None, &about, run);
            if mode == 'n' {
                command = command.bind(&keys);
            }
            crate::commands::register(command);
            if mode != 'n' {
                let map = format!("{mode}noremap {keys} <Cmd>{ex}<CR>");
                let _ = self.vim.execute(&map, &mut self.buffer);
            }
        }
    }

    /// A block a script rendered: its rows, in the note's font, beside the
    /// fence line's number.
    fn lua_block_row(
        &self,
        number: gpui::SharedString,
        current: bool,
        rows: &[BlockRow],
        p: &crate::theme::Palette,
    ) -> gpui::AnyElement {
        use crate::theme::{StyledExt, Theme, size, space, text};
        use gpui::{ParentElement, div, px};
        let ink = |name: &Option<String>| {
            name.as_deref()
                .and_then(|n| p.token(n))
                .map_or(Theme::color(p.ink), Theme::color)
        };
        div()
            .flex()
            .w_full()
            .py(px(space::S1))
            .when(current, |row| row.bg(Theme::color(p.well)))
            .child(
                div()
                    .flex_none()
                    .w(px(size::GUTTER_W))
                    .pr(px(space::S8))
                    .text_right()
                    .text_style(text::NOTE_BODY)
                    .text_color(Theme::color(if current { p.ink } else { p.ink_3 }))
                    .child(number),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .max_w(px(size::MEASURE))
                    .px(px(space::S3))
                    .py(px(space::S2))
                    .rounded(px(crate::theme::radius::MD))
                    .bg(Theme::color(p.well))
                    .text_style(text::NOTE_BODY)
                    .children(rows.iter().map(|row| {
                        div()
                            .text_color(ink(&row.color))
                            .child(gpui::SharedString::from(row.text.clone()))
                    })),
            )
            .into_any_element()
    }

    pub(super) fn show_plugin_notices(&mut self, host: &LuaHost, cx: &mut Context<Self>) {
        let notices = std::mem::take(&mut host.state.borrow_mut().notices);
        if let Some(last) = notices.last() {
            self.show_toast(last.clone(), false, cx);
        }
    }

    /// Runs Lua callback `ix` as one undo step; `.` repeats it (as `:name`)
    /// when it changed the text and has an ex name.
    fn run_lua(
        &mut self,
        host: &Rc<LuaHost>,
        ix: usize,
        call: &Invocation,
        repeat: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let args = call.args.clone().unwrap_or_default();
        let bang = call.bang;
        self.run_lua_with(host, ix, repeat, cx, move |lua| {
            let arg = lua.create_table()?;
            arg.set("args", args)?;
            arg.set("bang", bang)?;
            Ok(mlua::MultiValue::from_vec(vec![Value::Table(arg)]))
        });
    }

    /// Runs Lua callback `ix` with what `args` makes, as one undo step, then
    /// starts what it asked for (pickers, jobs) and shows its notices.
    /// Returns what it returned (`Nil` after an error, which is shown).
    fn run_lua_with(
        &mut self,
        host: &Rc<LuaHost>,
        ix: usize,
        repeat: Option<&str>,
        cx: &mut Context<Self>,
        args: impl FnOnce(&Lua) -> mlua::Result<mlua::MultiValue>,
    ) -> Value {
        let started = Instant::now();
        let revision = self.buffer.revision();
        self.buffer.begin_change(self.vim.cursor());
        // Edits the script makes are recorded as its own.
        let user = self.vim.set_origin(host.origin());
        let result = host.call_with(self, cx, ix, args);
        self.vim.set_origin(user);
        let cursor = clamp_normal(self.buffer.text(), self.vim.cursor());
        self.vim.set_cursor(cursor);
        self.buffer.end_change(cursor);
        self.sync_lines();
        if self.buffer.revision() != revision {
            self.notice = None;
            self.schedule_autosave(cx);
            if let Some(name) = repeat {
                self.vim.repeat_with(name);
            }
        }
        let took = started.elapsed();
        self.start_requests(host, cx);
        let value = match result {
            Err(err) => {
                let text = if host.failed() {
                    format!(
                        "{} is turned off for this session after {MAX_ERRORS} errors: {}",
                        host.source,
                        host.message(&err)
                    )
                } else {
                    host.message(&err)
                };
                self.show_toast(text, false, cx);
                Value::Nil
            }
            Ok(value) => {
                if took > SLOW {
                    self.show_toast(
                        format!(
                            "{}: that took {} ms; keys wait for it",
                            host.source,
                            took.as_millis()
                        ),
                        false,
                        cx,
                    );
                } else {
                    self.show_plugin_notices(host, cx);
                }
                value
            }
        };
        cx.notify();
        value
    }

    /// Opens the pickers and starts the jobs a script asked for.
    pub(super) fn start_requests(&mut self, host: &Rc<LuaHost>, cx: &mut Context<Self>) {
        let requests = std::mem::take(&mut host.state.borrow_mut().requests);
        for request in requests {
            match request {
                Request::Picker {
                    title,
                    items,
                    on_select,
                } => {
                    let h = host.clone();
                    let picker = crate::picker::custom::CustomPicker::new(
                        title,
                        items.clone(),
                        Rc::new(move |index| {
                            let h = h.clone();
                            let item = items.get(index).map(|i| i.text.clone()).unwrap_or_default();
                            crate::picker::PickerCallback(Rc::new(move |e, cx| {
                                let item = item.clone();
                                e.run_lua_with(&h, on_select, None, cx, move |lua| {
                                    Ok(mlua::MultiValue::from_vec(vec![
                                        Value::String(lua.create_string(&item)?),
                                        Value::Integer(index as i64 + 1),
                                    ]))
                                });
                            }))
                        }),
                    );
                    self.open_picker(crate::picker::Picker::new(Box::new(picker)), cx);
                }
                Request::Job {
                    cmd,
                    on_stdout,
                    on_exit,
                } => self.start_job(host, cmd, on_stdout, on_exit, cx),
                Request::Open { note, line } => self.open_from_plugin(note, line, cx),
                // Keys and Ex lines run as typed, but their edits are the
                // plugin's.
                Request::Keys(keys) => {
                    let user = self.vim.set_origin(host.origin());
                    for key in keys {
                        self.press(key, cx);
                    }
                    self.vim.set_origin(user);
                }
                Request::Ex(line) => {
                    let user = self.vim.set_origin(host.origin());
                    self.picker_action(crate::picker::PickerAction::RunEx(line), cx);
                    self.vim.set_origin(user);
                }
                Request::ApplyEdits { edits, label } => {
                    let edits = self.char_edits(edits);
                    if let Err(err) = self.apply_edit_group(edits, host.origin(), &label, cx) {
                        self.show_toast(format!("mnote.apply_edits: {err}"), false, cx);
                    }
                }
            }
        }
    }

    /// Line edits to character edits, on each note's current text (open, or
    /// as saved). A note that can't be read gets no edits, and is reported
    /// when the group applies.
    fn char_edits(
        &self,
        edits: Vec<(NotePath, Vec<LineEdit>)>,
    ) -> Vec<super::edit_groups::NoteEdits> {
        edits
            .into_iter()
            .map(|(note, list)| {
                let key = super::DocKey::Note(note.clone());
                let text = if self.source.doc_key() == key {
                    Some(self.buffer.text().clone())
                } else if let Some(i) = self.parked_index(&key) {
                    Some(self.parked[i].buffer.text().clone())
                } else {
                    self.vault
                        .as_ref()
                        .and_then(|v| v.read(&note).ok())
                        .map(|t| mnote_core::Rope::from_str(&t))
                };
                let ranges = text
                    .map(|text| {
                        list.iter()
                            .map(|e| line_edit(&text, e.first, e.last, &e.lines))
                            .collect()
                    })
                    .unwrap_or_default();
                (note, ranges)
            })
            .collect()
    }

    /// `mnote.open`: the note (it must exist), at `line` if given.
    fn open_from_plugin(&mut self, note: NotePath, line: Option<usize>, cx: &mut Context<Self>) {
        let exists = self
            .vault
            .as_ref()
            .is_some_and(|v| v.path_of(&note).exists());
        if !exists {
            self.show_toast(format!("mnote.open: there's no note {note}"), false, cx);
            return;
        }
        match line {
            Some(line) => self.go_to(note, line, cx),
            None => self.switch_to(note, cx),
        }
    }

    /// Runs `cmd` off the UI thread (in the vault's folder), handing each
    /// line it prints, then its exit code, to the callbacks.
    fn start_job(
        &mut self,
        host: &Rc<LuaHost>,
        cmd: Vec<String>,
        on_stdout: Option<usize>,
        on_exit: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        enum Out {
            Line(String),
            Exit(i64),
        }
        let (tx, rx) = async_channel::unbounded::<Out>();
        let dir = self.vault.as_ref().map(|v| v.root().to_path_buf());
        let spawned = std::thread::Builder::new()
            .name("mnote-job".into())
            .spawn(move || {
                use std::io::BufRead;
                let mut command = std::process::Command::new(&cmd[0]);
                command
                    .args(&cmd[1..])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null());
                if let Some(dir) = dir {
                    command.current_dir(dir);
                }
                let code = match command.spawn() {
                    Ok(mut child) => {
                        if let Some(out) = child.stdout.take() {
                            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                                let _ = tx.send_blocking(Out::Line(line));
                            }
                        }
                        child
                            .wait()
                            .ok()
                            .and_then(|s| s.code())
                            .map_or(-1, i64::from)
                    }
                    Err(_) => -1,
                };
                let _ = tx.send_blocking(Out::Exit(code));
            });
        if spawned.is_err() {
            return;
        }
        let h = host.clone();
        self._tasks.push(cx.spawn(async move |this, cx| {
            while let Ok(out) = rx.recv().await {
                let (ix, value) = match out {
                    Out::Line(line) => (on_stdout, Ok::<_, String>(line)),
                    Out::Exit(code) => (on_exit, Err(code.to_string())),
                };
                let Some(ix) = ix else { continue };
                let h = h.clone();
                let alive = this.update(cx, |e, cx| {
                    e.run_lua_with(&h, ix, None, cx, move |lua| {
                        Ok(mlua::MultiValue::from_vec(vec![match value {
                            Ok(line) => Value::String(lua.create_string(&line)?),
                            Err(code) => Value::Integer(code.parse().unwrap_or(-1)),
                        }]))
                    });
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    /// A block renderer for fences of `lang`: the block's rows come from
    /// the script, rendered after the frame (never while drawing) and kept
    /// until its text changes. With the cursor in the block, it shows as
    /// typed.
    fn register_lua_block(&mut self, host: &Rc<LuaHost>, lang: String, ix: usize) {
        use mnote_core::styled::{LineKind, line_kind};
        let h = host.clone();
        let name = format!("lua:{lang}");
        self.register_block(
            &name,
            Rc::new(move |e, line, cx| {
                let rope = e.buffer.text();
                // The block this line belongs to: an open fence, a line in
                // it, or the close fence right after one.
                let block = match line.kind {
                    LineKind::FenceOpen { lang: l, block } | LineKind::Code { lang: l, block }
                        if *l == lang =>
                    {
                        block.clone()
                    }
                    LineKind::FenceClose if line.ix > 0 => {
                        match line_kind(e.tree.as_ref(), rope, line.ix - 1) {
                            LineKind::Code { lang: l, block }
                            | LineKind::FenceOpen { lang: l, block }
                                if l == lang =>
                            {
                                block
                            }
                            _ => return None,
                        }
                    }
                    _ => return None,
                };
                // Editing it: as typed.
                if (block.start.saturating_sub(1)..=block.end).contains(&line.cursor_line) {
                    return None;
                }
                let text: Vec<String> = block.clone().map(|l| line_text(&e.buffer, l)).collect();
                let key = (lang.clone(), text.join("\n"));
                let cached = h.block_rows.borrow().get(&key).cloned();
                let rows = match cached {
                    Some(Some(rows)) => rows,
                    Some(None) => return None,
                    None => {
                        // Rendered by the script after this frame.
                        h.block_rows.borrow_mut().insert(key.clone(), None);
                        let h = h.clone();
                        cx.spawn(async move |this, cx| {
                            let _ = this.update(cx, |e, cx| {
                                let lines = text.clone();
                                let value = e.run_lua_with(&h, ix, None, cx, move |lua| {
                                    Ok(mlua::MultiValue::from_vec(vec![Value::Table(
                                        lua.create_sequence_from(lines)?,
                                    )]))
                                });
                                let rows = block_rows(value);
                                h.block_rows.borrow_mut().insert(key, Some(Rc::new(rows)));
                                cx.notify();
                            });
                        })
                        .detach();
                        return None;
                    }
                };
                if !matches!(line.kind, LineKind::FenceOpen { .. }) {
                    return Some(gpui::div().h(gpui::px(0.)).into_any_element());
                }
                Some(e.lua_block_row(line.gutter.clone(), line.current, &rows, line.palette))
            }),
        );
    }
}

/// The event called `name` (`"NoteSaved"`).
fn event_kind(name: &str) -> Option<EventKind> {
    Some(match name {
        "NoteOpened" => EventKind::NoteOpened,
        "NoteSavePre" => EventKind::NoteSavePre,
        "NoteSaved" => EventKind::NoteSaved,
        "TextChanged" => EventKind::TextChanged,
        "ModeChanged" => EventKind::ModeChanged,
        "InsertLeave" => EventKind::InsertLeave,
        "CursorMoved" => EventKind::CursorMoved,
        "VaultChanged" => EventKind::VaultChanged,
        "ConfigApplied" => EventKind::ConfigApplied,
        "PaneFocused" => EventKind::PaneFocused,
        _ => return None,
    })
}

/// An event as a handler gets it: `{ event = "NoteSaved", note = "…" }`,
/// with `line` / `column`, `from` / `to` (modes) or `revision` as it has.
fn event_table(lua: &Lua, event: &EditorEvent, note: Option<&str>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("event", format!("{:?}", event.kind()))?;
    t.set("note", note)?;
    match event {
        EditorEvent::CursorMoved { line, column } => {
            t.set("line", *line)?;
            t.set("column", *column)?;
        }
        EditorEvent::ModeChanged { from, to } => {
            t.set("from", from.label())?;
            t.set("to", to.label())?;
        }
        EditorEvent::TextChanged {
            revision, origin, ..
        } => {
            t.set("revision", *revision)?;
            t.set("origin", origin.to_string())?;
        }
        EditorEvent::NoteSaved { note } | EditorEvent::NoteSavePre { note } => {
            t.set("note", note.to_string())?;
        }
        _ => {}
    }
    Ok(t)
}

/// A pattern with `*` (any characters) against a note path:
/// `journal/*` matches `journal/2026-11-24.md`.
fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if parts.len() == 1 {
        return pattern == text;
    }
    if !text.starts_with(first) || !text[first.len()..].ends_with(last) {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

/// A decoration from Lua options: `line` (0-based) and, for a highlight,
/// `col` / `end_col` (default: the whole line) with `fg` / `bg`; or
/// `line_bg`; or `text` (with `color`, `right`); or `sign` (with `color`).
/// A decoration's range, kind, and the bias of its two ends.
type Decoration = (
    std::ops::Range<usize>,
    mnote_core::MarkKind,
    mnote_core::text::Bias,
    mnote_core::text::Bias,
);

fn decoration(buf: &mnote_core::Buffer, opts: &Table) -> mlua::Result<Decoration> {
    use mnote_core::MarkKind;
    use mnote_core::text::Bias;
    let text = buf.text();
    let count = line_count(text).max(1);
    let line: usize = opts.get::<usize>("line")?.min(count - 1);
    let start = line_start(text, line);
    let len = mnote_vim::text::line_len(text, line);
    let col: usize = opts.get::<Option<usize>>("col")?.unwrap_or(0).min(len);
    let end_col: usize = opts
        .get::<Option<usize>>("end_col")?
        .unwrap_or(len)
        .min(len);
    let color = opts.get::<Option<String>>("color")?;
    let ghost = opts.get::<Option<String>>("ghost")?;
    // Ghost text follows typing at its position; everything else stays.
    let bias = match opts.get::<Option<String>>("bias")?.as_deref() {
        Some("after") => Bias::After,
        Some("before") => Bias::Before,
        Some(other) => {
            return Err(mlua::Error::runtime(format!(
                "bias is \"before\" or \"after\", not {other:?}"
            )));
        }
        None if ghost.is_some() => Bias::After,
        None => Bias::Before,
    };
    if let Some(text) = ghost {
        let kind = MarkKind::Ghost {
            text,
            color: color.unwrap_or_else(|| "ink_3".into()),
        };
        return Ok((start + col..start + col, kind, bias, bias));
    }
    let kind = if let Some(bg) = opts.get::<Option<String>>("line_bg")? {
        MarkKind::LineBackground { background: bg }
    } else if let Some(t) = opts.get::<Option<String>>("text")? {
        MarkKind::VirtualText {
            text: t,
            color: color.unwrap_or_else(|| "ink_3".into()),
            right: opts.get::<Option<bool>>("right")?.unwrap_or(false),
        }
    } else if let Some(sign) = opts.get::<Option<String>>("sign")? {
        MarkKind::Sign {
            text: sign,
            color: color.unwrap_or_else(|| "ink_3".into()),
        }
    } else {
        MarkKind::Highlight {
            color: opts.get::<Option<String>>("fg")?,
            background: opts.get::<Option<String>>("bg")?,
        }
    };
    Ok((start + col..start + end_col.max(col), kind, bias, bias))
}

/// A block renderer's result as rows: strings, or `{ text = …, color = … }`.
fn block_rows(value: Value) -> Vec<BlockRow> {
    let Value::Table(t) = value else {
        return vec![BlockRow {
            text: "(the block renderer returned no rows)".into(),
            color: Some("danger".into()),
        }];
    };
    t.sequence_values::<Value>()
        .filter_map(Result::ok)
        .map(|row| match row {
            Value::Table(r) => BlockRow {
                text: r
                    .get::<Option<String>>("text")
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
                color: r.get::<Option<String>>("color").ok().flatten(),
            },
            Value::String(s) => BlockRow {
                text: s.to_string_lossy(),
                color: None,
            },
            other => BlockRow {
                text: format!("{other:?}"),
                color: None,
            },
        })
        .collect()
}

/// Every `mnote` function: its call and what it does. The API reference
/// (`docs/plugins.md`) is generated from this, and a test checks that each
/// function the Lua state has is here.
pub const API: &[(&str, &str, &str)] = &[
    (
        "mnote.command.create",
        "(name, fn, { desc })",
        "Adds `:name` (letters only) and a palette entry. `fn` gets `{ args, bang }`. Its edits are one undo step, and `.` repeats it.",
    ),
    (
        "mnote.keymap.set",
        "(mode, keys, fn | \"command\" | \":ex line\", { desc })",
        "Binds keys in Vim notation (`<leader>t`, `<C-w>x`). `\"n\"` binds in NORMAL through the keymap; `\"i\"` and `\"v\"` through Vim mappings.",
    ),
    (
        "mnote.on",
        "(event, [{ pattern }], fn)",
        "Calls `fn(event)` for an editor event (see Events), optionally only for notes matching `pattern` (`journal/*`). Runs after the frame; `NoteSavePre` runs before the write and may edit.",
    ),
    ("mnote.notify", "(text)", "Shows a toast."),
    (
        "mnote.config",
        "(name) -> table",
        "The `[plugins.<name>]` table from config.toml (empty without one).",
    ),
    (
        "mnote.picker.open",
        "({ title, items, on_select })",
        "Opens a picker. `items` are strings or `{ text, detail }`; `on_select(text, index)` runs for the chosen one.",
    ),
    (
        "mnote.job.start",
        "(cmd, { on_stdout, on_exit })",
        "Runs a command (`{ \"git\", \"log\" }`) in the vault's folder, off the UI thread. `on_stdout(line)` for each line, then `on_exit(code)`.",
    ),
    (
        "mnote.blocks.register",
        "(lang, { render })",
        "Draws ```lang fences in the styled view: `render(lines)` returns rows, strings or `{ text, color }`. Rendered after the frame and kept until the block changes; with the cursor in it, it shows as typed.",
    ),
    (
        "mnote.buf.get_lines",
        "(first, last) -> lines",
        "Lines `first` to `last` (0-based, `last` not included; negative counts from the end, `-1` being past the last line).",
    ),
    (
        "mnote.buf.set_lines",
        "(first, last, lines)",
        "Replaces those lines (an empty range inserts).",
    ),
    ("mnote.buf.get_text", "() -> text", "The whole note."),
    ("mnote.buf.line_count", "() -> n", "How many lines it has."),
    (
        "mnote.buf.cursor",
        "() -> line, column",
        "Where the cursor is (0-based).",
    ),
    (
        "mnote.buf.set_cursor",
        "(line, column)",
        "Moves the cursor.",
    ),
    ("mnote.buf.insert", "(text)", "Inserts text at the cursor."),
    (
        "mnote.note",
        "() -> path | nil",
        "The open note's path in the vault.",
    ),
    (
        "mnote.mode",
        "() -> mode",
        "Vim's mode: `NORMAL`, `INSERT`, `VISUAL`…",
    ),
    (
        "mnote.decorate",
        "(namespace, opts) -> id",
        "Draws over the note, moving with edits: `{ line, col, end_col, fg, bg }` colours text; `{ line, line_bg }` a line; `{ line, text, color, right }` adds text after it; `{ line, sign, color }` a gutter sign; `{ line, col, ghost, color }` draws text inside the line that isn't part of it (the cursor skips it). `bias = \"before\"` or `\"after\"`: which side of text typed exactly at an end the decoration stays on (ghost text: after; the rest: before). Colours are theme token names (`match`, `danger`, `ink_3`).",
    ),
    (
        "mnote.undecorate",
        "(namespace)",
        "Removes every decoration in the namespace.",
    ),
    (
        "mnote.open",
        "(path, { line })",
        "Opens a note of the vault (`journal/today`, `.md` optional), at `line` (0-based) if given, once the function returns. A note that doesn't exist is reported.",
    ),
    (
        "mnote.apply_edits",
        "({ { note, first, last, lines }, … }, { label })",
        "Replaces lines `first..last` (as `mnote.buf.set_lines` counts them) in several notes as one undo step, once the function returns. Notes that aren't open are opened in the background, edited and saved. `u` in any of them undoes the whole edit in all of them that haven't changed since. Edits to one note mustn't overlap.",
    ),
    (
        "mnote.feedkeys",
        "(keys)",
        "Runs keys in Vim notation (`ggdd`, `<C-w>v`, `<leader>f`) as if typed, through the keymap, mappings and Vim, once the function returns.",
    ),
    (
        "mnote.cmd",
        "(line)",
        "Runs an Ex line (`set so=5`, `w`, `daily`; a leading `:` is fine) once the function returns.",
    ),
    (
        "mnote.clipboard.get",
        "() -> text | nil",
        "The system clipboard's text.",
    ),
    (
        "mnote.clipboard.set",
        "(text)",
        "Puts text on the system clipboard.",
    ),
];

/// The events `mnote.on` takes.
pub const EVENTS: &[(&str, &str)] = &[
    (
        "NoteOpened",
        "the focused pane shows another note (and at start); `note`",
    ),
    (
        "NoteSavePre",
        "a note is about to be written; runs before it, may edit; `note`",
    ),
    ("NoteSaved", "a note was written; `note`"),
    (
        "TextChanged",
        "the note's text changed; `note`, `revision`, `origin` (who made the latest change: `user`, `plugin:<name>`, `lsp:<server>`, `agent:<session>`, `prediction`, `external`)",
    ),
    ("ModeChanged", "Vim's mode changed; `from`, `to`"),
    ("InsertLeave", "INSERT or REPLACE ended"),
    ("CursorMoved", "the cursor moved; `line`, `column`"),
    ("VaultChanged", "notes changed on disk outside mNote"),
    ("ConfigApplied", "config.toml was applied"),
    ("PaneFocused", "another pane took focus"),
];

/// The plugin API reference, as `docs/plugins.md` holds it (written by a
/// test, so it never goes stale).
#[cfg(test)]
pub fn api_reference() -> String {
    let mut out = String::from(
        "<!-- Generated from crates/mnote-app/src/editor/plugins.rs (API, EVENTS).\n     MNOTE_WRITE_DOCS=1 cargo test -p mnote-app plugin_docs rewrites it. -->\n\n\
# mNote plugins\n\n\
Plugins are Lua 5.4. Each lives in a folder of its own:\n\n\
```\n~/.config/mnote/plugins/tasks/\n  plugin.toml   name, version, api, description\n  init.lua      runs when the plugin loads\n  lua/          its own modules, for require\n```\n\n",
    );
    out.push_str(&format!(
        "plugin.toml:\n\n```toml\nname = \"tasks\"\nversion = \"1.0.0\"\napi = {}\ndescription = \"Marks open tasks and lists them\"\n```\n\n",
        super::plugin_list::PLUGIN_API
    ));
    out.push_str(
        "A plugin runs when config.toml turns it on (above any `[table]`, or as its own table):\n\n\
```toml\nplugins.tasks.enabled = true\n```\n\n\
The first time it would run, or after its code changed, mNote asks: a plugin can do anything you can. \
`:plugins` lists the installed plugins, whether each runs, and how long it took to load. \
A plugin for another API version is skipped; one with an error is reported and the others still load; \
one that fails three times is turned off until mNote restarts. Commands and keys slower than 2 ms say so. \
`~/.config/mnote/init.lua`, your own, loads last and needs no folder.\n\n\
Settings for a plugin go in its table and reach it through `mnote.config(\"tasks\")`.\n\n## API\n\n",
    );
    for (name, call, about) in API {
        out.push_str(&format!("- `{name}{call}`: {about}\n"));
    }
    out.push_str("\n## Events\n\nEach handler gets a table with `event` and `note`, and:\n\n");
    for (name, about) in EVENTS {
        out.push_str(&format!("- `{name}`: {about}\n"));
    }
    out.push_str("\n## Examples\n\n`examples/plugins/` holds `tasks` (signs, a count, a picker and a ```progress block), `wordgoal` (a daily word goal from its settings) and `pins` (harpoon-style pinned notes, with `mnote.open` and the clipboard). Copy a folder into `~/.config/mnote/plugins/` and turn it on.\n");
    out
}

impl Editor {
    /// `:pluginapi`: every `mnote` function and event, in short.
    pub(super) fn list_plugin_api(&mut self) {
        let mut lines: Vec<String> = API
            .iter()
            .map(|(name, call, _)| format!("{name}{call}"))
            .collect();
        lines.push(format!(
            "Events: {}",
            EVENTS
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join(", ")
        ));
        lines.push("More in docs/plugins.md".into());
        self.vim.show_message(lines.join("\n"));
    }
}

/// A fresh number for a key's hidden command, unique across plugins.
fn next_key_id() -> usize {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// `0` → `a`, `25` → `z`, `26` → `ba`: a number as letters, for ex names.
fn letters(mut n: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'a' + (n % 26) as u8);
        n /= 26;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_api_function_is_documented() {
        let dir = std::env::temp_dir().join(format!("mnote-api-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("init.lua");
        std::fs::write(&path, "").expect("write");
        let host = LuaHost::load(&path, &Config::default()).expect("loads");
        let names: Vec<String> = host
            .lua
            .load(
                r#"
                local out = {}
                local function walk(t, prefix)
                  for k, v in pairs(t) do
                    if type(v) == "function" then table.insert(out, prefix .. k)
                    elseif type(v) == "table" then walk(v, prefix .. k .. ".") end
                  end
                end
                walk(mnote, "mnote.")
                return out
                "#,
            )
            .eval()
            .expect("names");
        for name in &names {
            assert!(
                API.iter().any(|(n, _, _)| n == name),
                "{name} isn't documented"
            );
        }
        for (name, _, _) in API {
            assert!(
                names.contains(&name.to_string()),
                "{name} is documented but missing"
            );
        }
        for (event, _) in EVENTS {
            assert!(event_kind(event).is_some(), "{event}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Loads `code` as an init.lua.
    fn host(code: &str) -> Result<LuaHost, String> {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("mnote-req-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("init.lua");
        std::fs::write(&path, code).expect("write");
        let host = LuaHost::load(&path, &Config::default());
        let _ = std::fs::remove_dir_all(&dir);
        host
    }

    #[test]
    fn open_feedkeys_and_cmd_wait_for_the_editor() {
        let h = host(
            r#"
            mnote.open("journal/today", { line = 4 })
            mnote.open("ideas.md")
            mnote.feedkeys("gg<C-w>v")
            mnote.cmd(":set so=3")
            mnote.cmd("   ")
            mnote.apply_edits({
              { note = "a", first = 0, last = 1, lines = { "x" } },
              { note = "b.md", first = 2, last = 2, lines = { "y" } },
              { note = "a", first = 3, last = 4 },
            }, { label = "Rename tag" })
            mnote.apply_edits({})
            "#,
        )
        .expect("loads");
        let requests = std::mem::take(&mut h.state.borrow_mut().requests);
        let shown: Vec<String> = requests
            .iter()
            .map(|r| match r {
                Request::Open { note, line } => format!("open {note} {line:?}"),
                Request::Keys(keys) => format!("keys {}", mnote_vim::key_notation(keys)),
                Request::Ex(line) => format!("ex {line}"),
                Request::ApplyEdits { edits, label } => format!(
                    "apply {label}: {}",
                    edits
                        .iter()
                        .map(|(note, list)| format!("{note}×{}", list.len()))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                Request::Picker { .. } | Request::Job { .. } => "other".into(),
            })
            .collect();
        assert_eq!(
            shown,
            [
                "open journal/today.md Some(4)",
                "open ideas.md None",
                "keys gg<C-w>v",
                "ex set so=3",
                "apply Rename tag: a.md×2 b.md×1",
            ]
        );
        let err = host(r#"mnote.open("../outside")"#)
            .err()
            .unwrap_or_default();
        assert!(err.contains("isn't a note path"), "{err}");
        // The clipboard needs the window: outside a call it says so.
        let err = host("mnote.clipboard.get()").err().unwrap_or_default();
        assert!(err.contains("inside a command or a key"), "{err}");
    }

    #[test]
    fn the_example_plugins_load() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).expect("examples") {
            let path = entry.expect("entry").path().join("init.lua");
            if !path.exists() {
                continue;
            }
            seen += 1;
            if let Err(err) = LuaHost::load(&path, &Config::default()) {
                panic!("{}: {err}", path.display());
            }
        }
        assert!(seen >= 3);
    }

    #[test]
    fn plugin_docs_are_up_to_date() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/plugins.md");
        let want = api_reference();
        if std::env::var_os("MNOTE_WRITE_DOCS").is_some() {
            std::fs::create_dir_all(path.parent().expect("docs")).expect("docs dir");
            std::fs::write(&path, &want).expect("write docs");
        }
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            have == want,
            "docs/plugins.md is out of date: MNOTE_WRITE_DOCS=1 cargo test -p mnote-app plugin_docs"
        );
    }

    #[test]
    fn patterns_events_and_rows() {
        assert!(glob("journal/*", "journal/2026-11-24.md"));
        assert!(!glob("journal/*", "ideas/a.md"));
        assert!(glob("*.md", "a/b.md"));
        assert!(glob("a*c*e", "abcde"));
        assert!(!glob("a*c*e", "abde"));
        assert!(glob("exact.md", "exact.md") && !glob("exact.md", "other.md"));
        assert_eq!(event_kind("NoteSaved"), Some(EventKind::NoteSaved));
        assert_eq!(event_kind("Nope"), None);
        let lua = Lua::new();
        let value: Value = lua
            .load(r#"return { "plain", { text = "red", color = "danger" } }"#)
            .eval()
            .expect("rows");
        assert_eq!(
            block_rows(value),
            [
                BlockRow {
                    text: "plain".into(),
                    color: None
                },
                BlockRow {
                    text: "red".into(),
                    color: Some("danger".into())
                }
            ]
        );
        assert_eq!(block_rows(Value::Nil)[0].color.as_deref(), Some("danger"));
    }

    #[test]
    fn decorations_from_lua_options() {
        use mnote_core::MarkKind;
        let lua = Lua::new();
        let buf = rope("one\ntwo words\n");
        let opts = |code: &str| -> Table { lua.load(code).eval().expect("a table") };
        let (range, kind, ..) =
            decoration(&buf, &opts("return { line = 1, col = 4, bg = 'match' }"))
                .expect("a highlight");
        assert_eq!(range, 8..13);
        assert_eq!(
            kind,
            MarkKind::Highlight {
                color: None,
                background: Some("match".into())
            }
        );
        let (_, kind, ..) = decoration(
            &buf,
            &opts("return { line = 0, text = '3 tasks', right = true }"),
        )
        .expect("virtual text");
        assert!(matches!(kind, MarkKind::VirtualText { right: true, .. }));
        let (_, kind, ..) =
            decoration(&buf, &opts("return { line = 9, sign = '●' }")).expect("a sign");
        assert!(
            matches!(kind, MarkKind::Sign { .. }),
            "a line past the end clamps"
        );
        let (_, kind, ..) = decoration(&buf, &opts("return { line = 0, line_bg = 'danger_wash' }"))
            .expect("a line background");
        assert!(matches!(kind, MarkKind::LineBackground { .. }));
        // Ghost text: a point inside the line, following what's typed there.
        use mnote_core::text::Bias;
        let (range, kind, start, end) =
            decoration(&buf, &opts("return { line = 1, col = 3, ghost = ': x' }")).expect("ghost");
        assert_eq!(range, 7..7);
        assert!(matches!(kind, MarkKind::Ghost { ref text, .. } if text == ": x"));
        assert_eq!((start, end), (Bias::After, Bias::After));
        let (.., start, _) = decoration(
            &buf,
            &opts("return { line = 0, sign = 's', bias = 'after' }"),
        )
        .expect("biased");
        assert_eq!(start, Bias::After);
        assert!(decoration(&buf, &opts("return { line = 0, bias = 'left' }")).is_err());
    }

    fn rope(text: &str) -> mnote_core::Buffer {
        mnote_core::Buffer::new(text)
    }

    fn edit(text: &str, first: i64, last: i64, lines: &[&str]) -> String {
        let buf = rope(text);
        let lines: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
        let (range, new) = line_edit(buf.text(), first, last, &lines);
        let mut out: String = buf.to_string();
        let start = out
            .char_indices()
            .nth(range.start)
            .map_or(out.len(), |(b, _)| b);
        let end = out
            .char_indices()
            .nth(range.end)
            .map_or(out.len(), |(b, _)| b);
        out.replace_range(start..end, &new);
        out
    }

    #[test]
    fn set_lines_replaces_inserts_and_appends_like_neovim() {
        assert_eq!(edit("a\nb\nc\n", 1, 2, &["B"]), "a\nB\nc\n");
        assert_eq!(edit("a\nb\nc\n", 1, 1, &["x"]), "a\nx\nb\nc\n", "insert");
        assert_eq!(edit("a\nb\nc\n", 0, 2, &[]), "c\n", "delete");
        assert_eq!(edit("a\nb\n", -1, -1, &["z"]), "a\nb\nz\n", "append");
        assert_eq!(edit("a\nb", -1, -1, &["z"]), "a\nb\nz", "no final newline");
        assert_eq!(edit("a\nb", 1, 2, &["B"]), "a\nB");
        assert_eq!(edit("a\n", 0, -1, &["x", "y"]), "x\ny\n", "everything");
    }

    #[test]
    fn indexes_count_from_the_end_when_negative() {
        assert_eq!(index(0, 3), 0);
        assert_eq!(index(-1, 3), 3);
        assert_eq!(index(-2, 3), 2);
        assert_eq!(index(9, 3), 3);
        assert_eq!(index(-9, 3), 0);
        assert_eq!(letters(0), "a");
        assert_eq!(letters(27), "bb");
    }

    #[test]
    fn init_lua_registers_commands_and_keys_and_errors_name_the_file() {
        let dir = std::env::temp_dir().join(format!("mnote-lua-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("init.lua");
        std::fs::write(
            &path,
            r#"
            mnote.command.create("Today", function(call) end, { desc = "Insert today" })
            mnote.keymap.set("n", "<leader>t", "Today")
            mnote.keymap.set("i", "jj", function() end)
            mnote.notify("hello " .. (mnote.config("demo").greeting or "?"))
            local ok, err = pcall(mnote.buf.get_text)
            assert(not ok and tostring(err):find("inside a command"))
            mnote.on("NoteSaved", function(ev) end)
            mnote.on("TextChanged", { pattern = "journal/*" }, function(ev) end)
            mnote.picker.open({ title = "T", items = { "a", { text = "b", detail = "x" } },
              on_select = function(item, i) end })
            mnote.job.start({ "echo", "hi" }, { on_exit = function(code) end })
            mnote.blocks.register("chart", { render = function(lines) return lines end })
            assert(not pcall(mnote.on, "Nope", function() end))
            assert(not pcall(mnote.decorate, "x", { line = 0 }))
            "#,
        )
        .expect("write");
        let mut config = Config::default();
        let mut demo = toml::Table::new();
        demo.insert("greeting".into(), toml::Value::String("world".into()));
        config.plugins.insert("demo".into(), demo);
        let host = LuaHost::load(&path, &config).expect("loads");
        let state = host.state.borrow();
        assert_eq!(state.commands.len(), 1);
        assert_eq!(state.commands[0].0, "Today");
        assert_eq!(state.bindings.len(), 2);
        assert_eq!(state.notices, ["hello world"]);
        assert_eq!(state.subscriptions.len(), 2);
        assert_eq!(state.subscriptions[1].1.as_deref(), Some("journal/*"));
        assert_eq!(state.requests.len(), 2, "a picker and a job");
        assert_eq!(state.blocks[0].0, "chart");
        drop(state);

        std::fs::write(&path, "mnote.command.create('bad name', function() end)").expect("write");
        let err = LuaHost::load(&path, &config).err().expect("an error");
        assert!(err.starts_with("init.lua: "), "{err}");
        assert!(err.contains("letters only"), "{err}");
        std::fs::write(&path, "this is not lua").expect("write");
        assert!(LuaHost::load(&path, &config).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
