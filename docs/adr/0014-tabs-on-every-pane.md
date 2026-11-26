# 0014. Tabs on every pane, as VS Code has them

Status: Accepted (2026-11-26). Supersedes [0013](0013-tab-pages.md). Amended by [0015](0015-no-title-bar.md): there is no top bar.

## Context

[ADR-0013](0013-tab-pages.md) made tabs Vim's tab pages: one tab line for the window, with each tab a whole layout of panes. AMI-218 and AMI-219 built that model, and AMI-220 built its tab line.

Using it showed that the model people expect is VS Code's:
- each pane has its own row of tabs, one per open file;
- the row scrolls sideways when the tabs don't fit;
- each row has a `…` menu (close all, close others…) and a `×` in each tab.

That's also how VSCodeVim and Neovim's bufferline plugins present tabs, and what Zed and Sublime do. ADR-0013 listed "file tabs per pane" as a non-goal because it changes what a pane is: a pane would show one of several notes, not exactly one. That change is now the point.

## Decision

- **A pane holds tabs.** Each pane has an ordered list of open notes (its tabs), one of them active, and remembers the cursor, scroll and view of each. Notes stay shared: one note in two panes, or in two tabs of the window, is one buffer with one undo history.
- **Every pane draws its own tab row** along its top edge. That includes the lower pane of an above/below split (`⌃w s`, `:split`) and every pane of a nested layout. The window's top bar stays as it is: the focused note's path, and nothing else.
- **Tab keys act on the focused pane's tabs**, as VSCodeVim maps them:
  - `gt` and `gT` move between them, `{N}gt` and `⌘1`…`⌘9` go to tab N;
  - `:tabnew`, `:tabedit`, `:tabclose`, `:tabonly`, `:tabmove`, `:tabs` and `:tabreopen` work on them;
  - `⌘T`, `⌘W`, `⌘⇧W` and `⌘⇧T` work on them.
- **Closing cascades**: `:q` or `⌘W` closes the active tab, then the pane when that was its last tab, then the app (`:q` only) when that was the last pane. `⌘W` never quits.
- **Preview tabs**, as in VS Code: a note opened to look at (the explorer, a search result, following a link) takes the pane's preview tab, shown in italics. The next such note replaces it, and editing it or double-clicking its tab keeps it.
- **A `…` menu ends each tab row**:
  - close all, close others, close saved, close to the right;
  - reopen closed;
  - split right, split down, move to a new pane.
- **Window-wide tab pages are dropped.** Nothing else uses them, and two rows of tabs work against "the note owns the window". Saved layouts can come later as their own feature.

## Alternatives considered

- **Keep ADR-0013's tab pages.** That's Vim's model and is already built, but it isn't what users reach for, and it gives no way to keep several notes in one pane.
- **Both: tab pages and tabs on every pane.** The most capable, but a window tab line above per-pane tab rows doubles the chrome. Every tab command would also need two meanings (`gt` for which?). Tab pages could return later as saved layouts without that clash.
- **Tab rows only on the top panes.** Less chrome in above/below splits, but a lower pane couldn't show or switch its own notes, and the model would no longer be one rule for every pane.
- **One tab per note across the window, panes only showing them.** Simpler state, but it isn't how any editor people know behaves when a note sits in two panes.

## Consequences

- `PaneSlot` gains its tabs: a list of `{ doc, preview, local }` and the active index. The editor's fields keep holding the focused pane's active tab, as they hold the focused pane today. Switching tabs swaps a doc and a `PaneLocal`, as focusing a pane does.
- **From AMI-218/219, what carries over, applied per pane:**
  - the order, last-used, `:tabmove` and reopening logic (`TabSet`);
  - untitled notes, drafts and the save prompt;
  - the `⌘W` cascade and its close question;
  - the keys and chords, and the `MNOTE_SCRIPT` checks.
- **What goes:** `TabPage`, switching whole layouts, and the window tab line. The AMI-220 tab row becomes each pane's row, with its close slots, scrolling, drag and theme keys.
- `show_tabline` becomes `show_tabs = "auto" | "always" | "never"`. Under `auto`, a pane shows its row when it has two or more tabs or the window has two or more panes.
- The theme keys `tabline`, `tabline_current` and `tabline_text` are renamed `tabs`, `tab_active` and `tab_text`. A new one, `tab_preview`, is added.
- The focused pane's cyan rule moves onto its active tab: the tab row carries the focus, and the pane needs no rule of its own.
- Plugin events and the Lua API name a pane with every tab: `TabOpened { pane, note }` and `mnote.tabs(pane)`.
- The design is [docs/architecture/pane-tabs.md](../architecture/pane-tabs.md). [docs/architecture/tab-pages.md](../architecture/tab-pages.md) and ADR-0013 are kept, marked superseded.
