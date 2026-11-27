# Editing queries for the built-in languages

The indent, fold, locals and text-object queries mNote ships for the grammars
built into it (AMI-232). They're the built-in layer: your own language packs
and plugins replace or extend them (`; extends`), and Neovim's folders are
not used for these grammars, whose versions differ from nvim-treesitter's.

| Files | From | Commit |
| --- | --- | --- |
| `*/indents.scm`, `*/folds.scm`, `*/locals.scm` | [nvim-treesitter](https://github.com/nvim-treesitter/nvim-treesitter) `runtime/queries` (main) | `5a7e5638e7d220575b1c22c8a2e099b52231886e` |
| `*/textobjects.scm` | [nvim-treesitter-textobjects](https://github.com/nvim-treesitter/nvim-treesitter-textobjects) `queries` | `d0d12338230c1ce4ce27373f5b8d50a8c691794b` |

Both are under the Apache License 2.0 ([LICENSE](LICENSE)). Changes made so
they compile against mNote's pinned grammars are marked `; mNote:` where
they're made.
