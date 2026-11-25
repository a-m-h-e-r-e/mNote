# 0012. Boundaries for AI and agents, fixed now

Status: Accepted (2026-11-25). The AI features get their own design; this record fixes what they may and may not change.

## Context

AI features like Zed's are planned but not designed yet: an agent panel, external agents, MCP, edit predictions and model providers. The aim is to add them later without rewriting code support's foundations. That means deciding now:
- where they live;
- what they build on;
- what network, secret and permission rules they follow.

## Decision

- **Where they live.** AI lives in one new crate, **`mnote-ai`**, and in UI in `mnote-app`. It depends on the crates below it; nothing below depends on it.
- **What it builds on, unchanged:**
  - edit groups, origins and proposals for every edit an agent makes ([ADR-0010](0010-edit-origins-and-proposals.md));
  - the context API for what a model is shown ([ADR-0011](0011-context-api.md));
  - `mnote-rpc` for ACP agents and MCP servers ([ADR-0009](0009-shared-rpc-transport.md));
  - the completion engine's inline suggestions for edit predictions ([ADR-0007](0007-completion-engine.md));
  - the permission store for agent tools.
- **Agents never write files directly.** An agent's edits arrive as proposals, or as edit groups with origin `Agent` when the user allows auto-applying. Either way they're undoable per turn and go through the vault's atomic writes and Trash.
- **One permission store.** It's today's plugin trust generalised, and holds what the user allowed for:
  - plugins,
  - grammar libraries,
  - language-server commands,
  - agent tools,
  - context servers.

  Each entry has a fingerprint and a scope (once, this session, always), and one prompt picker says exactly what will run or be read. It's built in phase 1a, with the grammar loader as its first new user.
- **One network stack.** mNote gets one HTTP client, a synchronous one with TLS running on background threads, such as `ureq` with `rustls`. It's chosen under the dependency rules when the tools installer needs it (phase 4), and reused by providers. No async runtime, so not `reqwest`, which brings tokio.
- **Secrets** (API keys) live in the macOS Keychain, never in config.toml or plugin settings.
- **Privacy defaults.** Nothing leaves the machine unless the user turns a provider on. It's off by default and opt-in per vault, and local models are a first-class provider.

## Alternatives

- **Design AI now as well.** Too early: providers, protocols and models are moving fast, while the foundations they need are stable and already well understood.
- **Decide nothing until AI starts.** That's how foundations end up rewritten: each of the items above changes code that code support writes first.

## Consequences

- Code support's phases build the permission store, origins, proposals, ghost text, the context API and `mnote-rpc`, each with a non-AI first user.
- The AI design can only add crates, UI and settings. A change below `mnote-ai` would need a new ADR that supersedes this one.
