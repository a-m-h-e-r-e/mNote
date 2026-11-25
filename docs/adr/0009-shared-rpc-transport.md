# 0009. One JSON-RPC transport for LSP, ACP and MCP

Status: Accepted (2026-11-25)

## Context

Three protocols mNote will speak are JSON-RPC 2.0 with a process on the other end:
- **LSP**, for language servers, now;
- **ACP**, the Agent Client Protocol, for external agents such as Claude Code and Gemini CLI, later;
- **MCP**, the Model Context Protocol, for context servers, later.

They differ in framing and in their messages, not in mechanics. Each needs:
- spawning and stopping a process;
- a reader and a writer thread;
- request ids, timeouts and cancellation;
- notifications and requests in both directions;
- a log of the conversation.

If that lives inside the LSP client, ACP and MCP would mean copying it or prying it out later.

## Decision

- A crate, **`mnote-rpc`**, with no gpui, provides a **peer**: one child process (or later a socket) with a reader thread, a writer thread and stderr to a log.
- **Framing is a trait** with two implementations: `Content-Length` headers (LSP) and newline-delimited JSON (ACP and MCP over stdio).
- **A request manager** numbers requests and keeps timeouts. It cancels through a hook, since each protocol has its own cancel message (`$/cancelRequest` for LSP, `notifications/cancelled` for MCP). It routes replies to callbacks and hands incoming requests and notifications to a handler.
- **Messages are `serde_json` values at this level.** Each protocol crate turns them into its own typed messages.
- **A scripted fake peer** (a test binary) plays sessions in both framings. It's shared by every protocol's tests.
- `mnote-lsp` is the first protocol on top. `mnote-ai` would later add ACP and MCP on the same crate.

## Alternatives

- **A transport inside `mnote-lsp`.** Faster to start, but ACP and MCP would copy it or force it out later, which is the rewrite this design is meant to avoid.
- **A general RPC framework crate.** The ones available bring async runtimes, which CLAUDE.md rules out, or far more than three small protocols need.

## Consequences

- LSP's transport is written once, against a trait, with its tests in `mnote-rpc`.
- MCP over HTTP (its streamable HTTP transport) would be a third transport for the same peer when it's needed, using the HTTP client from [ADR-0012](0012-ai-integration-boundaries.md).
