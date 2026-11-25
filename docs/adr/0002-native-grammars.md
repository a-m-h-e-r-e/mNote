# 0002. Native tree-sitter grammars, one isolated `unsafe` loader

Status: Accepted (2026-11-25)

## Context

Languages beyond the built-in ones need grammars mNote doesn't ship. Tree-sitter grammars can run as native libraries (`.so` / `.dylib`, as Neovim, Helix and Emacs load them) or as WASM in a sandbox (tree-sitter's `wasm` feature with wasmtime, as Zed does for extension grammars). In both, queries and trees run natively; only the parser runs in the sandbox.

| | Native | WASM |
| --- | --- | --- |
| Parsing speed | native | slower (reports put it around 1.5–3×; to be measured) |
| Load time | about a millisecond | compiling each grammar on first load: tens to hundreds of ms unless cached |
| Binary size | `libloading`, tens of KB (already locked through gpui) | wasmtime with its compiler: roughly 10–15 MB or more |
| Dependencies | one small crate | a large, fast-moving tree |
| `unsafe` in our code | one small loader | none |
| Safety | a buggy or malicious grammar runs with mNote's rights and can crash it | sandboxed; a crash becomes an error |
| Portability | one build per OS and CPU | one file everywhere |
| Neovim's parsers | reused as they are | rebuilt and hosted |
| Notarization | loading libraries not signed by us needs the `disable-library-validation` entitlement | no issue |

mNote's budgets are 8 ms per keystroke, 200 ms cold start and 25 MB of binary (22.1 MB used).

## Decision

Native grammars.

- **Built in**: Markdown and the six code grammars mNote already ships, so notes never depend on a loaded grammar.
- **Loaded**: parser libraries from mNote's grammar folder and Neovim's parser folders only.
- **One module**, `mnote-lang/src/grammar/native.rs`, is the only `unsafe` code besides `summon.rs`. Each block has a `// SAFETY:` comment, and the lint is lifted only there. CLAUDE.md lists it.
- **Before use**, a grammar's ABI version is checked against tree-sitter's supported range. A grammar outside it is refused with a message naming its version.
- **Trust**: a new or changed grammar library needs the user's OK once, recorded by fingerprint in the shared permission store, which plugin trust also moves onto ([ADR-0012](0012-ai-integration-boundaries.md)).
- **Libraries are never unloaded**, since a `Language` points into its library.
- **A `GrammarEngine` trait** sits in front of the loader, so a WASM engine can be added later as an optional build feature.

## Alternatives

- **WASM only.** Safer for untrusted grammars, but costs speed on the keystroke path, cold-start time, about half the binary budget and a large dependency, and can't reuse Neovim's parsers.
- **Only built-in grammars.** No `unsafe`, but each grammar adds roughly 0.5–2 MB to the binary, and new languages would need a new mNote release.

## Consequences

- Grammars run unsandboxed; the trust prompt and the fixed folders limit that to grammars the user chose.
- Notarizing mNote later needs the library-validation entitlement.
- Each platform needs its own grammar builds (`:TSInstall` compiles them locally).
- Revisit if mNote gets a public extension marketplace: that is where WASM's sandbox pays for itself.
