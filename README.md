# mNote

A keyboard-first Markdown notes app built with Rust and [gpui](https://gpui.rs). Notes are plain `.md` files in a folder, Vim works everywhere, and a global key summons it from any app.

## Workspace

| Crate | What it holds |
| --- | --- |
| `crates/mnote-core` | Vault, note index, links. No UI dependencies. |
| `crates/mnote-vim` | Vim modes, motions, operators and the ex command parser. Pure logic. |
| `crates/mnote-app` | The gpui desktop app (binary `mnote`). |

## Develop

Requires macOS with Xcode (gpui renders with Metal) and stable Rust.

```bash
cargo run -p mnote-app
```

```bash
cargo test --workspace
```

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

## Docs

- [Tabs and panes](docs/tabs.md): opening notes, preview tabs, keys, the mouse and settings.
- [Editing code](docs/code.md): indent, comments, text objects, auto-pairs, folds, the outline and context.
- [Plugins](docs/plugins.md): the Lua API and events.

## Design

- Design system: https://claude.ai/artifact/VAXKWj371tAeXScQgoDh5S
- Screens: https://claude.ai/artifact/UMKfpfGMnNg7gE2WXjr32P
