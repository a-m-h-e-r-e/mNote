# 0016. The workspace folder: opening folders as Neovim and VS Code do

Status: Accepted (2026-11-28).

## Context

mNote opens one folder as its **vault** and treats everything in it as notes:

- `mnote ~/notes` opens `inbox.md` there (created on first save);
- `mnote src/main.rs` makes `src/` the vault, so the finder, explorer and session only see `src/`;
- `mnote` alone opens the vault in `config.toml`;
- there's no `:cd` or `:pwd`, and no `mnote` command in the shell (only `mNote.app`).

That works for a notes folder and badly for a code project. `mnote .` in a repository writes an `inbox.md` into it on the first save, and opening a file roots everything at its folder rather than the project.

Editors give one folder this job, under different names:

- **Neovim's working directory.** It's the shell's folder when Neovim starts. Paths show relative to it, and `:e`, the pickers and root-based tools start there. `nvim src/main.rs` keeps the shell's folder, and `nvim .` browses the folder. `:cd`, `:pwd`, `:lcd` and `:tcd` change or show it. LazyVim also finds a project root (`.git`, `Cargo.toml`) for pickers and language servers.
- **VS Code's workspace folder.** `code .` opens a folder as the workspace. The explorer, search, Quick Open, terminals, workspace settings and language servers work inside it, and the layout is kept per folder. Another folder opens in a new window (`code -r` reuses the current one). "Install 'code' command in PATH" adds `code` to the shell.

## Decision

mNote has a **workspace folder**: the one folder everything works from, like Neovim's working directory and VS Code's workspace. It's separate from the **notes vault** in `config.toml`, where captured and daily notes go.

- **Opening from the shell:**
  - `mnote <folder>` (`mnote .`) makes it the workspace and shows the panes and tabs left there, or the explorer on it the first time. Nothing is created.
  - `mnote <file>` opens the file. The workspace is the shell's folder if the file is inside it (as in Neovim). Otherwise it's the nearest project root above the file (a language pack's `roots`, such as `.git`, `Cargo.toml` or `package.json`), or else the file's folder.
  - `mnote` alone, or from the Dock, opens the notes vault as the workspace, as it does now.
- **The workspace is the root of:**
  - the finder (`space f`), full-text search, tags and backlinks (the index);
  - the explorer, and `:e` with relative paths;
  - the breadcrumbs' path, the status line's path and the tab names;
  - the session (`sessions/{hash}.toml`, one per folder, as today);
  - and, in phase 2, where language servers start (ADR-0008's `roots`).
- **Notes keep to the notes vault.** Quick capture, the daily note and the summon key write to the notes vault even while a code folder is the workspace, so no `journal/` appears in a repository. With no vault set, they use the workspace.
- **`:cd {folder}` and `:pwd`**, as in Vim:
  - `:cd` alone goes back to the notes vault, and `:cd -` to the previous workspace.
  - Changing the workspace saves the old one's session, re-roots the index, and restores the new one's session. Unsaved text asks first, as closing does.
- **Recent folders:** `:recent` and a "Recent folders" palette entry list the last workspaces (kept in mNote's config folder), most recent first. Choosing one does `:cd` there.
- **One window.** mNote stays one window (ADR-0014's non-goal). A second `mnote <folder>` or `mnote <file>` while mNote runs hands its folder or file to the running window, which switches to it as `:cd` would. That's VS Code's `code -r`, and it's the default here.
- **The `mnote` command:**
  - The palette's "Install mnote command" puts a small `mnote` script on the `PATH` (`/usr/local/bin`, or `~/.local/bin` without permission), as VS Code's "Install 'code' command" does.
  - The script resolves its argument and the shell's folder to absolute paths. It opens `mnote://open?path=…&cwd=…` with `open`, which starts mNote or reaches the running one through gpui's `on_open_urls`. The app's `Info.plist` declares the `mnote` URL scheme.
  - Launching the binary directly keeps working as today.
- **Not now:** `:lcd` and `:tcd` (a workspace per pane or tab), several windows, and multi-root workspaces. A single workspace covers the usual case.

## Alternatives considered

- **Keep the vault as the only root.** `mnote .` keeps writing an `inbox.md` into code folders, and files keep rooting at their own folder.
- **A new window per folder,** as VS Code does by default. mNote is one window by design. Several windows would need the editor state and the summon panel per window, which is a much bigger change.
- **The shell's folder for bare `mnote`** (Neovim). mNote is a notes app first, and the Dock has no shell folder, so bare `mnote` opens the notes. `mnote .` asks for "here" in two characters.
- **A Unix socket for a second launch.** It needs a listener, a protocol and stale-socket handling. macOS already delivers URLs to the running app, and gpui exposes them.

## Consequences

- `Editor` holds a workspace root and a notes vault, not one vault. The index, explorer, finder and session read the workspace; capture and the daily note read the notes vault.
- `target_from_args` follows the rules above. A folder no longer implies `inbox.md`.
- `:cd`, `:pwd` and `:recent` are new ex commands. A recent-folders list lives beside `sessions/` in the config folder.
- The app bundle declares the `mnote` URL scheme. The installer writes one script, and that's the only thing outside the app.
- Phase 2's language servers get their root from the workspace, with the pack's `roots` refining it for a file outside it.
