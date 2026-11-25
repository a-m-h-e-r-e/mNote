# 0011. One context API for completion, plugins and agents

Status: Accepted (2026-11-25)

## Context

Several features need to know what's around the cursor, each in its own way:
- completion ranks by what's nearby;
- plugins ask for the current note or function;
- an agent or edit-prediction model is given context to work from: the enclosing function, the outline, diagnostics, recent edits, open files, symbols and their definitions, and in mNote's case linked notes and backlinks.

Zed, Cursor and Copilot each build one context service rather than letting every feature collect its own.

## Decision

- A **context API** in `mnote-core` with providers, each adding one kind of item:
  - enclosing syntax nodes and the outline (from the syntax map);
  - diagnostics near the position;
  - recent edits (from the change log, user edits first);
  - open documents and selections;
  - symbols with their definitions and references (through LSP);
  - backlinks and linked notes (from the index).
- **A request** names a document and position, the providers wanted, and a size limit (in characters, lines or an estimated token count). The API knows nothing about models.
- **The answer** is a list of items, each with its kind, document, anchored range, text and priority, trimmed to the limit by priority.
- **It runs in the background**, is versioned and can be cancelled, like any other request.
- **Plugins** add providers with `mnote.context.register` and read context with `mnote.context.get`.

## Alternatives

- **Each feature collects its own context.** Simple at first, but duplicated logic that drifts, and agents would need yet another copy.
- **Build it with the AI work.** Completion and plugins benefit now, and building it early means it's tested before agents rely on it.

## Consequences

- Phase 1b builds the syntax providers; phase 2 adds diagnostics and LSP providers.
- Privacy rules (what may leave the machine) belong to the AI layer that sends context, not to this API, which only reads.
