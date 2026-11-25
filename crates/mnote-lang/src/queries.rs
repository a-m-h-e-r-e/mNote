//! Tree-sitter query files for languages, as nvim-treesitter writes them
//! (ADR-0003): `queries/<language>/<kind>.scm`, where kind is
//! `highlights`, `injections`, `indents`, `textobjects`, `folds` or `locals`.
//!
//! Two lines at the top of a file change how it combines:
//!
//! - `; inherits: ecma,jsx` puts those languages' queries of the same kind
//!   before it (a name in parentheses, `(jsx)`, is optional: missing is fine);
//! - `; extends` adds the file after the one it would otherwise replace.
//!
//! Files come from layers ranked low to high (built in, Neovim's folders,
//! the user's). For each language and kind the highest file that doesn't
//! extend is the base, and every file that extends comes after it.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::error::{LangError, Origin};

/// The kinds of query mNote reads.
pub const KINDS: [&str; 6] = [
    "highlights",
    "injections",
    "indents",
    "textobjects",
    "folds",
    "locals",
];

/// Where a query file came from, low to high.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    Builtin,
    Neovim,
    User,
}

/// One query file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryFile {
    pub text: String,
    pub origin: Origin,
    pub layer: Layer,
    /// Within its layer, higher wins (Neovim's folders have an order).
    pub rank: usize,
}

/// A language's query of one kind, with what it inherits put in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub text: String,
    /// The layer of its base file (or of its first extension, with none).
    pub layer: Layer,
}

/// A file's `; inherits:` names (optional ones `true`) and whether it
/// extends.
pub fn modelines(text: &str) -> (Vec<(String, bool)>, bool) {
    let mut inherits = Vec::new();
    let mut extends = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(comment) = line.strip_prefix(';') else {
            break;
        };
        let comment = comment.trim_start_matches(';').trim();
        if let Some(names) = comment.strip_prefix("inherits:") {
            for name in names.split(',') {
                let name = name.trim();
                if let Some(inner) = name.strip_prefix('(').and_then(|n| n.strip_suffix(')')) {
                    inherits.push((inner.trim().to_owned(), true));
                } else if !name.is_empty() {
                    inherits.push((name.to_owned(), false));
                }
            }
        } else if comment == "extends" {
            extends = true;
        }
    }
    (inherits, extends)
}

/// How a folder holds queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// `<root>/<language>/<kind>.scm`, as Neovim's folders do.
    ByLanguage,
    /// `<root>/<language>/queries/<kind>.scm`, as mNote's language packs do.
    InPacks,
}

/// Every language's query files, by language and kind: some given as
/// text, the rest found in folders when a language is first asked for (so
/// starting mNote reads none of Neovim's hundreds of query files).
#[derive(Debug, Default, Clone)]
pub struct QuerySet {
    files: BTreeMap<(String, String), Vec<QueryFile>>,
    folders: Vec<(PathBuf, Layout, Layer, usize)>,
}

impl QuerySet {
    /// Adds a file for `language`'s `kind`.
    pub fn add(&mut self, language: &str, kind: &str, file: QueryFile) {
        self.files
            .entry((language.to_owned(), kind.to_owned()))
            .or_default()
            .push(file);
    }

    /// Looks for queries in `root` (laid out as `layout`) at `layer` and
    /// `rank`, when a language is asked for. Nothing is read now.
    pub fn add_folder(&mut self, root: PathBuf, layout: Layout, layer: Layer, rank: usize) {
        self.folders.push((root, layout, layer, rank));
    }

    /// `language`'s `kind` files: those given, then those in the folders.
    /// A file that can't be read is an error.
    fn files_for(&self, language: &str, kind: &str) -> Result<Vec<QueryFile>, LangError> {
        let mut files = self
            .files
            .get(&(language.to_owned(), kind.to_owned()))
            .cloned()
            .unwrap_or_default();
        if language.contains(['/', '\\', '.']) {
            return Ok(files);
        }
        for (root, layout, layer, rank) in &self.folders {
            let path = match layout {
                Layout::ByLanguage => root.join(language).join(format!("{kind}.scm")),
                Layout::InPacks => root
                    .join(language)
                    .join("queries")
                    .join(format!("{kind}.scm")),
            };
            if !path.is_file() {
                continue;
            }
            let text = std::fs::read_to_string(&path).map_err(|err| {
                LangError::new(Origin::File(path.clone()), Some(language), err.to_string())
            })?;
            files.push(QueryFile {
                text,
                origin: Origin::File(path),
                layer: *layer,
                rank: *rank,
            });
        }
        Ok(files)
    }

    /// `language`'s `kind` query with what it inherits put in, or `None`
    /// when it has none. A required parent that's missing, or an
    /// inheritance loop, is an error.
    pub fn resolve(&self, language: &str, kind: &str) -> Result<Option<Resolved>, LangError> {
        self.resolve_seen(language, kind, &mut HashSet::new())
    }

    fn resolve_seen(
        &self,
        language: &str,
        kind: &str,
        seen: &mut HashSet<String>,
    ) -> Result<Option<Resolved>, LangError> {
        let files = self.files_for(language, kind)?;
        if files.is_empty() {
            return Ok(None);
        }
        if !seen.insert(language.to_owned()) {
            return Err(LangError::new(
                files[0].origin.clone(),
                Some(language),
                format!("its {kind} query inherits from itself"),
            ));
        }
        let mut ordered: Vec<&QueryFile> = files.iter().collect();
        // Stable: given files keep their order within a rank.
        ordered.sort_by_key(|f| (f.layer, f.rank));
        let (bases, extensions): (Vec<&QueryFile>, Vec<&QueryFile>) =
            ordered.into_iter().partition(|f| !modelines(&f.text).1);
        let base = bases.last().copied();
        let Some(first) = base.or_else(|| extensions.first().copied()) else {
            seen.remove(language);
            return Ok(None);
        };
        let mut text = String::new();
        if let Some(base) = base {
            for (parent, optional) in modelines(&base.text).0 {
                match self.resolve_seen(&parent, kind, seen)? {
                    Some(inherited) => {
                        text.push_str(&inherited.text);
                        text.push('\n');
                    }
                    None if optional => {}
                    None => {
                        seen.remove(language);
                        return Err(LangError::new(
                            base.origin.clone(),
                            Some(language),
                            format!("its {kind} query inherits from {parent}, which has none"),
                        ));
                    }
                }
            }
            text.push_str(&base.text);
            text.push('\n');
        }
        for extension in &extensions {
            text.push_str(&extension.text);
            text.push('\n');
        }
        seen.remove(language);
        Ok(Some(Resolved {
            text,
            layer: first.layer,
        }))
    }
}

/// Neovim's query folders, highest first as Neovim ranks them (its
/// runtime path): the user's own, the installed queries, then
/// nvim-treesitter's (its main branch keeps them in `runtime/queries`,
/// master in `queries`), and last `after/`, whose files normally extend.
/// `config` and `data` are Neovim's folders (`~/.config/nvim`,
/// `~/.local/share/nvim`).
pub fn neovim_query_folders(config: &Path, data: &Path) -> Vec<PathBuf> {
    vec![
        config.join("queries"),
        data.join("site/queries"),
        data.join("lazy/nvim-treesitter/runtime/queries"),
        data.join("lazy/nvim-treesitter/queries"),
        data.join("site/pack/packer/start/nvim-treesitter/queries"),
        config.join("after/queries"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str, layer: Layer, rank: usize) -> QueryFile {
        QueryFile {
            text: text.into(),
            origin: Origin::Builtin,
            layer,
            rank,
        }
    }

    #[test]
    fn modelines_are_read_from_the_top() {
        let (inherits, extends) =
            modelines("; inherits: ecma, (jsx)\n;; extends\n(x) @y\n; inherits: no");
        assert_eq!(
            inherits,
            [("ecma".to_owned(), false), ("jsx".to_owned(), true)]
        );
        assert!(extends);
        assert_eq!(modelines("(x) @y").0.len(), 0);
    }

    #[test]
    fn parents_come_first_and_optional_ones_may_be_missing() {
        let mut q = QuerySet::default();
        q.add("ecma", "highlights", file("(e) @e", Layer::Builtin, 0));
        q.add(
            "typescript",
            "highlights",
            file("; inherits: ecma,(jsx)\n(t) @t", Layer::Builtin, 0),
        );
        let ts = q
            .resolve("typescript", "highlights")
            .expect("ok")
            .expect("some");
        let e = ts.text.find("(e) @e").expect("parent");
        let t = ts.text.find("(t) @t").expect("own");
        assert!(e < t, "{}", ts.text);
        q.add(
            "broken",
            "highlights",
            file("; inherits: nothing\n(b) @b", Layer::Builtin, 0),
        );
        let err = q
            .resolve("broken", "highlights")
            .expect_err("missing parent");
        assert!(err.message.contains("nothing"));
        assert_eq!(q.resolve("python", "highlights"), Ok(None));
    }

    #[test]
    fn higher_layers_replace_and_extensions_add() {
        let mut q = QuerySet::default();
        q.add(
            "rust",
            "highlights",
            file("(a) @builtin", Layer::Builtin, 0),
        );
        q.add("rust", "highlights", file("(a) @neovim", Layer::Neovim, 1));
        q.add(
            "rust",
            "highlights",
            file("(a) @neovim_low", Layer::Neovim, 0),
        );
        let r = q.resolve("rust", "highlights").expect("ok").expect("some");
        assert!(
            r.text.contains("@neovim\n")
                && !r.text.contains("@builtin")
                && !r.text.contains("@neovim_low")
        );
        assert_eq!(r.layer, Layer::Neovim);
        q.add(
            "rust",
            "highlights",
            file("; extends\n(b) @mine", Layer::User, 0),
        );
        let r = q.resolve("rust", "highlights").expect("ok").expect("some");
        assert!(r.text.find("@neovim").expect("base") < r.text.find("@mine").expect("added"));
        assert_eq!(r.layer, Layer::Neovim, "the base's layer");
        q.add("rust", "highlights", file("(c) @user_base", Layer::User, 1));
        let r = q.resolve("rust", "highlights").expect("ok").expect("some");
        assert!(
            r.text.contains("@user_base")
                && r.text.contains("@mine")
                && !r.text.contains("@neovim\n")
        );
        assert_eq!(r.layer, Layer::User);
    }

    #[test]
    fn a_loop_is_an_error_not_a_hang() {
        let mut q = QuerySet::default();
        q.add(
            "a",
            "folds",
            file("; inherits: b\n(a) @fold", Layer::Builtin, 0),
        );
        q.add(
            "b",
            "folds",
            file("; inherits: a\n(b) @fold", Layer::Builtin, 0),
        );
        assert!(q.resolve("a", "folds").is_err());
    }

    #[test]
    fn folders_are_read_when_a_language_is_asked_for() {
        let root = std::env::temp_dir().join(format!("mnote-queries-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("nvim/lua")).expect("dir");
        std::fs::create_dir_all(root.join("packs/lua/queries")).expect("dir");
        std::fs::write(root.join("nvim/lua/highlights.scm"), "(x) @nvim").expect("write");
        std::fs::write(
            root.join("packs/lua/queries/highlights.scm"),
            "; extends\n(x) @mine",
        )
        .expect("write");
        let mut q = QuerySet::default();
        q.add_folder(root.join("nvim"), Layout::ByLanguage, Layer::Neovim, 0);
        q.add_folder(root.join("packs"), Layout::InPacks, Layer::User, 0);
        let r = q.resolve("lua", "highlights").expect("ok").expect("some");
        assert!(r.text.find("@nvim").expect("base") < r.text.find("@mine").expect("added"));
        assert!(q.resolve("lua", "folds").expect("ok").is_none());
        assert!(
            q.resolve("../lua", "highlights").expect("ok").is_none(),
            "no paths"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
