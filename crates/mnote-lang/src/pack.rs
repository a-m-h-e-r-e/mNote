//! Language packs (ADR-0003): what mNote knows about a language, as data.
//!
//! A pack is a `language.toml` (and a `queries/` folder, see
//! [`crate::queries`]):
//!
//! ```toml
//! name = "rust"
//! aliases = ["rs"]                 # names code fences use
//! extensions = ["rs"]
//! filenames = []                   # exact names, like "Makefile"
//! shebangs = []                    # interpreters, like "python3"
//! first_line = ""                  # a pattern, like "^#!.*\\bpython"
//! grammar = "rust"                 # defaults to the name
//! comment = { line = "//", block = ["/*", "*/"] }
//! indent = { unit = "    ", tab_width = 4 }
//! brackets = [["(", ")"], ["[", "]"], ["{", "}"]]
//! auto_pairs = [["(", ")"], ["\"", "\""]]
//! word_chars = "_"
//! roots = ["Cargo.toml", ".git"]
//! servers = ["rust-analyzer"]
//! formatter = ["rustfmt", "--emit=stdout"]
//! ```
//!
//! Every field but `name` is optional: packs from several places merge field
//! by field, the later one winning where it sets a field ([`PackSpec::merge`]),
//! and what nobody set takes a default ([`Pack::resolve`]).

use std::collections::BTreeMap;

use crate::error::{LangError, Origin};

/// A language's comment tokens.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comment {
    pub line: Option<String>,
    pub block: Option<(String, String)>,
}

/// A language's indent: the text one level adds, and a tab's width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indent {
    pub unit: String,
    pub tab_width: u32,
}

impl Default for Indent {
    fn default() -> Self {
        Indent {
            unit: "    ".into(),
            tab_width: 4,
        }
    }
}

/// One source's description of a language: only what it sets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackSpec {
    pub name: String,
    pub aliases: Option<Vec<String>>,
    pub extensions: Option<Vec<String>>,
    pub filenames: Option<Vec<String>>,
    pub shebangs: Option<Vec<String>>,
    pub first_line: Option<String>,
    pub grammar: Option<String>,
    pub comment: Option<Comment>,
    pub indent: Option<Indent>,
    pub brackets: Option<Vec<(String, String)>>,
    pub auto_pairs: Option<Vec<(String, String)>>,
    pub word_chars: Option<String>,
    pub roots: Option<Vec<String>>,
    pub servers: Option<Vec<String>>,
    pub formatter: Option<Vec<String>>,
}

/// A language, every field settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pack {
    pub name: String,
    pub aliases: Vec<String>,
    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
    pub shebangs: Vec<String>,
    pub first_line: Option<String>,
    pub grammar: String,
    pub comment: Comment,
    pub indent: Indent,
    pub brackets: Vec<(String, String)>,
    pub auto_pairs: Vec<(String, String)>,
    pub word_chars: String,
    pub roots: Vec<String>,
    pub servers: Vec<String>,
    pub formatter: Option<Vec<String>>,
    /// Its queries by kind (`highlights`), resolved.
    pub queries: BTreeMap<String, crate::queries::Resolved>,
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(a, b)| ((*a).into(), (*b).into()))
        .collect()
}

impl Pack {
    /// `spec` with defaults for what it leaves out.
    pub fn resolve(spec: PackSpec) -> Pack {
        let brackets = spec
            .brackets
            .unwrap_or_else(|| pairs(&[("(", ")"), ("[", "]"), ("{", "}")]));
        let auto_pairs = spec.auto_pairs.unwrap_or_else(|| {
            let mut all = brackets.clone();
            all.extend(pairs(&[("\"", "\""), ("'", "'")]));
            all
        });
        Pack {
            grammar: spec.grammar.unwrap_or_else(|| spec.name.clone()),
            aliases: spec.aliases.unwrap_or_default(),
            extensions: spec.extensions.unwrap_or_default(),
            filenames: spec.filenames.unwrap_or_default(),
            shebangs: spec.shebangs.unwrap_or_default(),
            first_line: spec.first_line.filter(|p| !p.is_empty()),
            comment: spec.comment.unwrap_or_default(),
            indent: spec.indent.unwrap_or_default(),
            brackets,
            auto_pairs,
            word_chars: spec.word_chars.unwrap_or_else(|| "_".into()),
            roots: spec.roots.unwrap_or_else(|| vec![".git".into()]),
            servers: spec.servers.unwrap_or_default(),
            formatter: spec.formatter,
            queries: BTreeMap::new(),
            name: spec.name,
        }
    }

    /// Whether `name` is this language's name or one of its aliases.
    pub fn named(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
            || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(name))
    }
}

impl PackSpec {
    /// `higher` over `self`: each field `higher` sets wins.
    pub fn merge(self, higher: PackSpec) -> PackSpec {
        macro_rules! pick {
            ($($field:ident),*) => {
                PackSpec {
                    name: self.name,
                    $($field: higher.$field.or(self.$field),)*
                }
            };
        }
        pick!(
            aliases, extensions, filenames, shebangs, first_line, grammar, comment, indent,
            brackets, auto_pairs, word_chars, roots, servers, formatter
        )
    }

    /// Reads one `language.toml`. `origin` says where it came from, for
    /// errors.
    pub fn parse(text: &str, origin: &Origin) -> Result<PackSpec, LangError> {
        let table: toml::Table = text.parse().map_err(|e: toml::de::Error| {
            LangError::new(origin.clone(), None, e.message().to_owned())
        })?;
        Self::from_table(&table, origin)
    }

    /// A pack from an already-read table (a section of the built-in file, or
    /// a table a plugin passed).
    pub fn from_table(table: &toml::Table, origin: &Origin) -> Result<PackSpec, LangError> {
        let name = match table.get("name") {
            Some(toml::Value::String(name)) if !name.trim().is_empty() => name.trim().to_owned(),
            _ => {
                return Err(LangError::new(
                    origin.clone(),
                    None,
                    "a language needs a name",
                ));
            }
        };
        let fail = |field: &str, wants: &str| {
            LangError::new(
                origin.clone(),
                Some(&name),
                format!("{field} should be {wants}"),
            )
        };
        let strings = |field: &str| -> Result<Option<Vec<String>>, LangError> {
            match table.get(field) {
                None => Ok(None),
                Some(toml::Value::Array(items)) => items
                    .iter()
                    .map(|v| v.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
                    .map(Some)
                    .ok_or_else(|| fail(field, "a list of strings")),
                Some(_) => Err(fail(field, "a list of strings")),
            }
        };
        let string = |field: &str| -> Result<Option<String>, LangError> {
            match table.get(field) {
                None => Ok(None),
                Some(toml::Value::String(s)) => Ok(Some(s.clone())),
                Some(_) => Err(fail(field, "a string")),
            }
        };
        let pair_list = |field: &str| -> Result<Option<Vec<(String, String)>>, LangError> {
            let Some(value) = table.get(field) else {
                return Ok(None);
            };
            let bad = || fail(field, "a list of pairs, like [[\"(\", \")\"]]");
            let toml::Value::Array(items) = value else {
                return Err(bad());
            };
            items
                .iter()
                .map(|item| match item.as_array().map(Vec::as_slice) {
                    Some([toml::Value::String(a), toml::Value::String(b)]) => {
                        Ok((a.clone(), b.clone()))
                    }
                    _ => Err(bad()),
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some)
        };
        let comment = match table.get("comment") {
            None => None,
            Some(toml::Value::Table(t)) => {
                let line = t
                    .get("line")
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned);
                let block = match t
                    .get("block")
                    .and_then(toml::Value::as_array)
                    .map(Vec::as_slice)
                {
                    None => None,
                    Some([toml::Value::String(a), toml::Value::String(b)]) => {
                        Some((a.clone(), b.clone()))
                    }
                    Some(_) => {
                        return Err(fail("comment.block", "two strings, like [\"/*\", \"*/\"]"));
                    }
                };
                Some(Comment { line, block })
            }
            Some(_) => return Err(fail("comment", "a table, like { line = \"//\" }")),
        };
        let indent = match table.get("indent") {
            None => None,
            Some(toml::Value::Table(t)) => {
                let default = Indent::default();
                let unit = match t.get("unit") {
                    None => default.unit,
                    Some(toml::Value::String(u))
                        if u.chars().all(|c| c == ' ' || c == '\t') && !u.is_empty() =>
                    {
                        u.clone()
                    }
                    Some(_) => return Err(fail("indent.unit", "spaces or a tab, like \"    \"")),
                };
                let tab_width = match t.get("tab_width") {
                    None => default.tab_width,
                    Some(toml::Value::Integer(n)) if (1..=16).contains(n) => *n as u32,
                    Some(_) => return Err(fail("indent.tab_width", "a whole number from 1 to 16")),
                };
                Some(Indent { unit, tab_width })
            }
            Some(_) => return Err(fail("indent", "a table, like { unit = \"    \" }")),
        };
        let first_line = string("first_line")?;
        if let Some(pattern) = first_line.as_deref().filter(|p| !p.is_empty())
            && regex::Regex::new(pattern).is_err()
        {
            return Err(fail("first_line", "a valid pattern"));
        }
        let known = [
            "name",
            "aliases",
            "extensions",
            "filenames",
            "shebangs",
            "first_line",
            "grammar",
            "comment",
            "indent",
            "brackets",
            "auto_pairs",
            "word_chars",
            "roots",
            "servers",
            "formatter",
        ];
        if let Some(unknown) = table.keys().find(|k| !known.contains(&k.as_str())) {
            return Err(LangError::new(
                origin.clone(),
                Some(&name),
                format!("unknown field {unknown}"),
            ));
        }
        Ok(PackSpec {
            aliases: strings("aliases")?,
            extensions: strings("extensions")?,
            filenames: strings("filenames")?,
            shebangs: strings("shebangs")?,
            first_line,
            grammar: string("grammar")?,
            comment,
            indent,
            brackets: pair_list("brackets")?,
            auto_pairs: pair_list("auto_pairs")?,
            word_chars: string("word_chars")?,
            roots: strings("roots")?,
            servers: strings("servers")?,
            formatter: strings("formatter")?,
            name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST: &str = r#"
name = "rust"
aliases = ["rs"]
extensions = ["rs"]
comment = { line = "//", block = ["/*", "*/"] }
indent = { unit = "    ", tab_width = 4 }
brackets = [["(", ")"], ["{", "}"]]
roots = ["Cargo.toml", ".git"]
servers = ["rust-analyzer"]
formatter = ["rustfmt", "--emit=stdout"]
"#;

    #[test]
    fn a_pack_reads_every_field() {
        let spec = PackSpec::parse(RUST, &Origin::Builtin).expect("valid");
        assert_eq!(spec.name, "rust");
        assert_eq!(
            spec.comment.as_ref().and_then(|c| c.line.as_deref()),
            Some("//")
        );
        assert_eq!(
            spec.comment.as_ref().and_then(|c| c.block.clone()),
            Some(("/*".into(), "*/".into()))
        );
        assert_eq!(spec.brackets.as_ref().map(Vec::len), Some(2));
        let pack = Pack::resolve(spec);
        assert_eq!(pack.grammar, "rust", "the grammar defaults to the name");
        assert!(pack.named("RS") && pack.named("rust") && !pack.named("go"));
        assert_eq!(pack.word_chars, "_");
        assert_eq!(pack.auto_pairs.len(), 4, "the brackets and two quotes");
        assert_eq!(
            pack.formatter.as_deref(),
            Some(&["rustfmt".to_owned(), "--emit=stdout".into()][..])
        );
    }

    #[test]
    fn mistakes_say_which_field() {
        let err = |text: &str| {
            PackSpec::parse(text, &Origin::Builtin)
                .expect_err("invalid")
                .to_string()
        };
        assert!(err("extensions = [\"x\"]").contains("needs a name"));
        assert!(err("name = \"x\"\nextensions = \"rs\"").contains("extensions should be a list"));
        assert!(err("name = \"x\"\nindent = { unit = \"ab\" }").contains("indent.unit"));
        assert!(err("name = \"x\"\nfirst_line = \"(\"").contains("first_line"));
        assert!(err("name = \"x\"\nbrackets = [[\"(\"]]").contains("brackets"));
        assert!(err("name = \"x\"\ncolour = 1").contains("unknown field colour"));
        assert!(err("name = ").contains("built-in"), "toml errors say where");
    }

    #[test]
    fn later_fields_win_and_the_rest_stay() {
        let base = PackSpec::parse(RUST, &Origin::Builtin).expect("valid");
        let user = PackSpec::parse(
            "name = \"rust\"\nindent = { unit = \"  \" }\nservers = []",
            &Origin::Builtin,
        )
        .expect("valid");
        let pack = Pack::resolve(base.merge(user));
        assert_eq!(pack.indent.unit, "  ");
        assert!(pack.servers.is_empty(), "an empty list is a choice too");
        assert_eq!(pack.extensions, ["rs"], "untouched fields stay");
        assert_eq!(pack.roots, ["Cargo.toml", ".git"]);
    }
}
