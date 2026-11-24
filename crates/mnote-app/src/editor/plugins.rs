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

use gpui::Context;
use mlua::{Function, Lua, RegistryKey, Table, Value};
use mnote_core::config::Config;
use mnote_vim::ex::ArgKind;
use mnote_vim::text::{clamp_normal, line_count, line_of, line_start};

use super::Editor;
use crate::commands::{Command, Invocation, Run};

/// A command or key slower than this says so.
const SLOW: Duration = Duration::from_millis(2);

/// What a key binding runs.
enum Rhs {
    Callback(usize),
    Command(String),
    Ex(String),
}

/// What `init.lua` asked for while it ran, for the editor to install.
#[derive(Default)]
struct HostState {
    callbacks: Vec<RegistryKey>,
    commands: Vec<(String, String, usize)>,
    bindings: Vec<(char, String, Rhs, Option<String>)>,
    /// `mnote.notify` texts waiting to be shown.
    notices: Vec<String>,
    /// config.toml's `[plugins.*]` tables, for `mnote.config`.
    plugins: std::collections::BTreeMap<String, toml::Table>,
}

/// A running Lua state and what its script registered.
pub struct LuaHost {
    lua: Lua,
    state: Rc<RefCell<HostState>>,
    /// Where the code came from, for messages (`init.lua`).
    source: String,
}

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
];

impl LuaHost {
    /// Runs `path` (`init.lua`) with the `mnote` API.
    pub fn load(path: &Path, config: &Config) -> Result<LuaHost, String> {
        let code = std::fs::read_to_string(path)
            .map_err(|err| format!("Couldn't read {}: {err}", path.display()))?;
        let source = path
            .file_name()
            .map_or_else(|| "init.lua".into(), |n| n.to_string_lossy().into_owned());
        let host = LuaHost {
            lua: Lua::new(),
            state: Rc::default(),
            source,
        };
        host.set_plugins(config);
        host.install_api().map_err(|err| host.message(&err))?;
        host.lua
            .load(&code)
            .set_name(format!("@{}", host.source))
            .exec()
            .map_err(|err| host.message(&err))?;
        Ok(host)
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

    /// Calls callback `ix` with `call`, with `mnote.buf` and friends bound
    /// to `editor` for the call.
    fn call(&self, editor: &mut Editor, ix: usize, call: &Invocation) -> mlua::Result<()> {
        let f: Function = {
            let state = self.state.borrow();
            let key = state
                .callbacks
                .get(ix)
                .ok_or_else(|| mlua::Error::runtime("that function is gone"))?;
            self.lua.registry_value(key)?
        };
        let arg = self.lua.create_table()?;
        arg.set("args", call.args.clone().unwrap_or_default())?;
        arg.set("bang", call.bang)?;
        let editor = RefCell::new(editor);
        let result = self.lua.scope(|scope| {
            let mnote: Table = self.lua.globals().get("mnote")?;
            let buf: Table = mnote.get("buf")?;
            let e = &editor;
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
                        let (range, new) = line_edit(&e.buffer, first, last, &lines);
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
            f.call::<()>(arg)
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
    buf: &mnote_core::Buffer,
    first: i64,
    last: i64,
    lines: &[String],
) -> (std::ops::Range<usize>, String) {
    let text = buf.text();
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
    /// Loads `init.lua` (once), registering its commands and keys. Without
    /// one, nothing happens. A mistake in it is shown, and mNote carries on.
    pub(super) fn load_plugins(&mut self, config: &Config, cx: &mut Context<Self>) {
        if let Some(host) = &self.lua {
            host.set_plugins(config);
            return;
        }
        if self.plugins_tried {
            return;
        }
        self.plugins_tried = true;
        let Some(path) = init_path().filter(|p| p.is_file()) else {
            return;
        };
        let host = match LuaHost::load(&path, config) {
            Ok(host) => Rc::new(host),
            Err(err) => {
                self.show_toast(err, false, cx);
                return;
            }
        };
        self.install_plugin(&host);
        self.lua = Some(host.clone());
        self.show_plugin_notices(&host, cx);
    }

    /// Puts what the script registered into the command registry, the
    /// keymap's defaults and Vim's mappings.
    fn install_plugin(&mut self, host: &Rc<LuaHost>) {
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
        drop(state);
        for (n, (mode, keys, rhs, desc)) in bindings.into_iter().enumerate() {
            // A hidden command for the key; `i` / `v` keys reach it through
            // a Vim mapping to its ex name (letters only).
            let ex = format!("Luakey{}", letters(n));
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

    fn show_plugin_notices(&mut self, host: &LuaHost, cx: &mut Context<Self>) {
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
        let started = Instant::now();
        let revision = self.buffer.revision();
        self.buffer.begin_change(self.vim.cursor());
        let result = host.call(self, ix, call);
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
        match result {
            Err(err) => self.show_toast(host.message(&err), false, cx),
            Ok(()) if took > SLOW => self.show_toast(
                format!(
                    "{}: that took {} ms; keys wait for it",
                    host.source,
                    took.as_millis()
                ),
                false,
                cx,
            ),
            Ok(()) => self.show_plugin_notices(host, cx),
        }
        cx.notify();
    }
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

    fn rope(text: &str) -> mnote_core::Buffer {
        mnote_core::Buffer::new(text)
    }

    fn edit(text: &str, first: i64, last: i64, lines: &[&str]) -> String {
        let buf = rope(text);
        let lines: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
        let (range, new) = line_edit(&buf, first, last, &lines);
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
