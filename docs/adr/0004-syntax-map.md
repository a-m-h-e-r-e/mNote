# 0004. One syntax map with injection layers, for notes and code

Status: Accepted (2026-11-25)

## Context

Notes are parsed with tree-sitter's Markdown grammar on a background thread (`text/syntax.rs`), and code fences are highlighted one block at a time, each parsed from scratch (`code.rs`). Code files bring more nested languages: doc comments, templates, embedded SQL or CSS. Zed (its syntax map) and Helix (its syntax layers) keep every nested language as its own tree over the ranges it covers.

## Decision

- Each document has one **syntax map**: a root layer and one layer per injection, each with its language, its byte ranges (`Parser::set_included_ranges`) and its tree. `injections.scm` decides the layers.
- **On a keystroke**, edits are applied to every tree with `Tree::edit`, which is cheap. A worker reparses a snapshot of the root and of the changed layers. Results carry their revision, and a newer edit makes the worker parse again, as the Markdown worker does today. Each parse has a time limit.
- **Highlighting** runs queries over the visible rows only, across the layers there:
  - a deeper layer wins over the one it sits in, and `priority` metadata breaks ties;
  - spans are cached per line per revision;
  - capture names map to theme tokens with fallback by prefix.
- **Semantic tokens** from a language server are one more layer on top.
- **Markdown code fences** become injection layers; per-block highlighting goes away.
- **Large files**: above a set size injections are skipped, and above a second size highlighting is off.

## Alternatives

- **Keep per-block highlighting for notes and add a separate code highlighter.** Two pipelines to keep fast and correct, and fences would stay non-incremental.
- **Highlight whole documents.** Simpler caching, but the work would grow with the file, not the screen.

## Consequences

- The Markdown index keeps reading the root layer's tree. Nothing changes for it.
- Rebasing results by revision and the change log ([ADR-0005](0005-coordinates-and-encodings.md)) become shared infrastructure.
