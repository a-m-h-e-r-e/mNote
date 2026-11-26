# 0015. No title bar: tab rows reach the window's top edge

Status: Accepted (2026-11-26). Amends [0014](0014-tabs-on-every-pane.md), which kept the top bar.

## Context

[ADR-0014](0014-tabs-on-every-pane.md) gives every pane a tab row and keeps the window's top bar above them: the traffic lights (close, minimize, zoom), the focused note's path and a hint. With tab rows, that top bar is a second row of chrome over the first, and it says little: the active tab already names the note.

Keyboard-first editors drop it. Zed and VS Code put their tabs in the title bar's row. Terminal-like apps such as Ghostty and kitty can hide the title bar altogether. mNote's principle is that the note owns the window, and the top bar is the last piece of chrome that doesn't do anything for the note.

## Decision

- **No top bar.** The panes reach the window's top edge, and their tab rows are the first row of the window.
- **No window buttons.** The close, minimize and zoom buttons are hidden. The window is still a normal titled, resizable macOS window (rounded corners, shadow, Mission Control, full screen), with its title bar transparent and its three standard buttons hidden. The hiding is done in `summon.rs`, the one module already allowed `unsafe` for native window calls (CLAUDE.md).
- **Keys do what the buttons did,** as registry commands (in the palette, rebindable):
  - `⌘Q` (`quit-app`) quits;
  - `⌘M` (`window-minimize`) minimizes;
  - `⌃⌘F` (`window-fullscreen`) toggles full screen;
  - `⌘W` closes a tab, as ADR-0014 decided.
  mNote has no menu bar today. These commands are what the buttons and a Window menu would have given.
- **The window moves by its empty top space:**
  - the empty part of any tab row reaching the top edge;
  - with no rows shown (one note, `show_tabs = "auto"`), a 32px band along the top of the note: the padding the note already has above its first line.
  - A double click on a row's empty part opens an untitled tab. A double click on the band zooms, as a title bar does.
- **What the top bar showed moves to the status line:**
  - the focused note's path, as the file item;
  - the unsaved dot, on the file item;
  - hints such as `gt next tab` or `⌃w h j k l move`, at its right end, before the counts.
- **Full screen** shows the same layout with no gap where the buttons were.
- **`window_buttons = false | true`** in `config.toml` (default `false`) brings the buttons back. With `true`, the first pane's row leaves room for them, the way the top bar did (`TRAFFIC_LIGHT_INSET`).

## Alternatives considered

- **Keep the top bar** (ADR-0014 as written). It's a second row above the tab rows, mostly repeating the active tab.
- **Tabs in the title bar's row, beside the buttons,** as Zed and VS Code do. This keeps the buttons, but only the first pane's row can share the title bar row, so the layout differs by pane. A side-by-side split's right pane starts its row at the edge while the left one starts after the buttons, and the rows stop lining up. Available with `window_buttons = true`.
- **A borderless window** (`titlebar: None`). No native calls are needed, but it loses the rounded corners, the shadow, resizing from edges and native full screen on macOS.

## Consequences

- `TopBar` goes. `ui::chrome` keeps `StatusLine`, which takes the path, the unsaved dot and the hints. The status line's file item shows the note's path (`notes/projects/roadmap.md`), cut from the left when it's long.
- The keymap's chords gain `⌃⌘` with a letter (`<C-D-f>`) for full screen.
- `summon.rs` gains a small `hide_window_buttons(window)` (three `standardWindowButton:` lookups and `setHidden:`), called when the main window opens. The window keeps `appears_transparent: true`. If AppKit returns no button, nothing happens.
- Pane tab rows (AMI-220) handle window moves on their empty space, and the single-note band does too. `TRAFFIC_LIGHT_INSET` is used only with `window_buttons = true`.
- The window is 32px taller for the note: one more line on screen.
- The pane-tabs design, AMI-220 and the UI screens change to match.
