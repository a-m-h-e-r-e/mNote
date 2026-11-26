# Tabs and panes

Every pane in mNote has its own row of tabs, as in VS Code and Zed. A tab is a note (or a code file) open in that pane; the active tab is the one the pane shows. The same note can have a tab in several panes: they share its text and its undo, and each keeps its own cursor and scroll.

Keys act on the **focused pane**, the one with the cyan line at the top of its active tab.

## Opening notes

Where a note opens depends on how you got to it:

| From | Opens as |
| --- | --- |
| The finder, `:e`, `:tabedit`, a new note, quick capture | A tab in the focused pane, or that pane's tab for the note if it has one |
| The explorer, search results, backlinks, `gd` on a link | The pane's **preview tab** |
| `:tabnew`, `⌘T`, a double click on a row's empty part | An **untitled note** in a new tab |
| `⌃v` / `⌃x` in a picker, `:vsplit` / `:split` | A new pane beside or below |

### Preview tabs

Notes you only look at open in one **preview tab**, whose name is in *italics*. The next note you look at replaces it, so browsing doesn't leave a trail of tabs. It becomes a kept tab when you:

- edit it;
- press `space k` (`:keep`);
- double-click it.

Set `preview_tabs = false` in config.toml to give every note its own tab.

### Untitled notes

`:tabnew` and `⌘T` open an untitled note. Saving it (`:w`) asks for a name; `:saveas name` names it directly. Its text survives a restart until you save or discard it.

## Keys

| Keys | Ex command | What it does |
| --- | --- | --- |
| `⌘T` | `:tabnew [note]`, `:tabedit {note}` | A new tab after the active one: the note, or an untitled note |
| `gt`, `⌃⇥`, `⌃PageDown`, `⌘⇧]` | `:tabnext` | The next tab, round the row |
| `gT`, `⌃⇧⇥`, `⌃PageUp`, `⌘⇧[` | `:tabprevious` | The previous tab |
| `{N}gt`, `⌘1` … `⌘8` | `:tabnext N` | Tab N |
| `⌘9` | `:tablast` | The last tab (`:tabfirst` the first) |
| `g⇥` | `:tabnext #` | The tab used before this one |
| `⌘W` | | Closes an open overlay, else the active tab (and the pane with its last tab). It never quits mNote: `⌘Q` does |
| | `:tabclose[!] [N]` | Closes the active tab, or tab N |
| `⌘⇧W` | | Closes all the pane's tabs, and the pane |
| | `:tabonly[!]` | Closes the pane's other tabs |
| `⌘⇧T` | `:tabreopen` | Brings back the tab closed last, where it was |
| | `:tabmove [N\|+N\|-N]` | Moves the active tab along its row |
| `space k` | `:keep` | Keeps the preview tab |
| `⌃w T` | | Moves the active tab to a new pane on the right |
| `⌃w .` | | Opens the pane's `…` menu |
| | `:tabs` | Lists every pane and its tabs: `>` the active one, `+` unsaved text |

Closing a tab shows the tab you used before it. Unsaved untitled or scratch text asks first; add `!` to close it anyway.

Every one of these is also in the command palette (`⌘K`), and the keys can be changed in config.toml's `[keys]` table (`"<D-t>" = "tab-new"`).

## Mouse

- **Click** a tab to show it.
- **Double-click** a preview tab to keep it.
- **Middle-click** a tab, or click its `×`, to close it.
- **Drag** a tab to reorder its row, or onto another pane's row to move it there. If that pane already has a tab for the note, it's shown instead.
- **Double-click** a row's empty part for a new untitled tab.
- **Scroll** a row that has more tabs than fit with the wheel.

## The `…` menu

The `…` at the end of each pane's row (or `⌃w .`) has:

- Close all, Close others, Close saved, Close to the right;
- Reopen closed tab, Keep preview tab;
- Split right, Split down, Move tab to new pane.

## Restoring

With `restore_session = true` (the default), mNote reopens the vault's panes and tabs as you left them, with their cursors and unsaved untitled notes. Tabs that weren't shown load when you first show them, so a long row doesn't slow the start.

## Settings

| Setting | Values | Default |
| --- | --- | --- |
| `show_tabs` | `"auto"` (when a pane has two or more tabs, or there are two or more panes), `"always"`, `"never"` | `"auto"` |
| `preview_tabs` | `true`, `false` | `true` |
| `restore_session` | `true`, `false` | `true` |
| `window_buttons` | `true` shows the window's close, minimize and zoom buttons; without them the tab rows reach the top edge | `false` |

The theme keys for the rows are `tabs` (the row's ground), `tab_active`, `tab_text` and `tab_preview`.

## Plugins

Plugins can follow tabs (`tab_opened`, `tab_closed`, `tab_activated`, `tab_moved`) and drive them (`mnote.tab.open`, `mnote.tab.close`, `mnote.tab.activate`, `mnote.tabs`). See [plugins.md](plugins.md).
