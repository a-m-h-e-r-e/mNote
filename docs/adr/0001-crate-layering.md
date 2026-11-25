# 0001. Crate layering: `mnote-lang`, `mnote-rpc`, `mnote-lsp`, and room for `mnote-ai`

Status: Accepted (2026-11-25)

## Context

Language knowledge is spread over `mnote-core` today: `code.rs` (grammars and highlight queries for code fences), `filetype.rs`, and `text/syntax.rs` (the Markdown background parser). Code support adds much more of it: language packs, grammar loading, a syntax map with injections, highlights, indent, text objects, folds, completion and snippets. An LSP client is a separate concern again: processes, JSON-RPC and protocol state.

CLAUDE.md requires that `mnote-core` and `mnote-vim` don't depend on gpui, and that logic stays testable outside the app.

## Decision

- A new crate, **`mnote-lang`**, holds everything about languages that needs only a rope and tree-sitter. `mnote-core` depends on it: the `Buffer` holds a `SyntaxMap`.
- A new crate, **`mnote-rpc`**, holds the JSON-RPC transport for processes: framing, request ids, timeouts and cancellation ([ADR-0009](0009-shared-rpc-transport.md)). It has no gpui and knows no protocol.
- A new crate, **`mnote-lsp`**, holds the LSP client on top of `mnote-rpc`. It depends on `mnote-core` (snapshots, changes, coordinates) and `mnote-lang` (which servers a language has). It runs its own threads and has no gpui.
- A later crate, **`mnote-ai`**, will hold AI and agents on the same crates, and nothing below it depends on it ([ADR-0012](0012-ai-integration-boundaries.md)).
- `mnote-core` also holds two shared services: the permission store and the context API ([ADR-0011](0011-context-api.md)).
- `mnote-app` connects them to the UI. `mnote-vim` gets language behaviour only through its extension hooks.

## Alternatives

- **Everything in `mnote-core`.** Fewer crates, but `core` would mix the vault, the index and the language layer, and every change to languages would rebuild and retest all of it.
- **One `mnote-code` crate for languages and LSP.** LSP brings threads, processes and JSON that the syntax layer, which notes need too, shouldn't carry.
- **The transport inside `mnote-lsp`.** ACP and MCP would later need it pried out ([ADR-0009](0009-shared-rpc-transport.md)).

## Consequences

- `code.rs`, `filetype.rs` and `text/syntax.rs` move into `mnote-lang` (with re-exports in `mnote-core` while callers move).
- `mnote-lang`, `mnote-rpc` and `mnote-lsp` follow the same rules as `core`: no UI crates, typed errors, tests next to the code.
