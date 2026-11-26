# Tabs on every pane: design

Status: **accepted** (2026-11-26), with [ADR-0015](../adr/0015-no-title-bar.md) (accepted): no top bar, so the tab rows reach the window's top edge. The decision is [ADR-0014](../adr/0014-tabs-on-every-pane.md), which supersedes [ADR-0013](../adr/0013-tab-pages.md) and [the tab pages design](tab-pages.md).

Every pane in mNote has its own row of tabs, as every editor group in VS Code does. A pane holds several open notes, shows one of them, and remembers where you were in each. Notes are shared: one note in two panes is one buffer with one undo history. Splits keep working as they do. Each pane of a split has its own tab row, the lower one of an above/below split included.

The screens are drawn in the [UI design](https://claude.ai/artifact/Cv6oLXFUddX1y3yMMuPrsT), on its "Tabs on every pane" page.

The keys stay Vim's, with VSCodeVim's meaning: `gt`, `gT` and `:tabnew` act on the focused pane's tabs. The Mac's `⌘T`, `⌘W` and `⌘1`…`⌘9` sit alongside.

## Goals

- **Several notes per pane:** each one a tab, kept open with its cursor, scroll and view, and one click or key away.
- **Each pane's tab row:**
  - it scrolls sideways when tabs don't fit;
  - it has VS Code's close slots (`×`, and `●` for unsaved text);
  - it ends in a `…` menu for closing in bulk and for splitting.
- **Every pane gets a row**, top or bottom, left or right, however the splits nest.
- **Preview tabs:** looking through notes doesn't pile up tabs.
- **The same keys as AMI-219,** now on the focused pane's tabs: `gt`, `{N}gt`, `gT`, `g⇥`, `⌃PageDown`/`⌃PageUp`, `⌘T`, `⌘1`…`⌘9`, `⌘⇧]`/`⌘⇧[`, `⌃⇥`, `⌘W`, `⌘⇧W`, `⌘⇧T`.
- **Untitled notes** (`:tabnew`, `⌘T`) and the save prompt, as built in AMI-218.
- **Restoring:** tabs reopen per vault, as they were.
- **Budgets:** tabs that aren't shown cost nothing per keystroke, and switching tabs takes one frame.

## Non-goals

- **Window-wide tab pages** (ADR-0013). They're dropped and may come back as saved layouts.
- **Pinned tabs, locked panes, and dragging a tab onto a pane's edge to split.** These can come later without changing the model.
- **Several windows,** and tearing a tab off into one.
- **Tabs that aren't notes or files** (settings pages, previews of other kinds).

## The model

```
Editor
 ├─ buffer, view, cursor, scroll …   the focused pane's active tab
 ├─ layout: Layout                   the panes, as today
 ├─ panes: Vec<PaneSlot>
 │    └─ PaneSlot { id, tabs: Vec<Tab>, active, used: Vec<usize> }
 │         └─ Tab { doc: DocKey, preview: bool, local: Option<PaneLocal> }
 ├─ parked: Vec<Doc>                 every other open note, shared
 └─ closed: Vec<ClosedTab>           for :tabreopen (pane, place, doc, state)
```

- **A `Tab` is a note in a pane:** which doc it shows, and that tab's own cursor, scroll and view (`PaneLocal`) while it isn't the one on screen. The focused pane's active tab keeps its state in the editor's fields, as the focused pane does today.
- **Docs are open while any tab shows them,** in any pane, or while an edit group needs them. That's today's rule for panes, applied to tabs.
- **Two tabs on one note,** in two panes, share the doc. A pane never holds the same note twice: opening a note it already has activates that tab.
- **Each pane keeps its own last-used order** (`used`): closing a tab shows the one used before it. This is the `TabSet` logic from AMI-218, moved from the window to the pane.
- **Switching tabs in the focused pane:**
  1. The editor's state goes into the old tab's `PaneLocal`.
  2. The old doc is parked.
  3. The new tab's doc is unparked (opened from disk if it isn't open), and its `PaneLocal` is swapped in.
  This is the same swap focusing a pane does now.
- **Tabs of hidden notes measure their lines again when shown,** if the note changed meanwhile. That's the revision check from AMI-218.

## Opening notes

| From | Opens as |
| --- | --- |
| The finder, `:e`, `:tabedit`, a new note, quick capture | A kept tab in the focused pane, or that pane's tab for the note if it has one |
| The explorer, search results, backlinks, `gd` on a link | The pane's **preview tab** (shown in italics), replacing the last preview |
| `:tabnew`, `⌘T`, a double click on a tab row's empty part | An **untitled note** in a new kept tab |
| `⌃v` / `⌃x` in a picker, `:vsplit` / `:split` | A new pane beside or below, its first tab |

- **A new tab goes right after the active one,** as in VS Code.
- **A preview tab becomes a kept tab** when you edit it, double-click it, or open it again from a place that keeps tabs. `:keep` (or `space k`) keeps it by key.
- **`preview_tabs = false`** in `config.toml` makes every open a kept tab.

## Closing

- **The tab:** `:q`, `:tabclose`, `⌘W`, the tab's `×`, or a middle click close the active (or that) tab.
  - The pane then shows its last-used tab.
  - Closing its last tab closes the pane, and the pane before it takes the focus.
  - On the last tab of the last pane, `:q` quits mNote, as today. `⌘W` never quits: it says *⌘Q quits mNote*.
- **Notes autosave,** so closing doesn't ask. Untitled or scratch text asks first:
  - `:q` says *E37* unless `!` is given;
  - `⌘W`, `×` and the `…` menu ask *Save "untitled" before closing?* with Save as…, Discard and Cancel.
- **`⌘⇧W` closes every tab of the focused pane,** and so the pane.
- **`:tabonly` closes the pane's other tabs.**
- **`⌘⇧T` or `:tabreopen` brings back the last closed tab** (up to 10) where it was, with its cursor. If its pane is gone, it opens in the focused pane.
- **`⌘W` with an overlay open** (a picker, the command line) closes the overlay first, as built in AMI-219.

## Keys and commands

All of these are registry commands: in the palette, rebindable in `[keys]`, runnable by plugins.

| Command | Ex | Keys | On the focused pane |
| --- | --- | --- | --- |
| `tab-new` | `:tabnew [note]`, `:tabedit {note}` | `⌘T` | New tab after the active one: the note, or an untitled note |
| `tab-next` | `:tabnext [N\|#]` | `gt`, `{N}gt`, `⌃⇥`, `⌃PageDown`, `⌘⇧]` | Next tab (wrapping), tab N, or the last used (`#`, `g⇥`) |
| `tab-previous` | `:tabprevious [N]` | `gT`, `{N}gT`, `⌃⇧⇥`, `⌃PageUp`, `⌘⇧[` | Previous tab, or N back |
| `tab-1` … `tab-8`, `tab-last` | `:tabfirst`, `:tablast` | `⌘1` … `⌘8`, `⌘9` | Tab N; `⌘9` the last |
| `close-something` | | `⌘W` | The overlay, else the active tab (and the pane when empty); never quits |
| `tab-close` | `:tabclose[!] [N]` | | The active tab, or tab N |
| `pane-close-tabs` | | `⌘⇧W` | Every tab of the pane, and the pane |
| `tab-only` | `:tabonly[!]` | | The pane's other tabs |
| `tab-reopen` | `:tabreopen` | `⌘⇧T` | The last closed tab |
| `tab-move` | `:tabmove [N\|+N\|-N]` | | Moves the active tab in its row, as Vim places tabs |
| `tab-keep` | `:keep` | `space k` | A preview tab becomes a kept one |
| `tab-list` | `:tabs` | | Every pane and its tabs; `>` the active one, `+` unsaved |
| `tab-to-pane` | | `⌃w T` | The active tab leaves for a new pane beside this one |
| `pane-menu` | | `⌃w .` | Opens the pane's `…` menu |

- **`⌃w T`** changes meaning: it moves the active tab into a new pane on the right, since there are no tab pages to move it to.
- **The Vim keys and chords are as built in AMI-219.** Only what they act on changes.

## The tab row

```
┌──────────────────────────────────────────────┬───────────────────────────────────┐
│ standup.md × │ ideas.md │ ‹i›today.md │   … │ main.rs ● │ syntax.rs │        … │
│                                              │                                   │
│ # Standup                                    │ fn main() {                       │
│                                              ├───────────────────────────────────┤
│                                              │ Cargo.toml × │ README.md │     … │
│                                              │ [package]                         │
└──────────────────────────────────────────────┴───────────────────────────────────┘
```

- **There is no top bar** ([ADR-0015](../adr/0015-no-title-bar.md)). The window's close, minimize and zoom buttons are hidden, and the panes reach the window's top edge. The tab rows along the top are the window's first row, and their empty part moves the window.
  - `⌘Q`, `⌘M`, `⌃⌘F` and `⌘W` do what the buttons did.
  - The status line takes what the top bar showed: the note's path with its unsaved dot, and the hints.
  - With no rows shown (one note), a 32px band along the top of the note moves the window.
  - `window_buttons = true` brings the buttons back, with room left for them in the first pane's row.
- **Every pane has a row along its top,** `bar_h` tall and on `tab` ground. That includes a pane below another (the right column above). It's part of the pane, so splits, `⌃w` moves and resizing all keep it with its pane.
- **When rows show** (`show_tabs = "auto" | "always" | "never"`):
  - Under `auto`, the default, every pane shows its row when any pane has two or more tabs or the window has two or more panes. Rows then line up across splits.
  - A single note in a single pane looks exactly as it does today, with no row.
- **Each tab shows:**
  - the note's file name (`ideas.md`), cut with `…` after 24 characters;
  - the folder after it in `ink_3` when two of the pane's tabs share a name;
  - italics for the preview tab;
  - the close slot at its right end, 2px from the tab's edge: a 16px `×` in a 24px slot on the active tab and under the pointer (on a soft square on hover), `●` for unsaved text, which turns into `×` under the pointer, and empty otherwise.
- **The active tab** is on `tab_active` (paper) with no line below it, so it opens into its note: the row's bottom hairline runs under the other tabs and the empty part only.
  - In the **focused** pane it carries the `cursor` rule along its top. That is the focus mark: panes no longer draw a rule of their own.
  - In other panes it carries an `ink_3` rule.
- **Overflow:**
  - The row scrolls sideways, as VS Code's does, with a trackpad or the wheel (vertical wheel turns into sideways).
  - A thin scrollbar shows while the pointer is over the row.
  - The active tab is kept in view when it changes.
  - Tabs never shrink below their names.
- **The `…` button** sits at the row's right end, outside the scrolling part, always visible. It opens a small menu under it:
  - Close all, Close others, Close saved, Close to the right;
  - Reopen closed tab;
  - Split right, Split down, Move tab to new pane;
  - Keep preview tab (only while there is one).
  Each item names its key where it has one. The keyboard reaches it with `⌃w .`, `j`/`k`, `⏎` and `esc`.
- **Mouse:**
  - A click shows a tab; a double click keeps a preview tab.
  - A middle click or the `×` closes a tab.
  - Dragging moves a tab within its row, or onto another pane's row to move it to that pane. If the note is already open there, that tab is activated and the dragged one closes.
  - A double click on a row's empty part opens an untitled tab.
- **Theme keys** (renamed from AMI-220's): `tabs` (row ground, default `chrome`), `tab_active` (default `paper`), `tab_text` (other tabs' names, default `ink_2`), and `tab_preview` (the preview tab's name, default `ink_3`, drawn in italics).

## Untitled notes and saving

These are unchanged from AMI-218:
- `:tabnew` and `⌘T` open `untitled` in a new tab, kept as a draft.
- The first `:w` opens the save prompt, prefilled from the first heading. `:w {path}` and `:saveas` name it directly.
- E13 for an existing file, E139 for a note open elsewhere, E37 on `:q` with text.

Once named, the tab shows the new name, in every pane that has it.

## Restoring

- **The session file** is per vault in mNote's config folder, as in the tab pages design (phase 6 there). It holds:
  - the pane layout;
  - each pane's tabs in order, with each tab's note, cursor, view and preview flag;
  - each pane's active tab, and the focused pane.
- **Tabs that aren't active stay dormant:** their notes are read when first shown. Cold start reads only the visible tabs' notes.
- **Untitled notes come back from their drafts.** Missing notes are skipped.

## Plugins

- **Events:**
  - `TabOpened { pane, note, preview }`
  - `TabClosed { pane, note }`
  - `TabActivated { pane, note }`
  - `TabMoved { pane, from, to }`
  - `PaneFocused` and `NoteOpened` keep their meaning.
- **Lua:**
  - `mnote.tabs(pane?)` lists `{ note, active, preview, unsaved }` per tab;
  - `mnote.tab.open(note, { pane, preview })`, `mnote.tab.close(pane, index, { force })`, `mnote.tab.activate(pane, index)`;
  - `mnote.open(path, { target = "tab" | "preview" | "split" | "vsplit" })`.

## What happens to what's built

| Built | Becomes |
| --- | --- |
| AMI-218 `TabSet` (order, last used, `:tabmove`, reopen) | Each pane's tab list |
| AMI-218 `TabPage`, switching layouts | Removed |
| AMI-218 untitled notes, drafts, save prompt, `:saveas`, `:qa` | Kept as is |
| AMI-218 `:q` cascade | Tab → pane → app |
| AMI-219 Vim keys and chords | Kept; they act on the focused pane |
| AMI-219 `⌘W` and its close question | Kept; closes a tab rather than a pane |
| AMI-219 `⌃w T` | Moves the active tab to a new pane |
| AMI-220 tab line (uncommitted) | Each pane's tab row: close slots, scrolling, drag, theme keys |
| `tools/check-tabs.sh` | Rewritten for pane tabs, same `MNOTE_SCRIPT` hook |

## Budgets and cost

- **Keystroke (8 ms):** unchanged. Only active tabs are drawn, and a tab row is a few small elements per pane.
- **Switching tabs:** one swap, plus measuring the new note's visible rows when it changed while hidden. It stays within a frame, and is benchmarked like tab switching was.
- **Memory:** a hidden tab is a doc key and a `PaneLocal`. Its doc is open while any tab shows it, and dormant tabs (after a restart) hold nothing until shown.
- **Binary:** no new crates.

## Testing

- **The pane's tab list as plain data:**
  - open after the active tab;
  - activate an existing tab for the same note;
  - preview replacement, and keeping a preview;
  - close showing the last used;
  - `:tabmove`;
  - reopen, including when the pane is gone.
- **`tools/check-tabs.sh` end to end:**
  - two panes each with their tabs, and the lower pane of an above/below split with its own row;
  - one note in two panes shares one buffer and one undo history;
  - the `:q` and `⌘W` cascades;
  - the keys;
  - untitled notes and the save prompt;
  - preview tabs from the explorer.
- **A screenshot check of the rows,** as done for AMI-220: side by side, above and below, overflowing, and the `…` menu.

## Phases

1. **Model.** (AMI-225) Tabs in `PaneSlot`, switching, the close cascade, and reopen. `TabPage` is removed and the AMI-218 commands move onto panes. Untitled notes and the save prompt are kept. The status line shows `2/5` for the active tab until rows land.
2. **Tab rows.** (AMI-220) Every pane's row, the lower ones included, from AMI-220's tab line: close slots, scrolling with wheel and scrollbar, `show_tabs`, theme keys, and the focus rule moving onto the active tab.
3. **Mouse.** (AMI-226) Click, double click, middle click, `×`, drag within a row and across panes, and double-clicking empty space.
4. **The `…` menu** (AMI-227) and `⌃w .`, with its close, reopen, split and move items.
5. **Preview tabs:** (AMI-221) the opening rules in the table above, `preview_tabs`, and `:keep`.
6. **Keys.** (AMI-228) AMI-219's keys moved onto the pane; `⌃w T` gets its new meaning, and `⌘⇧W` closes the pane's tabs.
7. **Restoring:** (AMI-223) the session file, dormant tabs, and drafts brought back.
8. **Plugins:** (AMI-222) tab events and the Lua API, with docs.
9. **Bench and polish:** (AMI-224) the tab-switch bench and user docs.

## Open questions

- **Should `:q` close the tab (VSCodeVim) or the pane (Vim, where windows are panes)?** This design closes the tab, so closing many tabs doesn't take many panes with it. `:close` still closes the whole pane.
- **Should `show_tabs = "auto"` show a row on a single pane with one tab?** This design keeps today's bare look there. A user who wants VS Code's always-on rows sets `"always"`.
