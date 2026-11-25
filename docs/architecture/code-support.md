# Code support: design

Status: **accepted** (2026-11-25). Decisions are recorded as ADRs in [`docs/adr/`](../adr/); this document is the whole picture they fit into.

mNote opens code as well as notes: highlighting, indentation, comments, text objects, folds, diagnostics, go to definition, completion and snippets. It does this the way established editors do (VS Code, Zed, Helix, Neovim), not with special cases per language, and inside mNote's budgets (CLAUDE.md).

The same foundations are built so that AI features and agents, like Zed's, can be added later **without rewriting any of them**: [Ready for AI](#ready-for-ai) says what each future feature uses, and which phase builds it.

## Goals

- Code files open as code: the language is recognised, coloured with tree-sitter, and edited with the same Vim.
- Editing knows the language: indent, comments, text objects and folds come from the language's tree, not from regexes.
- Language servers (LSP) give diagnostics, navigation, hover, rename, code actions, formatting and completion.
- What Neovim users already have is reused: nvim-treesitter's parsers and queries, Mason's installed servers, lspconfig's server defaults, VS Code-format snippets, and the same keys (`gd`, `K`, `gr`, `]d`, `⌃n`, `⌃y`).
- Adding a language is data, not code: a language pack.
- Notes and code share one pipeline: a Markdown note's code fences are highlighted by the same syntax layer as a `.rs` file.
- AI and agents fit on top later as new crates and UI, using the text model, transport, context and permissions built here unchanged.

## Non-goals (for this design)

- Running Neovim's Lua plugins unchanged. They depend on Neovim's API (`vim.api`, `vim.lsp`, `vim.treesitter`); their *data* is reused instead, and mNote's own Lua API gets extension points (see [Extension points](#extension-points)).
- WASM grammars ([ADR-0002](../adr/0002-native-grammars.md)).
- The AI features themselves (model providers, the agent panel, edit predictions): they get their own design. This one builds what they stand on.
- Debugging (DAP), terminals, project-wide refactoring beyond what LSP gives, and Git integration beyond today's blame.

## Principles

These come from how the established editors are built, and from mNote's rules.

1. **The UI thread never waits.** Parsing, server traffic, JSON, disk, network and process I/O run on background threads and report back as messages. Only cheap, bounded work (applying an edit to a tree, querying the visible lines) runs on a keystroke.
2. **Everything is versioned.** Background work runs on a snapshot of a document at a version. A result for an older version is moved onto the current text through anchors, or dropped.
3. **Everything can be cancelled.** A newer request replaces an older one; a parse is superseded by the next edit.
4. **Coordinates are types.** Characters, UTF-8 bytes, rows and columns, and LSP positions are different types with explicit conversions ([ADR-0005](../adr/0005-coordinates-and-encodings.md)).
5. **Features follow capabilities.** Nothing is hard-coded per language or per server: a language pack says what a language has; a server's `initialize` reply says what it can do.
6. **Data over code.** Grammars, queries, language descriptions, server defaults, themes and snippets are files that can be added without rebuilding mNote.
7. **Work proportional to the change and to the screen.** Incremental parsing; highlighting only the visible rows; incremental document sync.
8. **Every edit says who made it.** The user, a plugin, a language server or an agent: edits carry their origin, can be held as proposals for review, and undo by the step that made them.
9. **One mechanism per concern, shared by every client.** One JSON-RPC transport (LSP, ACP, MCP), one way to describe context (completion, plugins, agents), one permission store (plugins, grammars, servers, agent tools).
10. **Tested against the protocols.** Golden tests for queries, scripted fake peers for every protocol, and property tests for incremental parsing and coordinate conversions.

## Architecture

```
  mnote-app (gpui)   UI glue: drawing, popups, keys, settings, services
                     SyntaxService · LspService · CompletionUi · ProposalReview · Pickers
                     ▲                 ▲                    ▲
                     │ messages (async_channel)             │
  ┌──────────────────┴───┐  ┌──────────┴─────────┐  ┌───────┴─────────────────┐
  │ mnote-vim            │  │ mnote-lsp          │  │ mnote-ai (later)        │
  │ motions, operators,  │  │ servers, sync,     │  │ providers, agents (ACP),│
  │ ext hooks            │  │ features           │  │ MCP, tools, predictions │
  └──────────▲───────────┘  └───┬────────▲───────┘  └───┬────────▲────────────┘
             │                  │        │              │        │
             │                  ▼        │              ▼        │
             │           ┌──────────────────────────────────┐    │
             │           │ mnote-rpc: processes, JSON-RPC   │    │
             │           │ framing, requests, cancellation  │    │
             │           └──────────────────────────────────┘    │
  ┌──────────┴───────────────────────────────────────────────────┴───┐
  │ mnote-core: Buffer (rope, history, marks, change log with        │
  │ origins, edit groups, proposals), vault, index, config,          │
  │ permissions, context                                             │
  └──────────────────────────────▲───────────────────────────────────┘
  ┌──────────────────────────────┴───────────────────────────────────┐
  │ mnote-lang: language registry, packs, grammar loading, queries,  │
  │ syntax map + injections, highlights, indent, text objects,       │
  │ folds, comments, outline, completion engine, snippets            │
  └──────────────────────────────────────────────────────────────────┘
```

- **`mnote-lang`** (new, below `mnote-core`): everything about languages that needs only a rope and tree-sitter. It owns today's `code.rs`, `filetype.rs` and `text/syntax.rs`, generalised. No gpui, no I/O on the caller's thread. [ADR-0001](../adr/0001-crate-layering.md).
- **`mnote-core`**: the `Buffer` holds a `SyntaxMap` from `mnote-lang`, and records a change log with origins, edit groups and proposals (below). It also holds the permission store and the context API. The index keeps using the Markdown tree.
- **`mnote-rpc`** (new): spawning a process and talking JSON-RPC to it: framing, request ids, timeouts, cancellation, notifications both ways, logging. No gpui. LSP uses it now; ACP and MCP use it later. [ADR-0009](../adr/0009-shared-rpc-transport.md).
- **`mnote-lsp`** (new): the LSP client on top of `mnote-rpc`. [ADR-0006](../adr/0006-lsp-client.md).
- **`mnote-ai`** (later, its own design): model providers, agents, MCP, tools, edit predictions. It needs nothing below it to change. [ADR-0012](../adr/0012-ai-integration-boundaries.md).
- **`mnote-vim`**: unchanged in shape. Language-aware pieces plug in through the existing extension hooks (`register_text_object`, `register_operator`, `register_motion`) and a new indent hook.
- **`mnote-app`**: thin. It connects services to the editor, draws popups, diagnostics and proposals, and maps keys and settings.

## 1. Text model

The rope, undo history and marks exist. Code support, and later AI, need the following from `mnote-core`'s `Buffer`; all of it is built in phase 1a.

### Snapshots and versions

`Buffer::snapshot() -> Snapshot { rope, revision, language }`: a rope clone is O(1). The revision number already exists and becomes the document version that LSP sees. Background work always carries the revision it ran on.

### Change log, with origins

Every edit is recorded once:

```rust
Change {
    range: Range<CharOffset>,
    old_bytes: Range<ByteOffset>,
    new_text: Arc<str>,
    revision: u64,
    origin: Origin,       // User, Plugin(name), Lsp(server), Agent(session), Prediction, External
    group: GroupId,       // the edit group it belongs to
}
```

It feeds, from one source:

- the syntax map (`Tree::edit` with an `InputEdit`, as today),
- LSP `textDocument/didChange` (incremental, in the negotiated encoding),
- rebasing late results: a result computed at revision *r* is moved through the changes after *r*,
- events: `TextChanged` gains the origin, so plugins (and later agents and predictions) can tell their own edits from the user's,
- recent-edit history for the context API (an edit prediction model's main input).

The log is trimmed once every consumer has seen a revision; a bounded tail of user edits is kept for context. [ADR-0010](../adr/0010-edit-origins-and-proposals.md).

### Edit groups

An edit group is one undoable step. It may span several buffers: undo from any of them undoes all of it. Every group has an origin and an optional label ("Rename `foo` to `bar`", "Agent turn 3"). Files that aren't open are opened as parked documents (the panes already park documents), edited, and saved atomically like any other save.

### Proposals

An edit group can be held as a **proposal** instead of being applied:

- Its edits are kept as anchored hunks against the current text, and drawn as a diff: removed text struck through, added text inline, with gutter markers.
- Each hunk can be **accepted** (applied as part of the proposal's group) or **rejected** (dropped). The whole proposal can be accepted or rejected at once.
- When the user edits under a hunk, the hunk is rebased through the change log, or marked stale if it no longer applies.
- Where a proposal comes from doesn't matter to the mechanism: an LSP workspace edit (when review is turned on), a plugin, or later an agent.

LSP workspace edits are its first user (phase 2), so the mechanism is built and tested before any AI uses it.

### Anchors

Marks already move with edits. They gain a **bias** (stay before or after text inserted exactly at them) and become the single way to hold a position across edits: diagnostics, semantic tokens, snippet tabstops, document highlights, inlay hints, proposal hunks.

Mark kinds gain **inline ghost text**: text drawn at a position, inside the line, that isn't part of the buffer. Inlay hints use it now; LSP inline completions and edit predictions use it later.

### Coordinates

`CharOffset` (Vim's unit), `ByteOffset` (tree-sitter's), `Point { row, column_bytes }` (tree-sitter's), and `LspPosition { line, character }` with a `PositionEncoding` (`Utf8`, `Utf16`, `Utf32`). Conversions are methods on the rope (`ropey` has the primitives). Mixing them is a type error. [ADR-0005](../adr/0005-coordinates-and-encodings.md).

## 2. Language registry and language packs

A language is described by data. [ADR-0003](../adr/0003-language-packs.md).

```
languages/rust/
  language.toml
  queries/
    highlights.scm  injections.scm  indents.scm
    textobjects.scm  folds.scm  locals.scm
```

```toml
# language.toml
name = "rust"
extensions = ["rs"]
filenames = []                 # exact names, like "Makefile"
shebangs = []                  # "python3", "bash"
first_line = ""                # a pattern, like "^#!.*\\bpython"
grammar = "rust"               # built in, a parser file, or found in Neovim's folders
comment = { line = "//", block = ["/*", "*/"] }
indent = { unit = "    ", tab_width = 4 }
brackets = [["(", ")"], ["[", "]"], ["{", "}"]]
auto_pairs = [["(", ")"], ["[", "]"], ["{", "}"], ["\"", "\""]]
word_chars = "_"
roots = ["Cargo.toml", ".git"]
servers = ["rust-analyzer"]
formatter = { command = ["rustfmt", "--emit=stdout"] }   # optional; else LSP formatting
```

- **Where packs come from**, later ones overriding earlier ones field by field:
  1. built in (embedded with `include_str!`),
  2. Neovim's folders, for grammars and queries only (when `reuse_neovim = true`, the default),
  3. the user's `~/.config/mnote/languages/<name>/`.
- **Queries** use nvim-treesitter's conventions, so its query files work as they are: capture names (`@keyword.function`), `; inherits: ecma` lines, and its predicates and directives (`#lua-match?`, `#any-of?`, `#has-ancestor?`, `#not-has-parent?`, `#contains?`, `#set!`, `#offset!`). `mnote-lang` implements those predicates; an unknown predicate makes that pattern never match and is reported once per query, it never fails the language.
- **Filetypes** become a view of the registry: a file's language comes from its name, then its extension, then its shebang or first line. Markdown stays the one *note* language.

## 3. Grammars

Grammars are native tree-sitter parsers. [ADR-0002](../adr/0002-native-grammars.md).

- **Built in**: Markdown (and Markdown inline) plus the six code grammars mNote already ships (Rust, Python, JavaScript, TypeScript/TSX, Bash, JSON). Notes never depend on a loaded grammar.
- **Loaded**: a parser library (`.so` / `.dylib`) from mNote's grammar folder (`~/.config/mnote/grammars/`) or Neovim's (`~/.local/share/nvim/site/parser/`, `~/.local/share/nvim/lazy/nvim-treesitter/parser/`).
- **The loader** is one module, `mnote-lang/src/grammar/native.rs`, the only place outside `summon.rs` where `unsafe` is allowed. It:
  1. only loads from those folders,
  2. asks once per grammar and fingerprint, through the permission store (section 6),
  3. opens the library with `libloading` and calls its `tree_sitter_<name>` function,
  4. checks the grammar's ABI version is within what our tree-sitter supports, and refuses it otherwise with a message saying which version it has,
  5. never unloads a library (a `Language` points into it).
- **A `GrammarEngine` trait** sits in front of the loader, so a WASM engine could be added later without touching the syntax layer.
- **Installing** (phase 4): `:TSInstall <lang>` fetches a grammar's source at a pinned revision and compiles it with the system C compiler (Xcode's command-line tools), as nvim-treesitter does.

## 4. Syntax map

One structure per document, for notes and code alike. [ADR-0004](../adr/0004-syntax-map.md).

- **Layers**: a root layer for the document's language and one layer per injection: a Markdown fence in Rust, a Rust doc comment's Markdown, frontmatter's YAML, a JS template's CSS. Each layer has its language, the byte ranges it covers (`Parser::set_included_ranges`) and its tree. `injections.scm` decides the layers (`@injection.content`, `@injection.language`, `injection.combined`).
- **Parsing** generalises today's Markdown worker: edits apply to every tree at once with `Tree::edit` (cheap, on the keystroke); a worker reparses the root and changed layers from a snapshot; results land with their revision and are rebased or redone. A parse has a time limit, so a huge or pathological file can't stall the worker.
- **Large files**: above a size set in config.toml (default 4 MB), injections are skipped, and above a second limit (default 16 MB), highlighting is off; editing is never slowed.
- **Highlighting**: queries run over the *visible rows only*, on every layer that touches them, giving spans `(byte range, scope)`. Deeper layers win over the layers they sit in; a pattern's `priority` metadata breaks ties. Spans are cached per line per revision, so scrolling and redrawing cost nothing new.
- **Scopes to colours**: themes gain a `[syntax]` table mapping capture names to colour tokens, falling back by dropping the last part (`@keyword.function` → `@keyword` → plain). Built-in themes cover nvim-treesitter's standard captures, so Neovim colourschemes can be ported as data.
- **Semantic tokens** from a language server are another highlight layer on top, mapped through the same scope table (`@lsp.type.function`, as Neovim names them). A setting chooses whether they override tree-sitter.
- **Markdown notes**: code fences stop being highlighted one block at a time (today's `code::highlight`); they become injection layers, parsed incrementally like everything else.

## 5. Editing services

All of these live in `mnote-lang`, take a syntax map and a position, and return plain data. `mnote-vim` and `mnote-app` call them through hooks.

- **Indent**: nvim-treesitter's `indents.scm` semantics: `@indent.begin`, `@indent.end`, `@indent.align`, `@indent.dedent`, `@indent.branch`, `@indent.ignore`, `@indent.auto`, `@indent.zero`. Used by `o`, `O`, Enter, `==`, the `=` operator, and after pasting lines. Without an indent query a language keeps the previous line's indent. The indent unit comes from the language pack, overridable per file by `.editorconfig` (later) and `:set shiftwidth`.
- **Comments**: `gc{motion}`, `gcc`, visual `gc`. The comment token comes from the language at that position, so a fence inside a note uses its own language's comment.
- **Text objects and motions**: `textobjects.scm` captures (`@function.outer` / `.inner`, `@class.*`, `@parameter.*`, `@comment.*`, `@block.*`) become `af` `if` `ac` `ic` `aa` `ia` `a/` `i/`, and `]m` `[m` `]]` `[[` move between functions and classes. They're registered with `mnote-vim`'s existing extension hooks.
- **Folds**: `folds.scm` gives fold ranges; `zc` `zo` `za` `zR` `zM` (folding in the view is its own issue).
- **Brackets**: pairs from the language pack; `%` and auto-pairs use the tree so brackets in strings and comments don't count.
- **Outline**: `locals.scm` and `tags.scm` (definitions) give a symbol list without a server; with a server, `documentSymbol` replaces it.

## 6. Shared services: context and permissions

Two services in `mnote-core` that every client uses the same way.

### Context

One API answers "what is around this position", so completion, plugins and later agents and edit predictions ask the same question the same way. [ADR-0011](../adr/0011-context-api.md).

- **Providers** each add one kind of item:
  - the enclosing syntax nodes (function, class, block) from the syntax map;
  - the outline;
  - diagnostics near the position;
  - recent edits, from the change log;
  - open documents;
  - symbols under the cursor, with their definitions and references through LSP;
  - selections;
  - and, for notes, backlinks and linked notes from the index.
- **A request** says where it's asked (document and position), which providers it wants, and how large the answer may be. A token budget is just a size limit, so the API knows nothing about models.
- **The answer** is a list of items, each with its kind, document, anchored range, text, and a priority. Plugins can register providers (`mnote.context.register`).
- **It runs in the background**, is versioned like everything else, and can be cancelled.

Phase 1b builds the providers that need only syntax; phase 2 adds diagnostics and LSP symbols.

### Permissions

One store remembers what the user allowed: today's plugin trust, generalised. [ADR-0012](../adr/0012-ai-integration-boundaries.md).

- **An entry** is a subject (a plugin, a grammar library, a language server command, later an agent tool or a context server), a fingerprint of what was allowed, and a scope: once, for this session, or always.
- **A prompt** goes through one picker that says exactly what will run or what will be read.
- **The grammar loader** is its first new user (phase 1a). Plugins move onto it, and agent tools later reuse it unchanged.

## 7. LSP client

`mnote-lsp`, on `mnote-rpc`. [ADR-0006](../adr/0006-lsp-client.md), [ADR-0009](../adr/0009-shared-rpc-transport.md).

### Transport

- **`mnote-rpc` provides** a peer: one child process (or later a socket), a reader thread that parses framed JSON-RPC messages, a writer thread that drains a channel, and stderr to a log. It also numbers requests, keeps timeouts, cancels requests (each protocol's own cancel message, given as a hook), and delivers both requests and notifications coming the other way. No async runtime (CLAUDE.md).
- **Framing is pluggable:** LSP's `Content-Length` headers, and newline-delimited JSON for protocols that use it (ACP and MCP over stdio).
- JSON goes through `serde_json` and `serde`, already locked through gpui (an accepted exception to the dependency rule; [ADR-0006](../adr/0006-lsp-client.md)).
- Protocol types are mNote's own: the subset of LSP 3.17 it uses, serde-derived, each with the spec section it follows. (`lsp-types` would bring version churn and URI crates for little gain.)

### Server manager

- Servers start **lazily**, when the first document of a language that lists them opens.
- **The project root** is found by walking up from the file for the language's `roots` markers, stopping at the vault's root or the home folder. Several roots can share one server through workspace folders when the server supports them.
- **`initialize`** sends `clientInfo`, the capabilities mNote implements (and nothing else), `positionEncodings: ["utf-8", "utf-16"]`, `workspaceFolders`, and `initializationOptions` from settings.
- **Lifecycle**: `shutdown` and `exit` on quit; a crashed server restarts with backoff, at most three times in five minutes, then stays off with a message.
- **Commands**: `:LspInfo` (servers, roots, capabilities, status), `:LspRestart`, `:LspStop`, `:LspLog`.

### Finding servers

A server's command is looked up in this order ([ADR-0008](../adr/0008-finding-servers.md)):

1. `cmd` in config.toml's `[lsp.<server>]`,
2. mNote's tools folder (`~/.local/share/mnote/tools/bin/`, once the installer exists),
3. **Mason's** folder (`~/.local/share/nvim/mason/bin/`),
4. `PATH`.

Server defaults (command, file types, root markers, settings) ship as data in `servers.toml`, ported from nvim-lspconfig's configurations. config.toml can override any field and pass `settings` (sent through `workspace/configuration`). The first start of a server command goes through the permission store.

### Document sync

`didOpen` / `didChange` / `willSave` / `didSave` / `didClose`, with the buffer revision as the version. Changes are incremental when the server supports it (from the change log, in the negotiated encoding), otherwise the full text. Only documents of a server's languages are sent to it.

### Requests

- **The request manager** (in `mnote-rpc`) numbers requests and keeps, for each: the document and revision it was asked at, a callback, a timeout, and whether a newer request supersedes it.
- **Superseded requests are cancelled** with `$/cancelRequest`: a new completion cancels the old one, moving the cursor cancels a pending hover.
- **A reply for an older revision** is rebased through the change log if the feature allows it (diagnostics, highlights), or dropped (completion, hover).
- **Server-to-client requests** are answered: `workspace/applyEdit`, `workspace/configuration`, `window/showMessage(Request)`, `window/workDoneProgress/create`, `client/registerCapability` (for file watching), `workspace/workspaceFolders`.

### Features

Each feature is a module behind one interface: is it supported (from the server's capabilities), how to build its request, what to do with the reply.

| Feature | Keys and commands |
| --- | --- |
| Diagnostics (pushed, and pulled in 3.17) | gutter signs, underlines, text at the end of the line; `]d` `[d`; `:Diagnostics` picker; counts in the status line |
| Hover | `K`, a popup drawn with the styled Markdown renderer |
| Definition, declaration, type definition, implementation | `gd` `gD` `gy` `gi`; several results open a picker |
| References | `gr`, a picker |
| Document highlight | the symbol under the cursor highlighted after a pause |
| Rename (checked with `prepareRename`) | `<leader>rn` |
| Code actions (with `resolve`, and commands) | `<leader>ca`, a picker |
| Formatting, range formatting | `:Format`, `gq`, format on save (setting) |
| Signature help | a popup while typing arguments |
| Symbols (document and workspace) | `:Symbols`, `:WorkspaceSymbols` pickers |
| Inlay hints | inline ghost text, toggled with `:InlayHints` |
| Semantic tokens (full and delta) | a highlight layer |
| Workspace edits (rename, code actions, `applyEdit`) | applied as one edit group with origin `Lsp`; with `review_edits = true`, held as a proposal first |
| Completion | see below |

Keys are default bindings through the keymap, so `[keys]` in config.toml can change them; they only act where a server supports the feature.

### Files changing on disk

The vault's file watcher already exists; changes are forwarded as `workspace/didChangeWatchedFiles` to servers that registered for them, and recorded in the change log with origin `External` for open documents.

## 8. Completion and snippets

Completion is an engine with sources, not part of the LSP client. [ADR-0007](../adr/0007-completion-engine.md).

- **Sources** implement one trait and run in the background: the language server, words in open documents, file paths, snippets, plugin sources, and later AI sources.
- **The engine** keeps one completion session at a time:
  1. it opens on a trigger character, after a few identifier characters, or with `⌃n` / `⌃Space`;
  2. it asks the sources and fuzzy-matches the word being typed with `nucleo-matcher` (already a dependency);
  3. it ranks by match score, then the server's `sortText` and `preselect`, then nearness and recent use;
  4. it removes duplicates across sources;
  5. it asks again while a server's list is marked incomplete.
- **Choosing an item** runs `completionItem/resolve` lazily, after a short pause on it, to fetch its documentation and extra edits.
- **Accepting** applies its `textEdit` (insert or replace range), its `additionalTextEdits` (auto-imports) and its commit characters, then expands it if it's a snippet.
- **Keys**: `⌃n` / `⌃p` move through the list, `⌃y` accepts, `⌃e` closes, Tab optionally accepts (setting).
- **Inline suggestions** are the second kind of result the engine shows: one suggestion drawn as ghost text at the cursor, accepted whole or a word at a time. LSP 3.18's inline completions (still a proposed part of the spec) and later edit predictions are their sources; the session rules are the same as the list's (versioned, cancelled by typing that doesn't match).
- **Snippets** follow the LSP snippet grammar:
  - tabstops `$1` and `${1:placeholder}`, choices `${1|a,b|}`, the final stop `$0`, and variables such as `TM_FILENAME` and `CURRENT_YEAR`;
  - tabstops are anchors; editing one tabstop changes its copies;
  - Tab and Shift-Tab jump between stops (or `⌃j` / `⌃k`), and leaving the snippet's text ends it;
  - snippet files are in the VS Code JSON format, read from `~/.config/mnote/snippets/` and, when present, friendly-snippets' folder in Neovim's data.

## 9. Tools installer (phase 4)

- **`:LspInstall <server>`** reads Mason's package registry (its published registry index), picks the package's asset for this platform, downloads it, checks its checksum, and installs it into `~/.local/share/mnote/tools/<package>/` with its binaries linked into `tools/bin/`. `:LspUninstall` and `:LspUpdate` complete it. Nothing installs system-wide, and nothing downloads without a command.
- **`:TSInstall <lang>`** compiles a grammar into `~/.config/mnote/grammars/` (section 3). The source revisions are pinned in a lock file that mNote ships.
- The installer's downloads use the same HTTP client the AI layer will ([ADR-0012](../adr/0012-ai-integration-boundaries.md)), so mNote has one network stack.

## Ready for AI

What Zed-style AI features need, what they stand on, and where that is built. Nothing here is AI-specific code; it's the foundations, built and tested by code support's own features first.

| Future feature | Stands on | Built in |
| --- | --- | --- |
| An agent edits files, and you review its diff hunk by hunk | edit groups across buffers, proposals, origins, anchors and rebasing | 1a (model), 2 (review UI, first used by LSP workspace edits) |
| Undo one agent turn | edit groups with an origin and a label | 1a |
| Edit predictions (grey text at the cursor, Tab to accept) | inline suggestions in the completion engine, ghost-text marks, recent edits from the change log, cancellation | 1a (marks, change log), 3 (inline suggestions) |
| Context for a model: current function, outline, diagnostics, recent edits, open files, symbols, linked notes | the context API and its providers | 1b (syntax), 2 (LSP) |
| Agent tools: read and edit files, search, diagnostics, run commands | proposals, the vault's atomic writes and Trash, grep, the job runner, the permission store | 1a (permissions, proposals), today's vault and jobs |
| External agents (Claude Code, Gemini CLI) through ACP, the Agent Client Protocol Zed made | `mnote-rpc` (newline-delimited JSON-RPC over stdio), proposals for their edits, the permission store for their tool calls | 2 |
| MCP context servers, and mNote serving the vault to agents as an MCP server | `mnote-rpc`, the context API, the permission store | 2 |
| Model providers (Anthropic, OpenAI, local models) with streamed replies | one HTTP client on background threads, keys in the macOS Keychain | 4 (the installer brings the HTTP client) |
| Agents told what you're doing | events with origins (`TextChanged`, `CursorMoved`, `NoteOpened`) | 1a |
| A view of excerpts from many files (Zed's multibuffer: agent diffs, references, diagnostics) | proposals and anchors across buffers; a view of its own | its own design later; nothing below changes |

What stays for the AI design itself: the `mnote-ai` crate, the agent panel and its conversation UI, which providers and models, edit-prediction models, privacy settings (what leaves the machine; off by default, opt-in per vault, local models first), and the multibuffer view. [ADR-0012](../adr/0012-ai-integration-boundaries.md) fixes the boundaries they work within.

## Extension points

mNote's Lua API gains hooks so plugins can extend each layer properly, instead of patching around it:

- `mnote.lang.register(pack)`: a language pack from Lua.
- `mnote.lsp.register(server)` and `mnote.lsp.on_attach(fn)`: server configurations and per-server keys or settings.
- `mnote.complete.register(source)`: a completion source (list items or inline suggestions).
- `mnote.context.register(provider)` and `mnote.context.get(request)`: give and read context.
- `mnote.propose(edits, { label })`: edits held as a proposal for review.
- `mnote.format.register(lang, fn)` and code-action providers.
- Events: `LspAttach`, `LspDetach`, `DiagnosticChanged`, `ProposalResolved`; `TextChanged` carries the edit's origin.

These are written into `docs/plugins.md` (generated, as today) when each layer lands.

## Performance budgets

These extend CLAUDE.md's budgets and are measured the same way (release build, Apple Silicon).

| What | Budget |
| --- | --- |
| A keystroke in a code file, including syntax-tree edits and highlighting the visible rows | inside the existing < 8 ms |
| First colours after opening a 1 MB code file | < 50 ms (it opens plain at once; colours follow) |
| Completion list update per keystroke, once results are in | < 4 ms |
| Drawing a proposal of 1 000 hunks while scrolling | inside the existing < 8 ms (only visible hunks are drawn) |
| A context request with syntax providers only | < 5 ms, off the UI thread |
| LSP, JSON and network work on the UI thread | none (reader threads parse; the UI applies small results) |
| Cold start with no code file open | unchanged (< 200 ms): no grammar is loaded, no server started |
| Release binary size | < 25 MB: new languages are loaded, not built in; `mnote-rpc` and `mnote-lsp` are expected to add about 1 MB |

`tools/bench-large-note.sh` gains a large Rust file, a completion run and a large proposal.

## Testing

- **Query golden tests**, in nvim-treesitter's format: sample files with comment assertions under the code they check (`-- ^ @keyword`), run for every built-in language and for Markdown injections.
- **Indent tests**: before-and-after fixtures for `o`, Enter, `==` and `=`.
- **Incremental parsing**: property tests that random edits leave the incremental tree equal to a fresh parse.
- **Coordinates**: property tests for every conversion, with multi-byte and astral-plane text.
- **Proposals and edit groups**: property tests that accepting every hunk equals applying the edits, that rejecting every hunk leaves the text unchanged, and that user edits in between rebase or stale hunks correctly; undo of a group spanning several buffers.
- **A fake peer for `mnote-rpc`**: a small test binary that plays scripted sessions (initialize, sync, cancellation, requests from the other side, crashes) in both framings, so LSP now, and ACP and MCP later, are tested without real servers or agents.
- **Real-server smoke tests** (ignored by default, run by hand): rust-analyzer and lua-language-server on fixture projects.

## Phases

Each phase is a set of Linear issues. Every issue comes with its tests and budgets, and ends with everything passing.

1. **1a. Foundations, and code files as code.**
   - crate `mnote-lang`, with the registry, filetypes and syntax moved into it;
   - coordinate types; the change log with origins; edit groups across buffers; proposals (model and tests); anchors with bias; ghost-text marks;
   - the permission store, with plugin trust moved onto it;
   - language packs (format, built-in packs, the user's folder);
   - the native loader, with the ABI check and the permission prompt, and finding Neovim's parsers;
   - the syntax map with injections (Markdown fences move onto it);
   - highlighting from queries, with nvim-treesitter's predicates and scopes in themes.
2. **1b. Editing code.** Indent, comments (`gc`), text objects and motions, bracket matching, folds (data), outline without a server, and the context API with its syntax providers.
3. **2. LSP.**
   - crate `mnote-rpc`: process peers, both framings, request manager, cancellation, the fake peer;
   - crate `mnote-lsp`: server manager, finding servers (config, Mason, `PATH`) and `servers.toml`, document sync;
   - diagnostics, hover, navigation, references, rename, code actions, formatting, symbols, inlay hints, semantic tokens;
   - workspace edits as edit groups, and the proposal review UI (`review_edits`);
   - context providers for diagnostics and LSP symbols.
4. **3. Completion.** The engine, list sources (LSP, words, paths, snippets), inline suggestions (LSP inline completions), resolve, snippets, signature help.
5. **4. Installers and extension points.** The HTTP client, `:LspInstall` from Mason's registry, `:TSInstall`, the Lua hooks.

After these, the AI design starts from a finished base: `mnote-ai` adds providers, agents over ACP, MCP, tools and edit predictions, and none of the crates above change shape.

## Open questions

- Folding needs the view to hide lines; is that in 1b or its own phase?
- Should `.editorconfig` be read (it's what other editors use for indent settings)?
- Format on save: off by default, or on when the language has a formatter?
- `review_edits`: off by default (as VS Code and Helix apply workspace edits at once), or on for edits that touch files that aren't open?
