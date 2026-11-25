<!-- Generated from crates/mnote-app/src/editor/plugins.rs (API, EVENTS).
     MNOTE_WRITE_DOCS=1 cargo test -p mnote-app plugin_docs rewrites it. -->

# mNote plugins

Plugins are Lua 5.4. Each lives in a folder of its own:

```
~/.config/mnote/plugins/tasks/
  plugin.toml   name, version, api, description
  init.lua      runs when the plugin loads
  lua/          its own modules, for require
```

plugin.toml:

```toml
name = "tasks"
version = "1.0.0"
api = 1
description = "Marks open tasks and lists them"
```

A plugin runs when config.toml turns it on (above any `[table]`, or as its own table):

```toml
plugins.tasks.enabled = true
```

The first time it would run, or after its code changed, mNote asks: a plugin can do anything you can. `:plugins` lists the installed plugins, whether each runs, and how long it took to load. A plugin for another API version is skipped; one with an error is reported and the others still load; one that fails three times is turned off until mNote restarts. Commands and keys slower than 2 ms say so. `~/.config/mnote/init.lua`, your own, loads last and needs no folder.

Settings for a plugin go in its table and reach it through `mnote.config("tasks")`.

## API

- `mnote.command.create(name, fn, { desc })`: Adds `:name` (letters only) and a palette entry. `fn` gets `{ args, bang }`. Its edits are one undo step, and `.` repeats it.
- `mnote.keymap.set(mode, keys, fn | "command" | ":ex line", { desc })`: Binds keys in Vim notation (`<leader>t`, `<C-w>x`). `"n"` binds in NORMAL through the keymap; `"i"` and `"v"` through Vim mappings.
- `mnote.on(event, [{ pattern }], fn)`: Calls `fn(event)` for an editor event (see Events), optionally only for notes matching `pattern` (`journal/*`). Runs after the frame; `NoteSavePre` runs before the write and may edit.
- `mnote.notify(text)`: Shows a toast.
- `mnote.config(name) -> table`: The `[plugins.<name>]` table from config.toml (empty without one).
- `mnote.picker.open({ title, items, on_select })`: Opens a picker. `items` are strings or `{ text, detail }`; `on_select(text, index)` runs for the chosen one.
- `mnote.job.start(cmd, { on_stdout, on_exit })`: Runs a command (`{ "git", "log" }`) in the vault's folder, off the UI thread. `on_stdout(line)` for each line, then `on_exit(code)`.
- `mnote.blocks.register(lang, { render })`: Draws ```lang fences in the styled view: `render(lines)` returns rows, strings or `{ text, color }`. Rendered after the frame and kept until the block changes; with the cursor in it, it shows as typed.
- `mnote.buf.get_lines(first, last) -> lines`: Lines `first` to `last` (0-based, `last` not included; negative counts from the end, `-1` being past the last line).
- `mnote.buf.set_lines(first, last, lines)`: Replaces those lines (an empty range inserts).
- `mnote.buf.get_text() -> text`: The whole note.
- `mnote.buf.line_count() -> n`: How many lines it has.
- `mnote.buf.cursor() -> line, column`: Where the cursor is (0-based).
- `mnote.buf.set_cursor(line, column)`: Moves the cursor.
- `mnote.buf.insert(text)`: Inserts text at the cursor.
- `mnote.note() -> path | nil`: The open note's path in the vault.
- `mnote.mode() -> mode`: Vim's mode: `NORMAL`, `INSERT`, `VISUAL`…
- `mnote.decorate(namespace, opts) -> id`: Draws over the note, moving with edits: `{ line, col, end_col, fg, bg }` colours text; `{ line, line_bg }` a line; `{ line, text, color, right }` adds text after it; `{ line, sign, color }` a gutter sign; `{ line, col, ghost, color }` draws text inside the line that isn't part of it (the cursor skips it). `bias = "before"` or `"after"`: which side of text typed exactly at an end the decoration stays on (ghost text: after; the rest: before). Colours are theme token names (`match`, `danger`, `ink_3`).
- `mnote.undecorate(namespace)`: Removes every decoration in the namespace.
- `mnote.open(path, { line })`: Opens a note of the vault (`journal/today`, `.md` optional), at `line` (0-based) if given, once the function returns. A note that doesn't exist is reported.
- `mnote.apply_edits({ { note, first, last, lines }, … }, { label })`: Replaces lines `first..last` (as `mnote.buf.set_lines` counts them) in several notes as one undo step, once the function returns. Notes that aren't open are opened in the background, edited and saved. `u` in any of them undoes the whole edit in all of them that haven't changed since. Edits to one note mustn't overlap.
- `mnote.feedkeys(keys)`: Runs keys in Vim notation (`ggdd`, `<C-w>v`, `<leader>f`) as if typed, through the keymap, mappings and Vim, once the function returns.
- `mnote.cmd(line)`: Runs an Ex line (`set so=5`, `w`, `daily`; a leading `:` is fine) once the function returns.
- `mnote.clipboard.get() -> text | nil`: The system clipboard's text.
- `mnote.clipboard.set(text)`: Puts text on the system clipboard.

## Events

Each handler gets a table with `event` and `note`, and:

- `NoteOpened`: the focused pane shows another note (and at start); `note`
- `NoteSavePre`: a note is about to be written; runs before it, may edit; `note`
- `NoteSaved`: a note was written; `note`
- `TextChanged`: the note's text changed; `note`, `revision`, `origin` (who made the latest change: `user`, `plugin:<name>`, `lsp:<server>`, `agent:<session>`, `prediction`, `external`)
- `ModeChanged`: Vim's mode changed; `from`, `to`
- `InsertLeave`: INSERT or REPLACE ended
- `CursorMoved`: the cursor moved; `line`, `column`
- `VaultChanged`: notes changed on disk outside mNote
- `ConfigApplied`: config.toml was applied
- `PaneFocused`: another pane took focus

## Examples

`examples/plugins/` holds `tasks` (signs, a count, a picker and a ```progress block), `wordgoal` (a daily word goal from its settings) and `pins` (harpoon-style pinned notes, with `mnote.open` and the clipboard). Copy a folder into `~/.config/mnote/plugins/` and turn it on.
