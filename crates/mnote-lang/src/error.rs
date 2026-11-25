//! `mnote-lang`'s errors: a problem in a language pack or a query, with
//! where it came from. None is fatal: a broken pack is reported and skipped,
//! and the languages that load still work.

use std::fmt;
use std::path::PathBuf;

/// Where a language pack (or a query) came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Built into mNote.
    Builtin,
    /// A file: a user's `language.toml` or a query file.
    File(PathBuf),
    /// Registered by a plugin (`mnote.lang.register`).
    Plugin(String),
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Builtin => f.write_str("built-in languages"),
            Origin::File(path) => write!(f, "{}", path.display()),
            Origin::Plugin(name) => write!(f, "plugin {name}"),
        }
    }
}

/// Something wrong with a language pack or one of its queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LangError {
    pub origin: Origin,
    /// The language it's about, when known.
    pub language: Option<String>,
    pub message: String,
}

impl LangError {
    pub fn new(origin: Origin, language: Option<&str>, message: impl Into<String>) -> Self {
        LangError {
            origin,
            language: language.map(str::to_owned),
            message: message.into(),
        }
    }
}

impl fmt::Display for LangError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.language {
            Some(lang) => write!(f, "{} ({lang}): {}", self.origin, self.message),
            None => write!(f, "{}: {}", self.origin, self.message),
        }
    }
}

impl std::error::Error for LangError {}
