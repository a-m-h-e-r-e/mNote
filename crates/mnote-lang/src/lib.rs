//! mNote's language layer: what a file's language is, its tree-sitter
//! grammar, highlighting, and the background parser that keeps a
//! document's syntax tree up to date.
//!
//! It sits below `mnote-core` and needs only a rope and tree-sitter, so it
//! has no UI dependencies and is tested on its own. The design it grows
//! into is `docs/architecture/code-support.md` (ADR-0001).

pub mod code;
pub mod coords;
pub mod editing;
pub mod error;
pub mod filetype;
pub mod grammar;
pub mod highlight;
pub mod languages;
pub mod pack;
pub mod predicates;
pub mod queries;
pub mod scopes;
pub mod syntax;
pub mod view;

pub use error::LangError;
