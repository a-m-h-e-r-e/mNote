# 0006. An LSP client on `mnote-rpc`, with serde_json and its own protocol types

Status: Accepted (2026-11-25)

## Context

LSP is JSON-RPC over a server's stdin and stdout. CLAUDE.md rules out async runtimes, and discourages `serde_json`-style heavy formats where a simple parser would do. LSP requires JSON, and `serde_json` and `serde` are already locked through gpui, so using them adds no new crate versions. The `lsp-types` crate covers the whole protocol, but it changes often and brings URI crates with it. Helix and Zed both keep forks of it.

## Decision

- **On `mnote-rpc`, with threads rather than an async runtime.** Process handling, `Content-Length` framing, request ids, timeouts and cancellation come from `mnote-rpc` ([ADR-0009](0009-shared-rpc-transport.md)). This crate adds LSP's messages, the server manager, document sync and the features. Replies reach the app through `async_channel`, which mNote already uses for jobs, and the app applies them on the UI thread.
- **`serde_json` and `serde` are allowed in `mnote-lsp`.** This is an exception to the dependency rule, with the reason recorded in CLAUDE.md: the protocol requires JSON, and both crates are already locked through gpui at the versions we'd use.
- **mNote's own protocol types.** A `protocol` module holds the subset of LSP 3.17 that mNote implements, serde-derived, each type citing its spec section. Unknown fields are ignored.
- **Built on capabilities.** mNote advertises only what it implements, and uses a feature only where the server's capabilities allow it.
- **On top of `mnote-rpc`'s request manager**, this crate keeps the revision each request was asked at, cancels with `$/cancelRequest`, and drops or rebases stale replies.
- **Workspace edits** (rename, code actions, `workspace/applyEdit`) are applied as one edit group with origin `Lsp`, and held as a proposal when `review_edits` is on ([ADR-0010](0010-edit-origins-and-proposals.md)).
- **One module per feature**, behind one interface: whether it's supported, how to build its request, what to do with the reply.
- **Server lifecycle**:
  - servers start lazily, one per (server, project root);
  - `shutdown` and `exit` are sent on quit;
  - a crashed server restarts with backoff, at most three times in five minutes.

## Alternatives

- **The `lsp-types` crate.** It covers everything, but changes often, carries URI dependencies, and other editors fork it anyway.
- **A hand-written JSON parser.** Nothing to add, but a second JSON implementation to keep correct for no gain, since `serde_json` is already in the build.
- **An async runtime.** CLAUDE.md rules it out, and a few threads per server are enough.

## Consequences

- `mnote-lsp` can be tested without any real server: `mnote-rpc`'s scripted fake peer plays sessions through the same transport.
- Every new LSP feature means adding its types to `protocol` and a module of its own.
