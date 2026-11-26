# Tab pages: design

Status: **superseded** by [tabs on every pane](pane-tabs.md) and [ADR-0014](../adr/0014-tabs-on-every-pane.md) (2026-11-26). It was accepted on 2026-11-26 and built as AMI-218 and AMI-219; it's kept for the record. The decision is recorded in [ADR-0013](../adr/0013-tab-pages.md).

mNote gets tab pages the way Vim has them: **a tab page is a layout of panes**, not a file. Each tab keeps its own splits, its own focused pane and each pane's cursor and scroll; notes are shared by every tab, so one note open in two tabs is one buffer with one undo history. The keys and commands are Vim's (`gt`, `gT`, `:tabnew`, `⌃w T`), with the Mac's (`⌘T`, `⌘1`…`⌘9`) alongside.

## Goals

- Several workspaces in one window: a tab for a project's notes split side by side, another for today's daily note, another for a code file.
- Vim's model and Vim's keys, so a Neovim user's habits work unchanged: `gt`, `{N}gt`, `gT`, `:tabnew`, `:tabedit`, `:tabclose`, `:tabonly`, `:tabmove`, `:tabs`, `⌃w T`, `⌃PageDown` and `⌃PageUp`.
- macOS conventions alongside: `⌘T` new tab, `⌘⇧]` and `⌘⇧[` next and previous, `⌘1`…`⌘9` a tab by number (`⌘9` the last), `⌃⇥` and `⌃⇧⇥`.
- A tab line that appears only when there are two or more tabs, as Vim's `showtabline=1` does, so a one-tab window looks exactly like today's.
- Pickers and the explorer can open a note in a new tab (`⌃t`, as Telescope does), beside today's `⌃v` and `⌃x` splits.
- Tabs reopen as they were when mNote starts again on the same vault.
- Plugins see tabs: events when one opens, closes or takes focus, and Lua functions to list, open and focus them.
- Within the budgets: switching tabs takes one frame, and tabs that aren't shown cost nothing per keystroke.

## Non-goals

- **A tab per file** (VS Code or Zed's tabs on each pane). In Vim, and here, a pane shows one note and opening another replaces it. A list of open notes is already the buffer picker. Per-pane file tabs are a different model, and it can't be layered on later without changing what a pane is. This design leaves room for neither to block the other, but builds only Vim's.
- Several windows (`⌘N`). One window with tabs comes first.
- Moving a tab between windows, or tearing a tab off.
- `:tab {cmd}` (running any command so it opens in a new tab). The commands that open notes take a "new tab" target instead (see [Opening into a tab](#opening-into-a-tab)), which covers its real uses.
- Tab-local working directories (`:tcd`). mNote has one vault per window.

## How it fits what's there

Today ([`editor/panes.rs`](../../crates/mnote-app/src/editor/panes.rs)):

- The **editor's own fields hold the focused pane**: its buffer, view, cursor and scroll.
- Every other pane waits as a `PaneSlot` with its `PaneLocal` (scroll, view, cursor), and every other open note waits as a parked `Doc`.
- Focusing a pane swaps them in and out.
- Two panes on one note share one `Doc`, like Vim's windows.
- The window's division is one `Layout` tree, with `panes`, `focused` and `next_pane` beside it.

Tabs use the same pattern one level up. **The editor's fields hold the current tab's `layout`, `panes` and `focused`**. Every other tab waits as a `TabPage` holding those three things, with every one of its panes as a `PaneSlot` that has its `PaneLocal`, its focused pane's included. Switching tabs swaps them.

```
Editor
 ├─ buffer, view, cursor, scroll …   the focused pane (unchanged)
 ├─ layout, panes, focused           the current tab (unchanged fields)
 ├─ parked: Vec<Doc>                 every other open note, shared by all tabs
 └─ tabs: TabSet
      ├─ current: TabId, its name
      ├─ others: Vec<TabPage>        in order, the current one's place kept
      │    └─ TabPage { id, name, layout, panes (all with PaneLocal), focused }
      └─ next_tab, previous: Option<TabId>   (for `g⇥`, `:tabnext #`)
```

Pane ids stay unique across all tabs (`next_pane` is shared), so a `PaneId` names one pane in the whole window and events about panes stay unambiguous.

### Notes are shared, panes aren't

- A `Doc` stays open while **any pane in any tab** shows it, or an edit group needs it. Today that check reads `self.panes`; it reads every tab's panes instead (`shown_anywhere`).
- Editing a note updates every pane on it in the current tab, as today (`splice_other_panes`). Panes in other tabs are measured again when their tab is shown. Each tab keeps the buffer revision its panes were laid out at, and on switching, a pane whose note changed since then re-measures its lines, keeping its cursor in view. This is the same work `reset_panes_on` does after a reload, done once per switch rather than on every keystroke.
- Undo, autosave, external changes, the file watcher, renames (relinking) and deletions already act on docs, not panes, so they work across tabs unchanged. A renamed or deleted note's panes in other tabs follow it the way panes in the current tab do today: `parked_notes` and the rename handling walk all tabs' slots.

### Switching tabs

`switch_tab(target)`:

1. Save the focused pane's state into a `PaneLocal`, exactly as `focus_pane` does before moving to another pane.
2. Move `layout`, `panes`, `focused` and that local into a `TabPage`, and put it in `others` at the current tab's place.
3. Take the target `TabPage` out, and move its three fields into the editor's.
4. Make its focused pane the editor's: unpark its doc, swap its `PaneLocal` in. This reuses `focus_pane`'s second half, factored out as `enter_pane`.
5. Re-measure the panes whose notes changed since the tab was last shown. Then send `TabFocused`, then `PaneFocused` and `NoteOpened` if the note differs, and redraw.

None of this touches buffers, trees or highlighting caches, which stay with their `Doc`. Switching is a few swaps plus re-measuring the changed panes' visible rows: well within a frame.

### Closing

- **`:q`, `⌃w c`, `:close`**: close the focused pane, as today. When it's the last pane of its tab and there are other tabs, the tab closes and the **last-used tab** is shown (the one you were in before), as browsers and most editors do; `tabclose = "left"` gives Vim's choice, the tab to the left. When it's the last pane of the last tab, mNote quits, as today.
- **`:tabclose`** (`:tabc`) closes the current tab and all its panes. **`:tabclose N`** closes tab N. The last tab can't be closed: *E784: Cannot close last tab page*.
- **`:tabonly`** (`:tabo`) closes every other tab.
- **`:qa`, `:wqa`, `:xa`** close every tab and quit. These are added if they're missing: today `:q` with one pane quits.
- Notes autosave, so closing never asks. The exceptions are a scratch note and an [untitled note](#untitled-notes) with text: `:q` and `:tabclose` say *E37: No write since last change (add ! to override)*, as Vim does, and `⌘W` asks instead (see [`⌘W`](#w-close-without-losing-a-layout)).
- **Closed tabs can be reopened.** The last 10 closed tabs are kept (layout, notes, cursors and views; not the text of untitled notes, which is asked about first). `⌘⇧T` or `:tabreopen` brings the most recent back where it was. So closing a tab by mistake loses nothing.

### Untitled notes

`:tabnew` with no note does what Neovim does: a new tab with an **empty, unnamed buffer**. In mNote that's an *untitled note*, which is not the scratch note:

- It's a `Source::Untitled(n)`, shown as `untitled` (`untitled 2`, … when there are several) in the tab line and status line, with the dirty dot once it has text.
- Autosave doesn't write it (there's no path), but its text is kept as a draft in mNote's config folder (`drafts/`) a second after each change. If mNote quits or crashes, the draft comes back in its tab with the session (phase 6).
- It's Markdown until named; `:setf` changes that, as for any buffer.
- An untitled note with no text closes without a word; it isn't restored.

**Naming it, on save.** `:w` (or `⌘S`, or `:wq`, `:x`, `ZZ`) on an untitled note opens the **save prompt** rather than failing with *E32: No file name*:

```
 Save as  meetings/2026-11-26 standup.md                       ⏎ save · ⇥ complete · esc cancel
          meetings/   ideas/   projects/mnote/
```

- It's the command line's look, at the bottom, with the path editable in Vim insert keys.
- **Prefilled with a suggestion**: the note's first heading, or else its first line, cut at 60 characters with characters that can't be in a file name dropped (`# Standup notes` gives `standup notes.md`). It goes in the folder of the note you came from, or the vault's root, or the folder of today's daily note when it started from `⌘T` on a daily note. With no text yet, the suggestion is `untitled.md`, selected so typing replaces it.
- `⇥` completes folders and existing names (`ArgKind::Note`'s completion); a folder that doesn't exist yet is shown as `+ new folder` and made on save.
- No extension means `.md`. Any other extension (`.rs`, `.py`) saves a code file and sets its language from the name, as opening one does.
- An existing name asks before replacing it: *E13: File exists (add ! to override)*. Typing `!` at the end, or `⌘⏎`, replaces.
- `esc` cancels and the note stays untitled.
- `:w {path}` and `:saveas {path}` name it straight away with no prompt, as in Vim. They take the same completion and extension rules.
- Once named, it's an ordinary note: it autosaves, the index and backlinks pick it up, and the draft is removed.

`:tabnew {note}` and `:tabedit {note}` open that note in the new tab; a name that doesn't exist yet makes a new note of that name on save, as `:edit` does.

### `⌘W`: close without losing a layout

`⌘W` closes **one thing at a time, and never quits**:

1. With a picker, the palette, a prompt or another overlay open, it closes that.
2. Otherwise it closes the focused pane (as `⌃w c`); the last pane of a tab closes the tab, and the last-used tab is shown.
3. On the last pane of the last tab it does nothing but say *⌘Q quits mNote*. The window never closes by accident.
4. An untitled note with text asks first: *Save "untitled" before closing?*, with **Save as…** (the save prompt), **Discard** (`⌘⌫`) and **Cancel** (`esc`).

`⌘⇧W` closes the whole tab, all its panes, in one go (as `:tabclose`). Both can be undone with `⌘⇧T`, which reopens the tab that was closed, so the layout is never lost. This is what browsers and Zed do, and is safer than leaving `⌘W` unbound, where a Mac user's habit does nothing.
- A closed tab's docs go if no other tab shows them, the same rule `close_pane` already applies.

## Commands and keys

All of these are entries in the command registry ([`editor/commands.rs`](../../crates/mnote-app/src/editor/commands.rs)), so the palette lists them, `keys.toml` can rebind them and plugins can run them.

| Command | Ex | Keys | Does |
| --- | --- | --- | --- |
| `tab-new` | `:tabnew [note]`, `:tabedit {note}` (`:tabe`) | `⌘T` | A new tab after this one: on the note named, or an [untitled note](#untitled-notes), as Neovim does. |
| `save-as` | `:w [path]` on an untitled note, `:saveas {path}` (`:sav`) | `⌘S` on an untitled note, `⌘⇧S` | The save prompt, or saves at the path given. |
| `close-something` | | `⌘W` | The overlay, else the pane, else the tab; never quits ([`⌘W`](#w-close-without-losing-a-layout)). |
| `tab-reopen` | `:tabreopen` | `⌘⇧T` | Reopens the most recently closed tab. |
| `tab-next` | `:tabnext [N]` (`:tabn`) | `gt`, `⌃PageDown`, `⌘⇧]`, `⌃⇥` | The next tab, wrapping. `{N}gt` and `:tabnext N` go to tab N. |
| `tab-previous` | `:tabprevious [N]` (`:tabp`, `:tabNext`) | `gT`, `⌃PageUp`, `⌘⇧[`, `⌃⇧⇥` | The previous tab, wrapping; `{N}gT` goes back N tabs. |
| `tab-first`, `tab-last` | `:tabfirst` (`:tabrewind`), `:tablast` | `⌘1`, `⌘9` | |
| `tab-1` … `tab-8` | | `⌘1` … `⌘8` | Tab N, if there is one. |
| `tab-recent` | `:tabnext #` | `g⇥` | The tab used before this one (Neovim's `g<Tab>`). |
| `tab-close` | `:tabclose[!] [N]` (`:tabc`) | `⌘⇧W` | |
| `tab-only` | `:tabonly[!]` (`:tabo`) | | |
| `tab-move` | `:tabmove [N\|+N\|-N]` (`:tabm`) | | Moves the tab to after tab N (`0` makes it first; no N makes it last), or N places along. |
| `tab-list` | `:tabs` | | Lists the tabs and their panes' notes in the message area, `>` at the focused pane, as Vim does. |
| `tab-rename` | `:tabrename [name]` | | Names the tab (no name: back to the note's name). Not Vim's; Neovim users get it from plugins. |
| `pane-to-tab` | | `⌃w T` | Moves the focused pane to a new tab of its own (if it's the only pane, nothing to do). |

- `gt` and `gT` live in [`mnote-vim`](../../crates/mnote-vim/src/state.rs), as `g` commands with a count, and come out as app commands (`tabnext` with the count as the argument) through the same `AppCommand` path `:setfiletype` uses. That way `mnote-vim` stays free of UI, and counts, `.` and mappings behave as for any Vim key. `⌃PageDown` and `⌃PageUp` do the same in every mode, as in Vim.
- The Mac keys are ordinary `<D-…>` bindings in the default keymap, so a user can drop them. `⌘W`'s steps are one command (`close-something`), so a user who prefers `⌘W` to close the tab outright binds it to `tab-close` instead.

### Opening into a tab

The places that open notes take a target: *here*, *split*, *vertical split*, or **new tab**.

- **Finder, buffer picker, backlinks, search results, explorer**: `⌃t` opens the chosen note in a new tab, beside `⌃v` and `⌃x` (picker `ActInSplit` becomes `ActIn(Target)`).
- **Following a link**: `gf` and `Enter` on a link as today. `⌃w gf` opens the link in a new tab (Vim's).
- **`:tabedit`, `:tabnew`**: take the same note arguments and completion as `:edit` and `:split` (`ArgKind::Note`), code files included.
- **Plugins**: `mnote.open(path, { target = "tab" })`.

A note already shown somewhere doesn't jump to that tab: Vim opens it again in the new tab, sharing the doc, and so does mNote. `:tab drop`'s behaviour (go to where it's shown) is left for a later option.

## The tab line

- Shown when there are **two or more tabs** (`show_tabline = "auto"`, the default), or always or never (`"always"`, `"never"`), in `config.toml`.
- It's a row in the top bar, beside the macOS traffic lights (`TRAFFIC_LIGHT_INSET`), so a second bar doesn't take vertical room. The path segments of today's top bar move to the status line's file item when the tab line is shown. When it isn't, the top bar is unchanged.
- Each tab shows:
  - its number (for `{N}gt` and `⌘N`);
  - its name: the one given with `:tabrename`, or else the focused pane's file name (`ideas.md`, `main.rs`);
  - `+2` when it has more panes than one;
  - a close slot at its right end, as VS Code has: a plain `×` (no box) on the current tab and on the tab under the pointer, which gets a soft square behind it on hover. A tab with unsaved text shows `●` in the slot instead, which turns into `×` under the pointer. Other tabs leave the slot empty, so names don't shift.
- The current tab uses the theme's accent, as the focused pane's border does. Long names are cut with `…`. When tabs don't fit, the line scrolls sideways, as VS Code's does (trackpad, or the wheel), and keeps the current tab in view; tabs never shrink to nothing.
- Mouse: click shows a tab; middle-click or the `×` closes it (as `⌘W` would, asking first for untitled text); dragging a tab moves it (`:tabmove`); a double click on the empty part opens a new tab.
- Themes get `tabline`, `tabline_current` and `tabline_text` keys, defaulting to the top bar's colours, so existing themes look right without changes.
- The top bar's hint (`⌃w h j k l move · ⌃w c close`) gains `gt next tab` while there are several tabs.

## Restoring tabs

mNote reopens the vault as it was: every tab in order, each tab's layout and the notes, cursors and views of its panes, and which tab and pane had focus.

- Kept per vault in mNote's config folder, as `sessions/{vault-hash}.toml`, beside `permissions`. It isn't kept in the vault, so notes folders synced or shared with others don't get mNote's state in them. It's TOML, written with the `toml` crate mNote already uses (no `serde_json`, per CLAUDE.md), and written atomically like `config.toml`.
- Saved when tabs or panes change, debounced to once a second, and on quit. It isn't saved on cursor moves: those are taken at the next save or on quit.
- On start, a note that no longer exists is skipped: its pane closes, and a tab left with no panes goes. A bad or unreadable file is ignored, and mNote opens as it does today.
- `restore_session = true` in `config.toml` (default on). Opening mNote with a note path (`mnote notes/x.md`) restores the session and shows that note in a new tab, or in the current pane when the session restored is a single tab with an empty untitled or scratch note.
- Scratch notes aren't restored; their panes come back as the vault's start note. Untitled notes with text come back from their drafts, still untitled.
- Only the current tab's notes are read at start; other tabs' notes are opened when their tab is first shown. Their places are kept as rows and columns, which are checked against the file when it opens. This keeps cold start within its 200 ms budget however many tabs there were.

The last point means a `TabPage` can hold panes whose `Doc` isn't loaded yet, a **dormant tab**. Its `PaneSlot`s name their note, and `switch_tab` opens the docs before entering. It's the same as opening a note in a pane today, done for each pane of the tab.

## Plugins

Events, in [docs/plugins.md](../plugins.md)'s format:

- `TabOpened { tab, index }`
- `TabClosed { tab }`
- `TabFocused { tab, index, previous }`
- `TabMoved { tab, from, to }`

`PaneFocused` and `NoteOpened` keep their meaning: they fire on a tab switch when the focused pane or note changes.

Lua:

- `mnote.tabs()`: the tabs in order, each `{ id, index, name, current, panes = { { id, note, focused } } }`.
- `mnote.tab.open(note?, opts?)`, `mnote.tab.focus(id_or_index)`, `mnote.tab.close(id_or_index, { force })`, `mnote.tab.rename(id, name)`.
- `mnote.open(path, { target = "tab" | "split" | "vsplit" | "here" })`.
- `mnote.current_tab()`.

Tab ids are stable for the tab's life (unlike indexes, which change with `:tabmove`), as pane ids are.

## Ready for later

- **AI and agents** ([ADR-0012](../adr/0012-ai-integration-boundaries.md)): an agent's work can be shown in a tab of its own (its proposals' notes split side by side) by opening a tab with a name, through the same `TabSet` API. The context API ([ADR-0011](../adr/0011-context-api.md)) gains "the notes in this tab" as a source, since a tab is a set of related notes the user grouped. Neither needs changes here.
- **File tabs per pane**: if wanted later, a pane would get a list of notes, and the tab line here would stay the tab-page line, as in Neovim with a bufferline plugin. Nothing in this design assumes a pane shows exactly one note forever, beyond what panes already assume.
- **Several windows**: a window would own a `TabSet`; docs would move up to the app. `TabSet` holds no window state, so it can move.

## Budgets and cost

- **Keystroke (8 ms)**: unchanged. Only the current tab is drawn. `refresh_parked_trees` today refreshes every parked doc each frame; it refreshes only those shown in the current tab, which is cheaper than today when tabs hide notes.
- **Switching (one frame)**: the swaps above, plus re-measuring visible rows of panes whose note changed. Benchmarked with 9 tabs of 4 panes on 1 MB notes, from `tab-next` to the frame drawn: within 16 ms.
- **Cold start (200 ms)**: dormant tabs read nothing until shown, so the cost of restoring is reading one small TOML file plus today's start.
- **Memory**: a dormant tab is its layout and a few paths. An open doc costs what it does today, wherever it's shown.
- **Binary (25 MB)**: no new crates.

## Testing

- `TabSet` is plain data, like `Layout`, and is tested on its own:
  - open, close, move, next and previous with wrapping and counts;
  - `#` (last used);
  - closing the last pane of a tab;
  - E784 on the last tab;
  - pane ids unique across tabs.
- Editor tests (gpui test context):
  - a note in two tabs shares one buffer and one undo;
  - an edit in one tab shows in the other's pane on switching, with the cursor kept in view;
  - closing a tab keeps docs still shown elsewhere and drops the others;
  - an external change, rename and delete reach panes in other tabs;
  - `:q` cascades from pane to tab to quitting.
- `mnote-vim`: `gt`, `3gt`, `2gT`, `⌃PageDown` produce the right app commands in normal, visual and insert mode.
- Session file:
  - round-trips;
  - a missing note, a bad file and a future version are each handled without failing to start;
  - dormant tabs open their docs only on first switch.
- The large-note bench gains a tab-switching run (9 tabs × 4 panes, 1 MB notes) reporting p95 switch time.

## Phases

Each is an issue, shippable on its own.

1. **Tab model.** `TabSet` and `TabPage`, `switch_tab`, and docs kept while any tab shows them. Commands: `:tabnew`, `:tabedit`, `:tabclose`, `:tabonly`, `:tabnext`, `:tabprevious`, `:tabfirst`, `:tablast`, `:tabs`. `:q` cascades; closing shows the last-used tab; closed tabs can be reopened (`:tabreopen`). Untitled notes, their drafts and the save prompt (`:w`, `:saveas`). No tab line yet, so tabs are listed with `:tabs` and the status line shows `tab 2/3`.
2. **Keys.** `gt`, `gT`, `{N}gt`, `g⇥` in `mnote-vim`; `⌃PageDown` and `⌃PageUp`; the Mac keys; `⌃w T`; `:tabmove`.
3. **Tab line.** Drawing with the theme keys, `show_tabline`, the mouse (click, close, drag) and overflow scrolling.
4. **Opening into tabs.** Picker and explorer `⌃t`, `⌃w gf`, the open target in the API.
5. **Plugins.** Tab events and the Lua `mnote.tab` API, with docs.
6. **Restoring.** The session file, dormant tabs, `restore_session`, and the start-with-a-path rule. A cold-start bench with 9 restored tabs.
7. **Bench and polish.** The tab-switch bench run, `:tabrename`, and the docs for the user (keys and commands added to the help and the palette).

## Decided

- **`:tabnew` with no note** opens an untitled note, as Neovim's empty buffer, named by the save prompt on the first save ([Untitled notes](#untitled-notes)).
- **After closing a tab** the last-used tab is shown; `tabclose = "left"` gives Vim's.
- **`⌘W`** closes the overlay, then the pane, then the tab, and never quits; `⌘⇧W` closes the tab; `⌘⇧T` reopens it ([`⌘W`](#w-close-without-losing-a-layout)).

The screens are drawn in the [UI design](https://claude.ai/artifact/Cv6oLXFUddX1y3yMMuPrsT): the tab line with split panes, the save prompt, the close confirmation, `:tabs` with an overflowing tab line, and the reopen toast.
