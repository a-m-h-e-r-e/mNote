# 0008. Finding servers: config, mNote's tools, Mason, then `PATH`

Status: Accepted (2026-11-25)

## Context

Language servers are separate programs. Neovim users usually install them with Mason, which puts them in `~/.local/share/nvim/mason/bin/`, and configure them through nvim-lspconfig: command, file types, root markers, settings. Zed and Helix find servers on `PATH` or download their own.

## Decision

- **A server's command is looked up in this order:**
  1. `cmd` in config.toml's `[lsp.<server>]`,
  2. mNote's tools folder (`~/.local/share/mnote/tools/bin/`),
  3. Mason's `bin` folder,
  4. `PATH`.
- **Server defaults ship as data in `servers.toml`**, ported from nvim-lspconfig: command, file types, root markers, initialization options and settings. config.toml can override any field, and its `settings` are sent through `workspace/configuration`.
- **Installing comes later (phase 4).** `:LspInstall` would read Mason's package registry, pick the asset for this platform, check its checksum, and install into mNote's tools folder. Nothing installs system-wide, and nothing downloads without a command.

## Alternatives

- **Only `PATH`.** Simplest, but Mason's servers aren't on `PATH` unless the user's Neovim setup adds them.
- **Running Mason itself.** Mason is a Neovim plugin, so it needs Neovim to run.

## Consequences

- Neovim users get their installed servers with no setup.
- `servers.toml` needs updating when lspconfig's defaults change. It's data, so that's a small change without new code.
