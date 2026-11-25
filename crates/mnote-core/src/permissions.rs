//! What the user allowed to run (ADR-0012): plugins, grammar libraries,
//! language-server commands, and later agent tools and context servers.
//!
//! Each permission names a subject (its kind and name) and a fingerprint of
//! exactly what was allowed: when the code changes, the fingerprint does,
//! and mNote asks again. A permission is for once (not kept), this session
//! (kept in memory), or always (kept in `~/.config/mnote/permissions`, one
//! line each, written atomically).
//!
//! The first time the file is made, plugins allowed under the old
//! `plugins/.trusted` file are carried over, so nothing is asked again.

use std::fmt;
use std::path::{Path, PathBuf};

/// What kind of thing a permission is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// A plugin's code (plugin.toml and init.lua).
    Plugin,
    /// A tree-sitter grammar library loaded into mNote.
    Grammar,
    /// A language server's command.
    Server,
    /// An agent's tool (later).
    AgentTool,
    /// A context server (later).
    ContextServer,
}

impl Kind {
    const ALL: [Kind; 5] = [
        Kind::Plugin,
        Kind::Grammar,
        Kind::Server,
        Kind::AgentTool,
        Kind::ContextServer,
    ];

    /// Its name in the permissions file and in messages.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Plugin => "plugin",
            Kind::Grammar => "grammar",
            Kind::Server => "server",
            Kind::AgentTool => "agent-tool",
            Kind::ContextServer => "context-server",
        }
    }

    fn from_name(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.name() == name)
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// How long a permission lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// This time only; asked again next time.
    Once,
    /// Until mNote quits.
    Session,
    /// Kept, until revoked or the code changes.
    Always,
}

/// One permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub kind: Kind,
    pub name: String,
    pub fingerprint: u64,
    pub scope: Scope,
}

/// The permissions, kept and for this session.
#[derive(Debug, Default)]
pub struct Permissions {
    grants: Vec<Grant>,
    /// Where `Always` grants are kept; `None` keeps them in memory (tests,
    /// no config folder).
    path: Option<PathBuf>,
}

impl Permissions {
    /// The file permissions are kept in: next to config.toml.
    pub fn default_path() -> Option<PathBuf> {
        Some(crate::config::Config::path()?.parent()?.join("permissions"))
    }

    /// Reads the permissions kept at `path`. With no file yet, plugins
    /// allowed in `legacy_trust` (the old `plugins/.trusted` file) carry
    /// over, and the file is made.
    pub fn load(path: PathBuf, legacy_trust: Option<&Path>) -> Self {
        let mut permissions = Permissions {
            grants: Vec::new(),
            path: Some(path.clone()),
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => permissions.grants = parse(&text),
            Err(_) => {
                let old = legacy_trust
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .unwrap_or_default();
                permissions.grants = migrate(&old);
                if !permissions.grants.is_empty() {
                    let _ = permissions.save();
                }
            }
        }
        permissions
    }

    /// Permissions kept only in memory.
    pub fn in_memory() -> Self {
        Permissions::default()
    }

    /// Whether `kind` `name` at this `fingerprint` is allowed (for this
    /// session or always). A grant for other code doesn't count.
    pub fn allowed(&self, kind: Kind, name: &str, fingerprint: u64) -> bool {
        self.grants
            .iter()
            .any(|g| g.kind == kind && g.name == name && g.fingerprint == fingerprint)
    }

    /// Records the user's answer. `Once` keeps nothing; `Session` and
    /// `Always` replace any older grant for the same subject; `Always` is
    /// written to the file.
    pub fn allow(&mut self, kind: Kind, name: &str, fingerprint: u64, scope: Scope) {
        if scope == Scope::Once {
            return;
        }
        self.grants.retain(|g| !(g.kind == kind && g.name == name));
        self.grants.push(Grant {
            kind,
            name: name.to_owned(),
            fingerprint,
            scope,
        });
        if scope == Scope::Always {
            let _ = self.save();
        }
    }

    /// Takes back every grant for `kind` `name`; whether there was one.
    pub fn revoke(&mut self, kind: Kind, name: &str) -> bool {
        let before = self.grants.len();
        let kept = self
            .grants
            .iter()
            .any(|g| g.kind == kind && g.name == name && g.scope == Scope::Always);
        self.grants.retain(|g| !(g.kind == kind && g.name == name));
        if kept {
            let _ = self.save();
        }
        self.grants.len() != before
    }

    /// Every grant, by kind then name.
    pub fn grants(&self) -> Vec<Grant> {
        let mut grants = self.grants.clone();
        grants.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));
        grants
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut text = String::from(
            "# What mNote may run: kind, fingerprint, name. Delete a line to be asked again.\n",
        );
        for g in self.grants().iter().filter(|g| g.scope == Scope::Always) {
            text.push_str(&format!("{} {:016x} {}\n", g.kind, g.fingerprint, g.name));
        }
        crate::config::write_file(path, &text)
    }
}

/// The permissions file's lines: `kind fingerprint name` (the name last, so
/// it may hold spaces). Lines it can't read are skipped.
fn parse(text: &str) -> Vec<Grant> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|line| {
            let mut parts = line.trim().splitn(3, ' ');
            let kind = Kind::from_name(parts.next()?)?;
            let fingerprint = u64::from_str_radix(parts.next()?, 16).ok()?;
            let name = parts.next()?.trim();
            (!name.is_empty()).then(|| Grant {
                kind,
                name: name.to_owned(),
                fingerprint,
                scope: Scope::Always,
            })
        })
        .collect()
}

/// Plugins from the old `plugins/.trusted` lines (`name fingerprint`).
fn migrate(old: &str) -> Vec<Grant> {
    old.lines()
        .filter_map(|line| {
            let (name, print) = line.trim().rsplit_once(' ')?;
            Some(Grant {
                kind: Kind::Plugin,
                name: name.trim().to_owned(),
                fingerprint: u64::from_str_radix(print, 16).ok()?,
                scope: Scope::Always,
            })
        })
        .collect()
}

/// A fingerprint of some files' bytes, in order (FNV-1a: stable across runs
/// and builds). A missing file counts as empty.
pub fn fingerprint_files(paths: &[&Path]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for path in paths {
        for byte in std::fs::read(path).unwrap_or_default() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0xff;
    }
    hash
}

/// A fingerprint of a string (a server's command line, say).
pub fn fingerprint_text(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mnote-perm-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn a_grant_is_for_one_subject_at_one_fingerprint() {
        let mut p = Permissions::in_memory();
        p.allow(Kind::Plugin, "tasks", 7, Scope::Always);
        assert!(p.allowed(Kind::Plugin, "tasks", 7));
        assert!(!p.allowed(Kind::Plugin, "tasks", 8), "its code changed");
        assert!(!p.allowed(Kind::Grammar, "tasks", 7), "another kind");
        assert!(!p.allowed(Kind::Plugin, "other", 7));
        // Allowing new code replaces the old grant.
        p.allow(Kind::Plugin, "tasks", 8, Scope::Always);
        assert!(p.allowed(Kind::Plugin, "tasks", 8) && !p.allowed(Kind::Plugin, "tasks", 7));
        assert_eq!(p.grants().len(), 1);
    }

    #[test]
    fn scopes_decide_what_is_kept() {
        let dir = temp("scopes");
        let file = dir.join("permissions");
        let mut p = Permissions::load(file.clone(), None);
        p.allow(Kind::Plugin, "once", 1, Scope::Once);
        p.allow(Kind::Server, "rust-analyzer", 2, Scope::Session);
        p.allow(Kind::Grammar, "zig", 3, Scope::Always);
        assert!(!p.allowed(Kind::Plugin, "once", 1), "once isn't kept");
        assert!(p.allowed(Kind::Server, "rust-analyzer", 2));
        // A new session reads only what was kept always.
        let next = Permissions::load(file, None);
        assert!(!next.allowed(Kind::Server, "rust-analyzer", 2));
        assert!(next.allowed(Kind::Grammar, "zig", 3));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn revoking_forgets_and_saves() {
        let dir = temp("revoke");
        let file = dir.join("permissions");
        let mut p = Permissions::load(file.clone(), None);
        p.allow(Kind::Plugin, "a b", 1, Scope::Always);
        p.allow(Kind::Plugin, "c", 2, Scope::Always);
        assert!(p.revoke(Kind::Plugin, "a b"));
        assert!(!p.revoke(Kind::Plugin, "a b"), "already gone");
        let next = Permissions::load(file, None);
        assert!(!next.allowed(Kind::Plugin, "a b", 1));
        assert!(next.allowed(Kind::Plugin, "c", 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugins_trusted_before_carry_over_once() {
        let dir = temp("migrate");
        let file = dir.join("permissions");
        let legacy = dir.join(".trusted");
        std::fs::write(
            &legacy,
            "tasks 0000000000000007\nwordgoal 00000000000000ff\nbroken\n",
        )
        .expect("write");
        let p = Permissions::load(file.clone(), Some(&legacy));
        assert!(p.allowed(Kind::Plugin, "tasks", 7));
        assert!(p.allowed(Kind::Plugin, "wordgoal", 255));
        assert_eq!(p.grants().len(), 2, "unreadable lines are skipped");
        assert!(file.exists(), "the new file is made");
        // Once the file exists, the old one isn't read again: a revoked
        // plugin stays revoked.
        let mut p = Permissions::load(file.clone(), Some(&legacy));
        p.revoke(Kind::Plugin, "tasks");
        let again = Permissions::load(file, Some(&legacy));
        assert!(!again.allowed(Kind::Plugin, "tasks", 7));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_skips_what_it_cannot_read() {
        let grants = parse(
            "# a comment\nplugin 000000000000000a tasks\nnope 1 x\nserver zz y\ngrammar 0b\n",
        );
        assert_eq!(grants.len(), 1);
        assert_eq!(
            (
                grants[0].kind,
                grants[0].name.as_str(),
                grants[0].fingerprint
            ),
            (Kind::Plugin, "tasks", 10)
        );
    }

    #[test]
    fn fingerprints_follow_content() {
        let dir = temp("print");
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::write(&a, "one").expect("write");
        std::fs::write(&b, "two").expect("write");
        let first = fingerprint_files(&[&a, &b]);
        assert_eq!(first, fingerprint_files(&[&a, &b]), "stable");
        assert_ne!(first, fingerprint_files(&[&b, &a]), "order counts");
        std::fs::write(&b, "two!").expect("write");
        assert_ne!(first, fingerprint_files(&[&a, &b]));
        assert_ne!(
            fingerprint_text("rust-analyzer"),
            fingerprint_text("rust-analyzer --x")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
