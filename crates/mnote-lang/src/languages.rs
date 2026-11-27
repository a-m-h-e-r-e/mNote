//! The language registry (ADR-0003): every language pack, merged from where
//! packs come from, and their queries.
//!
//! Packs merge field by field, later sources winning:
//!
//! 1. built in (`languages.toml`, embedded);
//! 2. the user's `~/.config/mnote/languages/<name>/language.toml`;
//! 3. plugins (`mnote.lang.register`).
//!
//! Queries come from the built-in grammars, Neovim's query folders (when
//! reused) and the user's `languages/<name>/queries/`, and are read and
//! resolved the first time a language needs them (see [`crate::queries`]).
//!
//! A broken user pack or query is reported and skipped: the built-in pack
//! for that language still works. The registry is shared by every thread
//! and swapped whole on [`load`], so readers never wait on a reload.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use crate::error::{LangError, Origin};
use crate::pack::{Pack, PackSpec};
use crate::queries::{Layer, Layout, QueryFile, QuerySet, Resolved};

/// Where packs and queries come from, besides those built in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sources {
    /// The user's language packs (`~/.config/mnote/languages`).
    pub user: Option<PathBuf>,
    /// Neovim's config and data folders, when its queries are reused.
    pub neovim: Option<(PathBuf, PathBuf)>,
}

impl Sources {
    /// The usual places: `languages/` next to config.toml, and Neovim's
    /// folders (`$XDG_CONFIG_HOME/nvim`, `$XDG_DATA_HOME/nvim`) when
    /// `reuse_neovim`.
    pub fn standard(config_dir: Option<&Path>, reuse_neovim: bool) -> Sources {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let xdg = |var: &str, fallback: &str| {
            std::env::var_os(var)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|h| h.join(fallback)))
        };
        let neovim = reuse_neovim
            .then(|| {
                Some((
                    xdg("XDG_CONFIG_HOME", ".config")?.join("nvim"),
                    xdg("XDG_DATA_HOME", ".local/share")?.join("nvim"),
                ))
            })
            .flatten();
        Sources {
            user: config_dir.map(|d| d.join("languages")),
            neovim,
        }
    }
}

struct Registry {
    packs: Vec<Pack>,
    queries: QuerySet,
    /// Resolved queries by language and kind, filled as they're asked for.
    resolved: Mutex<HashMap<(String, String), Option<Resolved>>>,
    /// Compiled editing queries by language and kind, the same way.
    editing: Mutex<EditingCache>,
}

type EditingCache = HashMap<(String, String), Option<Arc<crate::editing::EditingQuery>>>;

fn cell() -> &'static RwLock<Arc<Registry>> {
    static REGISTRY: OnceLock<RwLock<Arc<Registry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let (registry, _) = build(&Sources::default(), &[]);
        RwLock::new(Arc::new(registry))
    })
}

fn current() -> Arc<Registry> {
    match cell().read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// A language a plugin registered: its pack and its queries by kind.
type PluginPack = (PackSpec, Vec<(String, String)>);

/// What plugins registered, kept across reloads.
fn plugin_specs() -> &'static Mutex<Vec<PluginPack>> {
    static SPECS: OnceLock<Mutex<Vec<PluginPack>>> = OnceLock::new();
    SPECS.get_or_init(Mutex::default)
}

/// The sources of the last [`load`], for reloads after a plugin registers.
fn last_sources() -> &'static Mutex<Sources> {
    static SOURCES: OnceLock<Mutex<Sources>> = OnceLock::new();
    SOURCES.get_or_init(Mutex::default)
}

/// Problems found since they were last taken (from loading, or from a
/// query read later).
fn problems() -> &'static Mutex<Vec<LangError>> {
    static PROBLEMS: OnceLock<Mutex<Vec<LangError>>> = OnceLock::new();
    PROBLEMS.get_or_init(Mutex::default)
}

fn report(new: impl IntoIterator<Item = LangError>) {
    if let Ok(mut list) = problems().lock() {
        for p in new {
            if !list.contains(&p) {
                list.push(p);
            }
        }
    }
}

/// Keeps one problem for [`take_problems`] (once, however often it's
/// found).
pub(crate) fn report_one(problem: LangError) {
    report([problem]);
}

/// The problems found since the last call, once each.
pub fn take_problems() -> Vec<LangError> {
    problems()
        .lock()
        .map(|mut l| std::mem::take(&mut *l))
        .unwrap_or_default()
}

/// Reads the packs from `sources` (and the built-in ones) and swaps the
/// registry; returns what was wrong (also kept for [`take_problems`]).
pub fn load(sources: &Sources) -> Vec<LangError> {
    if let Ok(mut last) = last_sources().lock() {
        *last = sources.clone();
    }
    let plugins = plugin_specs().lock().map(|p| p.clone()).unwrap_or_default();
    let (registry, found) = build(sources, &plugins);
    if let Ok(mut cell) = cell().write() {
        *cell = Arc::new(registry);
    }
    crate::code::rebuild();
    report(found.clone());
    found
}

/// Adds (or overrides fields of) a language from a plugin: `spec`, and
/// queries by kind as text. Kept across reloads.
pub fn register(spec: PackSpec, queries: Vec<(String, String)>) {
    if let Ok(mut specs) = plugin_specs().lock() {
        specs.push((spec, queries));
    }
    let sources = last_sources().lock().map(|s| s.clone()).unwrap_or_default();
    load(&sources);
}

fn build(
    sources: &Sources,
    plugins: &[(PackSpec, Vec<(String, String)>)],
) -> (Registry, Vec<LangError>) {
    let mut found = Vec::new();
    let mut specs: BTreeMap<String, PackSpec> = BTreeMap::new();
    for spec in builtin_specs(&mut found) {
        specs.insert(spec.name.clone(), spec);
    }
    let mut queries = QuerySet::default();
    for (language, kind, text) in crate::code::builtin_queries() {
        queries.add(
            &language,
            kind,
            QueryFile {
                text,
                origin: Origin::Builtin,
                layer: Layer::Builtin,
                rank: 0,
            },
        );
    }
    if let Some((config, data)) = &sources.neovim {
        let folders = crate::queries::neovim_query_folders(config, data);
        let count = folders.len();
        for (i, folder) in folders.into_iter().enumerate() {
            queries.add_folder(folder, Layout::ByLanguage, Layer::Neovim, count - i);
        }
    }
    if let Some(user) = &sources.user {
        for (name, path) in pack_folders(user) {
            let file = path.join("language.toml");
            let origin = Origin::File(file.clone());
            match std::fs::read_to_string(&file)
                .map_err(|e| LangError::new(origin.clone(), Some(&name), e.to_string()))
                .and_then(|text| PackSpec::parse(&text, &origin))
            {
                Ok(spec) => merge_into(&mut specs, spec),
                Err(err) => found.push(err),
            }
        }
        queries.add_folder(user.clone(), Layout::InPacks, Layer::User, 0);
    }
    for (spec, texts) in plugins {
        let name = spec.name.clone();
        merge_into(&mut specs, spec.clone());
        for (kind, text) in texts {
            queries.add(
                &name,
                kind,
                QueryFile {
                    text: text.clone(),
                    origin: Origin::Plugin(name.clone()),
                    layer: Layer::User,
                    rank: 1,
                },
            );
        }
    }
    let packs = specs.into_values().map(Pack::resolve).collect();
    let registry = Registry {
        packs,
        queries,
        resolved: Mutex::new(HashMap::new()),
        editing: Mutex::new(HashMap::new()),
    };
    (registry, found)
}

fn merge_into(specs: &mut BTreeMap<String, PackSpec>, spec: PackSpec) {
    let merged = match specs.remove(&spec.name) {
        Some(lower) => lower.merge(spec),
        None => spec,
    };
    specs.insert(merged.name.clone(), merged);
}

/// The built-in packs, from the embedded `languages.toml`.
fn builtin_specs(found: &mut Vec<LangError>) -> Vec<PackSpec> {
    const BUILTIN: &str = include_str!("../languages.toml");
    let table: toml::Table = match BUILTIN.parse() {
        Ok(t) => t,
        Err(e) => {
            let e: toml::de::Error = e;
            found.push(LangError::new(
                Origin::Builtin,
                None,
                e.message().to_owned(),
            ));
            return Vec::new();
        }
    };
    table
        .values()
        .filter_map(|v| match v {
            toml::Value::Table(t) => match PackSpec::from_table(t, &Origin::Builtin) {
                Ok(spec) => Some(spec),
                Err(err) => {
                    found.push(err);
                    None
                }
            },
            _ => None,
        })
        .collect()
}

/// The pack folders in `dir`: those with a `language.toml`.
fn pack_folders(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut found: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().join("language.toml").is_file())
                .filter_map(|e| {
                    let name = e.file_name().to_str()?.to_owned();
                    (!name.starts_with('.')).then(|| (name, e.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

/// Every language, by name.
pub fn packs() -> Vec<Pack> {
    current().packs.clone()
}

/// The language called `name`, or one of its aliases.
pub fn pack(name: &str) -> Option<Pack> {
    current().packs.iter().find(|p| p.named(name)).cloned()
}

/// `language`'s `kind` query (`highlights`), with what it inherits; `None`
/// when it has none. Read and resolved once; a problem is reported (see
/// [`take_problems`]) and the language goes on without that query.
pub fn query(language: &str, kind: &str) -> Option<Resolved> {
    let registry = current();
    let key = (language.to_owned(), kind.to_owned());
    if let Ok(cache) = registry.resolved.lock()
        && let Some(hit) = cache.get(&key)
    {
        return hit.clone();
    }
    let resolved = match registry.queries.resolve(language, kind) {
        Ok(r) => r,
        Err(err) => {
            report([err]);
            None
        }
    };
    if let Ok(mut cache) = registry.resolved.lock() {
        cache.insert(key, resolved.clone());
    }
    resolved
}

/// `language`'s editing query of `kind` as text (see [`crate::editing`]):
/// for a built-in grammar without Neovim's files, which don't fit it.
pub fn editing_query_text(language: &str, kind: &str) -> Option<Resolved> {
    let registry = current();
    let resolved = if crate::code::takes_neovim_queries(language) {
        registry.queries.resolve(language, kind)
    } else {
        registry
            .queries
            .resolve_without(language, kind, Layer::Neovim)
    };
    match resolved {
        Ok(r) => r,
        Err(err) => {
            report([err]);
            None
        }
    }
}

/// `language`'s editing query of `kind`, compiled once per registry.
pub(crate) fn editing_query(
    language: &str,
    kind: &str,
) -> Option<Arc<crate::editing::EditingQuery>> {
    let registry = current();
    let key = (language.to_owned(), kind.to_owned());
    if let Ok(cache) = registry.editing.lock()
        && let Some(hit) = cache.get(&key)
    {
        return hit.clone();
    }
    let compiled = editing_query_text(language, kind)
        .and_then(|r| crate::editing::compile(language, kind, &r.text))
        .map(Arc::new);
    if let Ok(mut cache) = registry.editing.lock() {
        cache.insert(key, compiled.clone());
    }
    compiled
}

/// The language of a file: by its exact name, then its extension, then its
/// first line (`#!/usr/bin/env python3`, or a pack's `first_line` pattern).
pub fn detect(path: &Path, first_line: Option<&str>) -> Option<Pack> {
    let registry = current();
    let name = path.file_name().and_then(|n| n.to_str());
    if let Some(name) = name
        && let Some(p) = registry
            .packs
            .iter()
            .find(|p| p.filenames.iter().any(|f| f == name))
    {
        return Some(p.clone());
    }
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && let Some(p) = registry
            .packs
            .iter()
            .find(|p| p.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)))
    {
        return Some(p.clone());
    }
    let line = first_line?.trim_end();
    if let Some(interpreter) = shebang(line)
        && let Some(p) = registry.packs.iter().find(|p| {
            p.shebangs
                .iter()
                .any(|s| interpreter == *s || versioned(&interpreter, s))
        })
    {
        return Some(p.clone());
    }
    registry
        .packs
        .iter()
        .find(|p| {
            p.first_line
                .as_deref()
                .and_then(|pattern| regex::Regex::new(pattern).ok())
                .is_some_and(|re| re.is_match(line))
        })
        .cloned()
}

/// The program a `#!` line runs: `/usr/bin/python3` and
/// `/usr/bin/env -S python3 -u` both give `python3`.
fn shebang(line: &str) -> Option<String> {
    let rest = line.strip_prefix("#!")?.trim();
    let mut words = rest.split_whitespace();
    let first = words.next()?;
    let program = if first.rsplit('/').next() == Some("env") {
        words.find(|w| !w.starts_with('-'))?
    } else {
        first
    };
    program.rsplit('/').next().map(str::to_owned)
}

/// `python3.12` runs `python3`; `node20` runs `node`.
fn versioned(interpreter: &str, name: &str) -> bool {
    interpreter.strip_prefix(name).is_some_and(|rest| {
        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '.')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test owns the shared registry's reloads, so tests running at
    /// once don't see each other's packs.
    #[test]
    fn packs_merge_detect_and_survive_mistakes() {
        let root = std::env::temp_dir().join(format!("mnote-langs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let user = root.join("languages");
        std::fs::create_dir_all(user.join("rust")).expect("dir");
        std::fs::create_dir_all(user.join("broken")).expect("dir");
        std::fs::create_dir_all(user.join("python/queries")).expect("dir");
        std::fs::create_dir_all(user.join("nim")).expect("dir");
        std::fs::write(
            user.join("rust/language.toml"),
            "name = \"rust\"\nindent = { unit = \"  \" }",
        )
        .expect("write");
        std::fs::write(
            user.join("broken/language.toml"),
            "name = \"python\"\nextensions = \"py\"",
        )
        .expect("write");
        std::fs::write(user.join("python/language.toml"), "name = \"python\"").expect("write");
        std::fs::write(
            user.join("python/queries/folds.scm"),
            "(function_definition) @fold",
        )
        .expect("write");
        std::fs::write(
            user.join("nim/language.toml"),
            "name = \"nim\"\nextensions = [\"nim\"]\nshebangs = [\"nim\"]",
        )
        .expect("write");
        let nvim_config = root.join("nvim-config");
        let nvim_data = root.join("nvim-data");
        std::fs::create_dir_all(nvim_data.join("site/queries/rust")).expect("dir");
        std::fs::create_dir_all(nvim_config.join("after/queries/rust")).expect("dir");
        std::fs::write(
            nvim_data.join("site/queries/rust/folds.scm"),
            "(block) @fold",
        )
        .expect("write");
        std::fs::write(
            nvim_config.join("after/queries/rust/folds.scm"),
            "; extends\n(impl_item) @fold",
        )
        .expect("write");

        let problems = load(&Sources {
            user: Some(user.clone()),
            neovim: Some((nvim_config, nvim_data)),
        });
        // The broken pack is reported; the built-in python still works.
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0]
                .to_string()
                .contains("extensions should be a list")
        );
        let python = pack("py").expect("python");
        assert_eq!(python.extensions, ["py", "pyi", "pyw"]);
        // The user's field won; the rest are built in.
        let rust = pack("rust").expect("rust");
        assert_eq!(rust.indent.unit, "  ");
        assert_eq!(rust.servers, ["rust-analyzer"]);
        // A new language from the user.
        assert_eq!(pack("nim").map(|p| p.grammar), Some("nim".into()));

        // Queries: built in, Neovim's, the user's.
        assert!(query("rust", "highlights").is_some_and(|q| q.layer == Layer::Builtin));
        let folds = query("rust", "folds").expect("neovim's");
        assert_eq!(folds.layer, Layer::Neovim);
        assert!(folds.text.contains("(block)") && folds.text.contains("(impl_item)"));
        assert!(query("python", "folds").is_some_and(|q| q.layer == Layer::User));
        assert!(query("go", "highlights").is_none());

        // Detection: exact name, extension, shebang, pattern.
        let lang = |path: &str, line: Option<&str>| detect(Path::new(path), line).map(|p| p.name);
        assert_eq!(lang("src/main.rs", None).as_deref(), Some("rust"));
        assert_eq!(lang("x/Makefile", None).as_deref(), Some("make"));
        assert_eq!(lang("x/Dockerfile", None).as_deref(), Some("dockerfile"));
        assert_eq!(lang("NOTES.MD", None).as_deref(), Some("markdown"));
        assert_eq!(
            lang("bin/tool", Some("#!/usr/bin/env -S python3.12 -u")).as_deref(),
            Some("python")
        );
        assert_eq!(
            lang("bin/run", Some("#!/bin/bash")).as_deref(),
            Some("bash")
        );
        assert_eq!(
            lang("bin/n", Some("#!/usr/bin/nim")).as_deref(),
            Some("nim")
        );
        assert_eq!(lang("bin/x", Some("#!/usr/bin/perl")), None);
        assert_eq!(lang("plain", None), None);

        // A plugin's language joins, and stays through a reload.
        register(
            PackSpec::parse(
                "name = \"gleam\"\nextensions = [\"gleam\"]",
                &Origin::Plugin("p".into()),
            )
            .expect("valid"),
            vec![("highlights".into(), "(x) @keyword".into())],
        );
        assert_eq!(lang("a.gleam", None).as_deref(), Some("gleam"));
        assert!(query("gleam", "highlights").is_some_and(|q| q.layer == Layer::User));

        // Back to the built-in packs only, for the other tests.
        if let Ok(mut specs) = plugin_specs().lock() {
            specs.clear();
        }
        load(&Sources::default());
        assert!(pack("nim").is_none() && pack("gleam").is_none());
        let _ = take_problems();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// By hand, against this machine's Neovim: how long loading takes, and
    /// whether its queries resolve (`cargo test -p mnote-lang real_neovim
    /// -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn real_neovim_queries_resolve() {
        let started = std::time::Instant::now();
        let problems = load(&Sources::standard(None, true));
        println!("load: {:?}, problems: {problems:?}", started.elapsed());
        for language in [
            "rust",
            "typescript",
            "tsx",
            "javascript",
            "python",
            "go",
            "lua",
            "markdown",
        ] {
            let started = std::time::Instant::now();
            for kind in crate::queries::KINDS {
                let q = query(language, kind);
                println!(
                    "{language:>10} {kind:<11} {:>6} bytes {:?}",
                    q.as_ref().map_or(0, |q| q.text.len()),
                    q.map(|q| q.layer)
                );
            }
            println!("{language:>10} took {:?}", started.elapsed());
        }
        println!("problems: {:?}", take_problems());
        load(&Sources::default());
    }

    #[test]
    fn the_built_in_packs_all_read() {
        let mut found = Vec::new();
        let specs = builtin_specs(&mut found);
        assert!(found.is_empty(), "{found:?}");
        assert!(specs.len() >= 20);
        for spec in &specs {
            let pack = Pack::resolve(spec.clone());
            assert!(
                !pack.extensions.is_empty() || !pack.filenames.is_empty(),
                "{}",
                pack.name
            );
        }
    }

    #[test]
    fn shebangs_name_their_program() {
        assert_eq!(shebang("#!/usr/bin/python3").as_deref(), Some("python3"));
        assert_eq!(shebang("#! /usr/bin/env node").as_deref(), Some("node"));
        assert_eq!(
            shebang("#!/usr/bin/env -S deno run").as_deref(),
            Some("deno")
        );
        assert_eq!(shebang("# not one"), None);
        assert!(versioned("python3.12", "python3") && versioned("node20", "node"));
        assert!(!versioned("nodejs", "node") && !versioned("python", "python"));
    }
}
