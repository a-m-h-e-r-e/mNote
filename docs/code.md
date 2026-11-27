# Editing code

mNote reads code with tree-sitter, so indenting, commenting, selecting and folding follow the code's structure rather than its characters. All of it works in code files and in code blocks inside notes: a ` ```rust ` block in a note indents, comments and folds as Rust.

The built-in languages are Rust, Python, JavaScript, TypeScript (and TSX), Bash, JSON and Markdown. Others load from Neovim's parsers or your own language packs (see [Where the rules come from](#where-the-rules-come-from)).

## Indent

New lines start at the indent the code around them calls for:

| Keys | What it does |
| --- | --- |
| `o`, `O`, Enter in INSERT | The new line's indent from the syntax: one level in after `{` or `:`, back out after `return` in Python |
| `==`, `={motion}`, `=ip`, `gg=G`, VISUAL `=` | Re-indents the lines; blank lines are emptied |
| `]p`, `[p` | Puts lines after (before) this one, shifted together so the first fits there |
| `}` `)` `]` typed first on a line | Moves the line back out to match its opener |

Where a language has no indent rules, a new line keeps the previous line's indent.

Each document has its own `shiftwidth`, `tabstop` and `expandtab`, starting from its language (Rust 4 spaces, Bash 2, Go tabs) and changed with `:set`.

## Comments

| Keys | What it does |
| --- | --- |
| `gcc`, `gc{motion}`, `3gcc`, VISUAL `gc` | Comments the lines in, or out when they all are |
| `gbc`, `gb{motion}` | Wraps the text in a block comment (`/* … */`), or unwraps it |

The comment is the language's at the cursor: `#` in a Python block of a note, `<!-- … -->` around the note's prose. A language without comments (JSON) says so. Each is one undo step and repeats with `.`.

## Text objects and motions

| Keys | Selects |
| --- | --- |
| `af` / `if` | A function, or its body |
| `ac` / `ic` | A class, struct or enum, or its body |
| `aa` / `ia` | An argument (with its comma), or just it |
| `a/` / `i/` | A comment |

They work after any operator (`daf`, `yic`, `cia`) and in VISUAL, where repeating one, or a count, grows to the enclosing one. A whole-line function or class is taken by lines, so `daf` leaves no blank gap.

| Keys | Goes to |
| --- | --- |
| `]m` / `[m` | The next or previous function's start |
| `]M` / `[M` | The next or previous function's end |
| `]]` / `[[` | The next or previous class (in a note's prose, the next or previous heading) |

These take counts and are jumps: `⌃o` comes back.

## Brackets and auto-pairs

`%` jumps between brackets and skips ones inside strings and comments.

In code, auto-pairs:

- an opening bracket or quote adds its closer;
- typing the closer when it's next steps over it;
- Backspace between a pair deletes both;
- Enter between brackets (`{|}`) puts the closer on its own line and the cursor on an indented line between.

Nothing pairs before a letter or a digit, nor inside a string or a comment; a quote doesn't pair after a letter. Rust leaves `'` out, for its lifetimes. Note prose never pairs. Turn it off with `auto_pairs = false` in config.toml, or `:set noautopairs`.

## Folds

Folds come from the code (functions, blocks, objects) and, in notes, from sections under headings, code blocks and nested lists.

| Keys | What it does |
| --- | --- |
| `zc` / `zo` / `za` | Closes the fold around the cursor (once more for the next one out), opens it, or toggles |
| `zC` / `zO` / `zA` | The same for every fold around the cursor |
| `zM` / `zR` | Closes, or opens, every fold |
| `zv` | Opens whatever hides the cursor's line |
| `zj` / `zk` | The next fold's start, the previous fold's end |

A closed fold shows its first line with `⋯ 12 lines` and a `▸` in the gutter; `▾` marks lines where an open fold starts. `j` and `k` step over a closed fold as one line, and the scrollbar counts it as one row. A search, a jump or an edit that lands inside one opens it. Each tab keeps its own folds, and they're restored with the session.

## Outline

`⌘⇧O` (`:outline`) lists the file's functions, types and other symbols, nested, or a note's headings. Type to filter; the preview shows the code; Enter goes there (`⌃o` comes back). The breadcrumbs above each pane's text show where its cursor is in the same outline.

## Context

`:context` shows what mNote gathers about the cursor for completion, plugins and agents: the selection, the enclosing function or section, your recent edits, the outline, linked notes, backlinks and other open documents, by priority. `:context links backlinks` asks only for those. Plugins read it with `mnote.context.get` and add their own with `mnote.context.register` ([plugins.md](plugins.md)).

## Settings

| Setting | Values | Default |
| --- | --- | --- |
| `auto_pairs` | `true`, `false` | `true` |

And `:set` for the shown document: `shiftwidth`/`sw`, `tabstop`/`ts`, `expandtab`/`et`, `autopairs`/`ap`.

## Where the rules come from

Each feature reads one kind of tree-sitter query per language: `indents`, `textobjects`, `folds`, `locals` and `tags`.

- **Built in:** mNote ships `indents`, `textobjects`, `folds` and `locals` for its built-in languages, from [nvim-treesitter](https://github.com/nvim-treesitter/nvim-treesitter) and [nvim-treesitter-textobjects](https://github.com/nvim-treesitter/nvim-treesitter-textobjects) (Apache-2.0; `crates/mnote-lang/queries`), and the grammars' own `tags` for the outline.
- **Your language packs:** `~/.config/mnote/languages/<language>/queries/<kind>.scm` replaces the built-in file, or adds to it when it starts with `; extends`.
- **Neovim:** for a grammar loaded from Neovim's parsers, Neovim's query folders are read too (nvim-treesitter's and nvim-treesitter-textobjects'). The built-in grammars don't use them, since they're written for other versions of those grammars.

A query that doesn't fit its grammar is reported once (`:messages`) and its feature falls back to what plain text does.
