# Architecture decision records

Each record is one decision: what was decided, why, what else was considered, and what follows from it. A record is never rewritten once accepted; a later one supersedes it.

| # | Decision | Status |
| --- | --- | --- |
| [0001](0001-crate-layering.md) | Crate layering: `mnote-lang`, `mnote-rpc`, `mnote-lsp`, and room for `mnote-ai` | Accepted |
| [0002](0002-native-grammars.md) | Native tree-sitter grammars, one isolated `unsafe` loader | Accepted |
| [0003](0003-language-packs.md) | Languages as data packs, compatible with nvim-treesitter | Accepted |
| [0004](0004-syntax-map.md) | One syntax map with injection layers, for notes and code | Accepted |
| [0005](0005-coordinates-and-encodings.md) | Coordinate types and a negotiated LSP position encoding | Accepted |
| [0006](0006-lsp-client.md) | An LSP client on `mnote-rpc`, with serde_json and its own protocol types | Accepted |
| [0007](0007-completion-engine.md) | A completion engine apart from its sources; LSP snippets | Accepted |
| [0008](0008-finding-servers.md) | Finding servers: config, mNote's tools, Mason, then `PATH` | Accepted |
| [0009](0009-shared-rpc-transport.md) | One JSON-RPC transport for LSP, ACP and MCP | Accepted |
| [0010](0010-edit-origins-and-proposals.md) | Edits carry their origin; edit groups and proposals | Accepted |
| [0011](0011-context-api.md) | One context API for completion, plugins and agents | Accepted |
| [0012](0012-ai-integration-boundaries.md) | Boundaries for AI and agents, fixed now | Accepted |
| [0013](0013-tab-pages.md) | Tab pages are layouts of panes, Vim's model | Superseded by 0014 |
| [0014](0014-tabs-on-every-pane.md) | Tabs on every pane, as VS Code has them | Accepted |

The designs these belong to: [code support](../architecture/code-support.md) (0001–0012), [tab pages](../architecture/tab-pages.md) (0013, superseded) and [tabs on every pane](../architecture/pane-tabs.md) (0014).
