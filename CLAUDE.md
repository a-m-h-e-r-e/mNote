# mNote engineering rules

mNote must feel instant. Speed, a small dependency footprint and sound engineering are requirements, not polish. These rules apply to every change, by people and by agents.

## Performance budgets

Measured on an Apple Silicon Mac, release build. A change that breaks a budget is a bug.

| What | Budget |
| --- | --- |
| Keystroke to pixels on screen: mNote's work for a key (handling, render, layout, paint), so it shows on the next refresh | < 8 ms (one frame at 120 Hz) |
| Summon key to window visible (app already running) | < 50 ms |
| Cold start to an editable note | < 200 ms |
| Opening any picker (finder, explorer, palette) | < 16 ms to first frame |
| Fuzzy results update per keystroke, 10 000-note vault | < 5 ms |
| Indexing a 10 000-note vault on first open | < 1 s, off the UI thread |
| Opening a 1 MB note | < 50 ms |
| Memory at idle with a 10 000-note vault | < 150 MB |
| Release binary size | < 25 MB |

- Never block the UI thread on disk, network, indexing or search. Do that work on background tasks and send results back.
- Do work proportional to what changed: incremental re-parsing (tree-sitter), incremental index updates from the file watcher, and rendering only the visible lines.
- Avoid allocation in hot paths (keystroke handling, rendering, fuzzy matching). Reuse buffers. Use `SharedString` and `Arc<str>` for text that's shared.
- Hot paths get a benchmark before they get clever. Optimise from measurements, not guesses.

## Dependencies

Every dependency is a cost: build time, binary size, attack surface, maintenance.

- Prefer the standard library. Add a crate only when it does real work we shouldn't redo (gpui, ropey, tree-sitter, nucleo, notify, grep).
- Write down why in the PR or commit message for each new dependency: what it does, why std isn't enough, and its size.
- Use `default-features = false` and enable only the features we use.
- No async runtime (tokio, async-std). Use gpui's executor and std threads.
- No `serde_json`-style heavy formats where a simple parser will do. `toml` for config is fine. Accepted exception: `serde_json` and `serde` in `mnote-rpc`, `mnote-lsp` (and later `mnote-ai`), because LSP, ACP and MCP are JSON-RPC; both are already locked through gpui, so match those versions (ADR-0006).
- `mnote-core` and `mnote-vim` must not depend on gpui or any UI crate.
- Don't add duplicate crate versions. gpui's own tree already carries about 50. Check the `Cargo.lock` diff: a change may add new crates, but must not add a second version of a crate that's already locked, and when we pick a crate gpui also uses, match gpui's version. (`cargo tree -d` is a rough guide only; its count shifts with feature resolution even when the lockfile gains no new versions.) One accepted exception: mlua's `vendored` feature pulls in `luajit-src` as a build dependency, which brings `which` 8 beside gpui's `which` 6. It only runs at build time and nothing of it ships in the binary (AMI-204).
- Never run `cargo` with `--offline` to try out dependency changes: it re-resolves `Cargo.lock` against the local cache and silently downgrades unrelated crates. Pin new crates with `cargo update -p <crate> --precise <version>`.
- Pick crate versions released at least two weeks ago; brand-new releases wait.
- CI fails on unused dependencies (`cargo machete`).

## Code standards

- Stable Rust, edition 2024. `cargo fmt` clean. `cargo clippy --all-targets -- -D warnings` clean.
- `unsafe` is forbidden in the workspace. If gpui or the macOS integration ever needs it, isolate it in one module with a `// SAFETY:` comment per block, and lift the lint only there. The modules allowed it: `mnote-app/src/summon.rs` (the global hotkey) and `mnote-lang/src/grammar/native.rs` (loading tree-sitter grammar libraries, ADR-0002). Nowhere else.
- Code support and anything AI follow the accepted design in `docs/architecture/code-support.md` and the ADRs in `docs/adr/`. A change that goes against an accepted ADR needs a new ADR that supersedes it, first.
- No `unwrap()` outside tests. Use `?`, or `expect("why this can't fail")` for true invariants.
- Errors: a typed error enum per crate, implementing `std::error::Error`. No panics on bad user data such as malformed Markdown, odd file names or a missing vault.
- Never lose user data. Write files atomically (write to a temp file, then rename), keep undo for every file operation, and move deleted files to the system Trash.
- Public items get doc comments. Modules start with a `//!` line saying what they own.
- Keep logic testable: Vim behaviour, the ex parser, the index and links are pure and unit-tested in `mnote-vim` and `mnote-core`. The app crate stays thin.
- Tests sit next to the code (`#[cfg(test)] mod tests`). Bugs get a regression test first.
- Follow the design system (tokens, components, decisions D-001 and D-002) for anything visible. Don't hard-code colours or sizes once the theme exists (AMI-153).

## Git and process

- Small, focused commits whose message says what changed and why, with the Linear issue in the subject, for example `Port design tokens to a gpui theme (AMI-153)`.
- `main` always builds and passes CI.
- No Claude or AI attribution lines in commits or PRs.

## Commands

```bash
cargo run -p mnote-app
```

```bash
cargo test --workspace
```

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

```bash
cargo build --release -p mnote-app
```

A shareable `mNote.app` and `.dmg` in `dist/` (see the script for universal builds and Developer ID signing):

```bash
tools/package-macos.sh
```

Keystroke-to-pixels in the real window (types into a note, then prints p50/p95/max of mNote's work, of the wait for the display's next frame, which no app controls, and of the two together):

```bash
MNOTE_LATENCY=1 MNOTE_LATENCY_BENCH=300 target/release/mnote ~/notes/some-note.md
```

The same on a large note (336 KB of long lines), wrapped and not, in a throwaway HOME; it fails if p95 goes over 8 ms. Run it after changes to the keystroke path:

```bash
tools/bench-large-note.sh
```

The plugin API reference (`docs/plugins.md`) is generated from the API table in `crates/mnote-app/src/editor/plugins.rs`; a test fails when it's stale. After changing the Lua API:

```bash
MNOTE_WRITE_DOCS=1 cargo test -p mnote-app plugin_docs
```
