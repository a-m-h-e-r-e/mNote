# 0010. Edits carry their origin; edit groups and proposals

Status: Accepted (2026-11-25)

## Context

Today an edit is a range and a text, and undo steps are per buffer. More edits will come from outside the user's keystrokes:
- language servers (renames and code actions across files),
- plugins (they already edit),
- later, agents and edit predictions.

Established editors need three things here, and Zed's agent features rely on all of them:
- **knowing who made an edit**, to undo an agent's turn, to let predictions ignore their own edits, and to tell plugins;
- **one undo step that spans files**;
- **edits shown as a diff for review before they're applied**, hunk by hunk.

Adding these after code support ships would mean changing the buffer, undo, events and every caller.

## Decision

- **Every change records an `Origin`**: `User`, `Plugin(name)`, `Lsp(server)`, `Agent(session)`, `Prediction` or `External` (the file changed on disk). It's recorded in the change log and carried by `TextChanged`.
- **Edit groups**: an undo step is a group with an origin and an optional label. A group may span several buffers, and undo from any of them undoes all of it. Files that aren't open are opened as parked documents, edited, and saved atomically.
- **Proposals**: a group can be held instead of applied.
  - Its edits are kept as anchored hunks and drawn as a diff.
  - Each hunk, or the whole proposal, can be accepted or rejected.
  - User edits under a hunk rebase it through the change log, or mark it stale.
  - The mechanism doesn't care where a proposal comes from.
- **Ghost text**: marks gain an inline ghost-text kind, text drawn at a position but not in the buffer. Inlay hints use it first.
- **The first users are not AI.** LSP workspace edits are applied as groups with origin `Lsp`, and held as proposals when `review_edits` is on. Plugins can propose with `mnote.propose`. Both arrive before any agent does.

## Alternatives

- **Add these with the AI work.** That touches the buffer, undo and every caller after the fact, and AI would be the first and only user, so nothing would have proven the mechanism first.
- **Proposals as a separate scratch buffer with a diff view.** Simpler to draw, but hunks can't be rebased as the user keeps editing the real file, and accepting would mean merging after the fact.

## Consequences

- `Buffer::edit` takes an origin (callers in `mnote-vim` pass `User`; plugins pass theirs).
- Undo history stores groups. A group touching several buffers is undone from whichever one the user is in.
- The proposal review UI is built in phase 2, for LSP workspace edits.
