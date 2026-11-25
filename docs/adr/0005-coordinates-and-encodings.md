# 0005. Coordinate types and a negotiated LSP position encoding

Status: Accepted (2026-11-25)

## Context

Positions mean different things to different parts of the editor:
- **Vim** counts characters.
- **Tree-sitter** counts UTF-8 bytes, plus row and column-in-bytes points.
- **LSP** counts lines and UTF-16 code units by default. Since LSP 3.17 the client and server can agree on UTF-8 or UTF-32 instead.

Mixing them up is a common source of bugs that only show on non-ASCII text.

## Decision

- **Distinct types**, with conversions as methods on the rope and no implicit conversion between them:
  - `CharOffset`, `ByteOffset`, `Point { row, column_bytes }`;
  - `LspPosition { line, character }`, tied to a `PositionEncoding` (`Utf8`, `Utf16`, `Utf32`).
- **Choosing the encoding:** the client offers `["utf-8", "utf-16"]` in `initialize`. It uses UTF-8 when the server accepts it and UTF-16 otherwise, the spec's default.
- **One change log feeds everything.** Every edit is recorded once (range, old bytes, new text, revision). The syntax map's `InputEdit`s, LSP's incremental `didChange` and the rebasing of late results all come from it.
- **Anchors gain a bias**: whether a position stays before or after text inserted exactly at it. Marks become the one way to hold a position across edits.

## Alternatives

- **Plain `usize` everywhere, with careful naming.** That's how the code stands today, and it's exactly where encoding bugs come from.
- **Always UTF-16.** Every conversion would cost a walk over the text, even with servers that accept UTF-8.

## Consequences

- Existing code keeps `usize` character offsets for now; the types are adopted where code support touches positions, and spread from there.
- Conversions get property tests with multi-byte and astral-plane text.
