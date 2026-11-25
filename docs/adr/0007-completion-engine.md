# 0007. A completion engine apart from its sources; LSP snippets

Status: Accepted (2026-11-25)

## Context

Completion candidates come from many places: language servers, words in open documents, file paths, snippets, and plugins. nvim-cmp and blink, VS Code and Zed all separate the engine (session, filtering, ranking, popup state) from its sources. LSP adds details of its own:
- trigger and commit characters;
- partial lists the server refines as you type;
- lazy `completionItem/resolve` for an item's docs and extra edits;
- insert-or-replace ranges;
- `additionalTextEdits`, the extra edits that add an import;
- snippets in a grammar of their own.

## Decision

- **A completion engine in `mnote-lang`**, with no UI.
  - It runs one session at a time. A session opens on a trigger character, after a few identifier characters, or by key.
  - It fuzzy-matches with `nucleo-matcher`, which mNote already uses.
  - It ranks by match score, then the server's `sortText` and `preselect`, then nearness and recent use.
  - It removes duplicates across sources, and asks again while a list is incomplete.
- **Sources implement one trait** and run in the background:
  - the LSP source lives in `mnote-lsp`;
  - words, paths and snippets live in `mnote-lang`;
  - plugins add their own through `mnote.complete.register`.
- **Accepting an item** applies its text edit, its extra edits and its commit characters, then expands it if it's a snippet. `resolve` is called lazily, after a short pause on an item.
- **Inline suggestions** are a second result kind: one suggestion drawn as ghost text at the cursor, accepted whole or a word at a time, with the same session rules. Their sources are LSP inline completions (still a proposed part of the spec) and, later, edit predictions ([ADR-0012](0012-ai-integration-boundaries.md)).
- **A snippet engine** follows the LSP snippet grammar: tabstops, placeholders, choices, `$0`, and variables. Tabstops are anchors, and copies of a tabstop are edited together. Snippet files are in VS Code's JSON format, read from mNote's folder and from friendly-snippets in Neovim's data.

## Alternatives

- **Completion inside the LSP client.** It would leave out the word, path and snippet sources, which also work without a server.
- **Only LSP snippets, no snippet files.** Neovim users' friendly-snippets would be lost.

## Consequences

- The popup in `mnote-app` only draws what the engine says, and sends keys back to it.
- Snippet sessions need anchors with a bias ([ADR-0005](0005-coordinates-and-encodings.md)), and inline suggestions need ghost-text marks ([ADR-0010](0010-edit-origins-and-proposals.md)).
