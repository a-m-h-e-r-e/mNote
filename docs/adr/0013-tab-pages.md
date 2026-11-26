# 0013. Tab pages are layouts of panes, Vim's model

Status: Superseded by [0014](0014-tabs-on-every-pane.md) (2026-11-26); accepted 2026-11-26

## Context

mNote has split panes: a `Layout` tree of panes, with the focused pane's state in the editor's own fields and every other note parked as a shared `Doc`. Users want several workspaces in one window: a project's notes side by side in one, a daily note in another.

Editors do "tabs" in two different ways:
- **Tab pages** (Vim, Neovim): a tab holds a layout of windows; buffers are shared by every tab.
- **File tabs** (VS Code, Zed, Sublime): each pane holds a list of open files, shown as tabs on that pane.

They aren't the same feature, and an editor that builds one has to change what a pane is to get the other.

## Decision

- **A tab page is a layout of panes**: its own `Layout`, its panes with their cursor, scroll and view, and its focused pane. Docs (buffers) stay shared by every tab, as they are by every pane today. One note in two tabs is one buffer with one undo history.
- **The current tab lives in the editor's fields**, as the focused pane does. Other tabs wait as `TabPage`s and switching swaps them, reusing the pane swap (`focus_pane`'s second half, factored out). A `Doc` stays open while any pane in any tab shows it.
- **Pane ids are unique across tabs**, so a `PaneId` names one pane in the window.
- **Vim's commands and keys, with the Mac's alongside**:
  - `:tabnew`, `:tabedit`, `:tabclose`, `:tabonly`, `:tabnext`, `:tabprevious`, `:tabmove`, `:tabs`;
  - `gt`, `gT`, `{N}gt`, `⌃w T`;
  - `⌘T`, `⌘1`…`⌘9`, `⌘⇧]` and `⌘⇧[`.
  - `gt` and `gT` are `mnote-vim` keys that come out as app commands, so the Vim crate stays free of UI.
- **`:q` cascades**: it closes the pane, then the tab when that was its last pane, then the app when that was the last tab. Closing a tab shows the **last-used** tab (`tabclose = "left"` for Vim's choice), and closed tabs can be reopened (`⌘⇧T`, `:tabreopen`).
- **`:tabnew` with no note opens an untitled note**, as Neovim's empty buffer. It's kept as a draft until named. The first `:w` opens a save prompt prefilled with a name from its first heading; `:w {path}` and `:saveas` name it directly.
- **`⌘W` closes one thing and never quits**: the overlay, else the pane, else the tab. On the last pane of the last tab it only says that `⌘Q` quits. An untitled note with text asks first. `⌘⇧W` closes the tab.
- **The tab line shows only with two or more tabs** by default, inside the top bar.
- **Tabs are restored per vault** from a TOML file in mNote's config folder, not in the vault. Tabs other than the current one stay dormant (their notes unread) until first shown, to keep cold start in budget.

## Alternatives considered

- **File tabs per pane.** Familiar from VS Code, but it changes what a pane is (a list of notes and an active one). That would touch every place that assumes a pane shows one note: drawing, Vim's cursor, the jump list, events. It duplicates the buffer picker, and Vim users don't expect it. It's left as a possible later addition on top, as a bufferline plugin is in Neovim.
- **Tabs owning their docs** (no sharing). Simpler to swap, but one note in two tabs would be two buffers that diverge, or need syncing. It breaks the one-buffer rule panes already follow, and undo across tabs would surprise.
- **Several windows instead of tabs.** It needs multi-window state in gpui and moving docs up to the app, which is more work for the same result. A window can own a `TabSet` later.
- **Restoring every tab's notes at start.** Simpler, but cold start then grows with the number of tabs and notes.

## Consequences

- `Editor` gains a `TabSet`. The checks that ask "is this doc shown elsewhere", the file events, renames and the parked-tree refresh walk every tab's panes, not only the current tab's.
- Panes in tabs that aren't shown don't follow edits keystroke by keystroke. They re-measure when their tab is shown, so a hidden tab costs nothing per keystroke.
- A dormant `TabPage` can name notes whose docs aren't loaded, and `switch_tab` opens them.
- The picker's split action becomes an open target (here, split, vertical split, tab), and the same target goes into the Lua `mnote.open`.
- New plugin events: `TabOpened`, `TabClosed`, `TabFocused`, `TabMoved`. New config keys: `show_tabline`, `restore_session`, `tabclose` (default `"last-used"`). A new `Source::Untitled`, with drafts kept in mNote's config folder. New theme keys: `tabline`, `tabline_current`, `tabline_text`.
