# 0003. Languages as data packs, compatible with nvim-treesitter

Status: Accepted (2026-11-25)

## Context

Today a language is a Rust value registered in code (`LanguageDef`). Established editors describe languages as data: Helix's `languages.toml` and `runtime/queries/`, Zed's language `config.toml` and queries, VS Code's `contributes.languages`. nvim-treesitter holds the most complete set of tree-sitter queries, under conventions of its own: capture names, `; inherits:` lines, and predicates and directives Neovim implements (`#lua-match?`, `#any-of?`, `#has-ancestor?`, `#set!`, `#offset!`).

## Decision

- A language is a **pack**: `language.toml` (name, file names and extensions, shebangs, first-line pattern, grammar, comment tokens, indent unit, brackets, auto-pairs, word characters, root markers, servers, formatter) and a `queries/` folder (`highlights`, `injections`, `indents`, `textobjects`, `folds`, `locals`).
- Packs come from three places, later ones overriding earlier ones field by field:
  1. built in,
  2. Neovim's folders (grammars and queries only, behind a setting that's on by default),
  3. the user's `~/.config/mnote/languages/`.
- Queries follow nvim-treesitter's conventions. `mnote-lang` implements its predicates and directives and `; inherits:`. A pattern with an unknown predicate never matches and is reported once; it never fails the language.
- The filetype table becomes a view of the registry.

## Alternatives

- **Helix's capture names and query set.** Also good, but smaller, and Neovim users already have nvim-treesitter's queries installed.
- **Keep languages in code.** No new format, but every language would need a release, and plugins couldn't add one.

## Consequences

- A new language needs no Rust: a folder, or a Lua call through `mnote.lang.register`.
- nvim-treesitter's predicates must be implemented and kept up with. Its main branch and its older master branch differ slightly; both are read, with tests on samples of each.
