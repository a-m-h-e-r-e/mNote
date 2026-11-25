//! A document's syntax map (ADR-0004): its tree in its own language, and
//! one layer per injection (a code fence's language inside Markdown, YAML
//! frontmatter, a nested injection inside those), each with its language,
//! the byte ranges it covers, and its tree. Parsed on a background thread.
//!
//! The Markdown grammar parses roughly 200 ms per megabyte, and re-parsing
//! after an edit costs nearly as much as a fresh parse. That can't run on the
//! keystroke path (budget: 8 ms to pixels), so:
//!
//! - every edit is applied to every tree at once with `Tree::edit`, which
//!   only shifts node positions and is cheap, so the trees always line up
//!   with the text even before they're re-parsed;
//! - a worker thread re-parses a snapshot of the text (a rope clone is
//!   O(1)): the root tree, then the injections its `injections` query finds,
//!   each layer re-using its old tree where it's the same language at the
//!   same place;
//! - when the result arrives, edits made since the snapshot are applied to
//!   it too, and another parse is requested if the text moved on.
//!
//! A parse has a time limit, so a pathological file can't stall the worker,
//! and on large files injections are skipped ([`Limits`]).
//!
//! Freeing a tree walks every node it alone owns, which on a large note
//! takes milliseconds; replaced trees go to [`drop_tree_later`] so that
//! happens off the UI thread too.

use std::collections::HashMap;
use std::ops::{ControlFlow, Range};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{OnceLock, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use ropey::Rope;
use tree_sitter::{
    InputEdit, Language, Node, ParseOptions, Parser, Point, Query, QueryCursor, StreamingIterator,
    Tree,
};

/// Called on the worker thread when a fresh tree is ready, so the app can
/// schedule a redraw.
pub type Waker = Box<dyn Fn() + Send + 'static>;

/// Injection layers nest at most this deep.
const MAX_DEPTH: usize = 3;
/// The longest a root parse may take, and an injection's.
const ROOT_TIME: Duration = Duration::from_secs(3);
const LAYER_TIME: Duration = Duration::from_millis(300);

/// Sizes above which work is skipped, so editing a huge file stays quick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Above this many bytes, injections aren't parsed.
    pub injections: usize,
    /// Above this many bytes, nothing is highlighted (the app checks it).
    pub highlight: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            injections: 4 * 1024 * 1024,
            highlight: 16 * 1024 * 1024,
        }
    }
}

fn limits_cell() -> &'static RwLock<Limits> {
    static LIMITS: OnceLock<RwLock<Limits>> = OnceLock::new();
    LIMITS.get_or_init(RwLock::default)
}

/// Sets the size limits (from config.toml).
pub fn set_limits(limits: Limits) {
    if let Ok(mut l) = limits_cell().write() {
        *l = limits;
    }
}

/// The size limits now.
pub fn limits() -> Limits {
    limits_cell().read().map(|l| *l).unwrap_or_default()
}

/// One injection: a language over some ranges of the document.
#[derive(Debug, Clone)]
pub struct Layer {
    /// Its language (a pack's name: `rust`, `yaml`).
    pub language: String,
    /// The bytes it covers, in order.
    pub ranges: Vec<Range<usize>>,
    pub tree: Tree,
    /// 1 for an injection into the document, 2 inside that, and so on.
    pub depth: usize,
}

impl Layer {
    /// Whether it covers byte `at`.
    pub fn covers(&self, at: usize) -> bool {
        self.ranges
            .iter()
            .any(|r| r.start <= at && at < r.end.max(r.start + 1))
    }
}

struct Request {
    text: Rope,
    old_tree: Option<Tree>,
    old_layers: Vec<Layer>,
    revision: u64,
}

struct Parsed {
    tree: Option<Tree>,
    layers: Vec<Layer>,
    revision: u64,
}

/// A document's syntax map, kept current by a background parser: edits
/// shift its trees at once, and a worker re-parses snapshots of the text.
pub struct Syntax {
    requests: Sender<Request>,
    results: Receiver<Parsed>,
    waker: std::sync::Arc<std::sync::Mutex<Option<Waker>>>,
    /// The newest root tree, with every edit up to `revision` applied.
    tree: Option<Tree>,
    /// The injection layers, with the same edits applied.
    layers: Vec<Layer>,
    /// The text revision the trees' structure was parsed from.
    parsed_revision: Option<u64>,
    /// The revision of the parse in flight, if any.
    in_flight: Option<u64>,
    /// Edits made after the in-flight snapshot, to replay on its result.
    edits_since_request: Vec<InputEdit>,
    revision: u64,
}

impl std::fmt::Debug for Syntax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Syntax")
            .field("revision", &self.revision)
            .field("parsed_revision", &self.parsed_revision)
            .field("in_flight", &self.in_flight)
            .field("layers", &self.layers.len())
            .finish()
    }
}

impl Syntax {
    /// A Markdown document's map (with its code fences and frontmatter as
    /// layers), or `None` if the grammar can't load or the thread can't
    /// start (the buffer then works as plain text).
    pub fn markdown() -> Option<Self> {
        Self::with_injections(&tree_sitter_md::LANGUAGE.into(), Some("markdown"))
    }

    /// A map for `language` (any tree-sitter grammar), with no injections.
    pub fn new(language: &Language) -> Option<Self> {
        Self::with_injections(language, None)
    }

    /// A map for `language`, finding injections with the `injections` query
    /// of the language called `name`.
    pub fn with_injections(language: &Language, name: Option<&str>) -> Option<Self> {
        let mut worker = Worker::new(language, name)?;
        let (requests, inbox) = mpsc::channel::<Request>();
        let (outbox, results) = mpsc::channel::<Parsed>();
        let waker: std::sync::Arc<std::sync::Mutex<Option<Waker>>> = Default::default();
        let worker_waker = waker.clone();
        thread::Builder::new()
            .name("mnote-syntax".into())
            .spawn(move || {
                // Ends when the buffer (and with it the sender) is dropped.
                while let Ok(mut request) = inbox.recv() {
                    // Only the newest snapshot matters; skip any queued behind it.
                    while let Ok(newer) = inbox.try_recv() {
                        request = newer;
                    }
                    let parsed = worker.run(request);
                    if outbox.send(parsed).is_err() {
                        break;
                    }
                    if let Ok(guard) = worker_waker.lock()
                        && let Some(wake) = guard.as_ref()
                    {
                        wake();
                    }
                }
            })
            .ok()?;

        Some(Syntax {
            requests,
            results,
            waker,
            tree: None,
            layers: Vec::new(),
            parsed_revision: None,
            in_flight: None,
            edits_since_request: Vec::new(),
            revision: 0,
        })
    }

    pub fn set_waker(&mut self, waker: Waker) {
        if let Ok(mut guard) = self.waker.lock() {
            *guard = Some(waker);
        }
    }

    /// Records an edit: shifts every tree (and each layer's ranges) and
    /// remembers it for the parse in flight.
    pub fn edit(&mut self, edit: &InputEdit) {
        if let Some(tree) = &mut self.tree {
            tree.edit(edit);
        }
        for layer in &mut self.layers {
            edit_layer(layer, edit);
        }
        if self.in_flight.is_some() {
            self.edits_since_request.push(*edit);
        }
        self.revision += 1;
    }

    /// Parses again even though the text didn't change (a grammar an
    /// injection needs has just loaded).
    pub fn reparse(&mut self) {
        self.parsed_revision = None;
    }

    /// The newest root tree without waiting. It always lines up with the
    /// current text; its structure may lag a few edits behind while a parse
    /// runs.
    pub fn tree(&mut self, rope: &Rope) -> Option<&Tree> {
        self.poll(rope);
        self.tree.as_ref()
    }

    /// The injection layers, without waiting (as [`Self::tree`]).
    pub fn layers(&mut self, rope: &Rope) -> &[Layer] {
        self.poll(rope);
        &self.layers
    }

    /// The deepest layer of `language` covering byte `at`.
    pub fn layer_at(&mut self, rope: &Rope, at: usize, language: &str) -> Option<&Layer> {
        self.poll(rope);
        self.layers
            .iter()
            .filter(|l| l.covers(at) && l.language == language)
            .max_by_key(|l| l.depth)
    }

    /// The trees for the current text, waiting for the parse if needed. For
    /// tests, tools and first loads of small notes; never on the keystroke
    /// path.
    pub fn tree_now(&mut self, rope: &Rope) -> Option<&Tree> {
        self.wait(rope);
        self.tree.as_ref()
    }

    /// The layers for the current text, waiting as [`Self::tree_now`].
    pub fn layers_now(&mut self, rope: &Rope) -> &[Layer] {
        self.wait(rope);
        &self.layers
    }

    /// Whether the trees' structure reflects the current text.
    pub fn is_current(&self) -> bool {
        self.parsed_revision == Some(self.revision)
    }

    fn poll(&mut self, rope: &Rope) {
        while let Ok(parsed) = self.results.try_recv() {
            self.accept(parsed);
        }
        self.request(rope);
    }

    fn wait(&mut self, rope: &Rope) {
        loop {
            while let Ok(parsed) = self.results.try_recv() {
                self.accept(parsed);
            }
            if self.is_current() {
                return;
            }
            self.request(rope);
            match self.results.recv() {
                Ok(parsed) => self.accept(parsed),
                Err(_) => return,
            }
        }
    }

    fn request(&mut self, rope: &Rope) {
        if self.in_flight.is_some() || self.is_current() {
            return;
        }
        let request = Request {
            text: rope.clone(),
            old_tree: self.tree.clone(),
            old_layers: self.layers.clone(),
            revision: self.revision,
        };
        if self.requests.send(request).is_ok() {
            self.in_flight = Some(self.revision);
            self.edits_since_request.clear();
        }
    }

    fn accept(&mut self, parsed: Parsed) {
        let mut tree = parsed.tree;
        let mut layers = parsed.layers;
        for edit in &self.edits_since_request {
            if let Some(tree) = &mut tree {
                tree.edit(edit);
            }
            for layer in &mut layers {
                edit_layer(layer, edit);
            }
        }
        if tree.is_some() {
            drop_tree_later(std::mem::replace(&mut self.tree, tree));
            for old in std::mem::replace(&mut self.layers, layers) {
                drop_tree_later(Some(old.tree));
            }
        }
        self.parsed_revision = Some(parsed.revision);
        self.in_flight = None;
        self.edits_since_request.clear();
    }
}

/// Shifts a layer for an edit: its tree, and its ranges as byte positions.
fn edit_layer(layer: &mut Layer, edit: &InputEdit) {
    layer.tree.edit(edit);
    let shift = |pos: usize| {
        if pos >= edit.old_end_byte {
            pos - edit.old_end_byte + edit.new_end_byte
        } else if pos > edit.start_byte {
            edit.start_byte.max(pos.min(edit.new_end_byte))
        } else {
            pos
        }
    };
    for range in &mut layer.ranges {
        let start = shift(range.start);
        range.end = shift(range.end).max(start);
        range.start = start;
    }
}

/// The worker thread's parsers and queries.
struct Worker {
    root: Parser,
    root_name: Option<String>,
    /// A parser per injected language.
    parsers: HashMap<String, Parser>,
    /// Compiled `injections` queries by language, with their predicate
    /// checks (`None`: it has none).
    queries: HashMap<String, Option<(Query, crate::predicates::Checks)>>,
}

impl Worker {
    fn new(language: &Language, name: Option<&str>) -> Option<Worker> {
        let mut root = Parser::new();
        root.set_language(language).ok()?;
        Some(Worker {
            root,
            root_name: name.map(str::to_owned),
            parsers: HashMap::new(),
            queries: HashMap::new(),
        })
    }

    fn run(&mut self, request: Request) -> Parsed {
        let rope = &request.text;
        let tree = parse(&mut self.root, rope, request.old_tree.as_ref(), ROOT_TIME);
        let layers = match (&tree, self.root_name.clone()) {
            (Some(tree), Some(name)) if rope.len_bytes() <= limits().injections => {
                let mut layers = Vec::new();
                self.inject(&name, tree, rope, &request.old_layers, 1, &mut layers);
                layers
            }
            // Timed out (the old trees stay), too big, or no injections.
            (None, _) => request.old_layers,
            _ => Vec::new(),
        };
        Parsed {
            tree,
            layers,
            revision: request.revision,
        }
    }

    /// The injections `language`'s query finds in `tree`, parsed, then
    /// theirs, to [`MAX_DEPTH`].
    fn inject(
        &mut self,
        language: &str,
        tree: &Tree,
        rope: &Rope,
        old: &[Layer],
        depth: usize,
        out: &mut Vec<Layer>,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        let found = match self.query(language) {
            Some((query, checks)) => injections(query, Some(checks), tree, rope),
            None => return,
        };
        for (name, ranges) in found {
            let Some(grammar) = crate::code::language(&name) else {
                continue;
            };
            let parser = self.parsers.entry(name.clone()).or_default();
            if parser.set_language(&grammar).is_err() {
                continue;
            }
            let ts_ranges: Vec<tree_sitter::Range> =
                ranges.iter().map(|r| ts_range(rope, r)).collect();
            if parser.set_included_ranges(&ts_ranges).is_err() {
                continue;
            }
            // Re-use the old tree for the same language at the same place.
            let reuse = old
                .iter()
                .find(|l| {
                    l.language == name
                        && l.depth == depth
                        && l.ranges.first().map(|r| r.start) == ranges.first().map(|r| r.start)
                })
                .map(|l| &l.tree);
            let Some(layer_tree) = parse(parser, rope, reuse, LAYER_TIME) else {
                continue;
            };
            self.inject(&name, &layer_tree, rope, old, depth + 1, out);
            out.push(Layer {
                language: name,
                ranges,
                tree: layer_tree,
                depth,
            });
        }
    }

    /// `language`'s compiled `injections` query, compiled once.
    fn query(&mut self, language: &str) -> Option<&(Query, crate::predicates::Checks)> {
        if !self.queries.contains_key(language) {
            let compiled = crate::code::injections(language)
                .and_then(|(grammar, text)| Query::new(&grammar, &text).ok())
                .map(|mut query| {
                    let (checks, _) = crate::predicates::Checks::new(&mut query);
                    (query, checks)
                });
            self.queries.insert(language.to_owned(), compiled);
        }
        self.queries.get(language).and_then(Option::as_ref)
    }
}

/// What an `injections` query finds: each injection's language (a pack's
/// name where one matches) and byte ranges, in document order. Combined
/// injections (`injection.combined`) of one language become one.
pub fn injections(
    query: &Query,
    checks: Option<&crate::predicates::Checks>,
    tree: &Tree,
    rope: &Rope,
) -> Vec<(String, Vec<Range<usize>>)> {
    let names = query.capture_names();
    let content = names.iter().position(|n| *n == "injection.content");
    let language_capture = names.iter().position(|n| *n == "injection.language");
    let Some(content) = content else {
        return Vec::new();
    };
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), |node: Node| {
        text_chunks(rope, node.byte_range())
    });
    let mut separate: Vec<(String, Vec<Range<usize>>)> = Vec::new();
    let mut combined: Vec<(String, Vec<Range<usize>>)> = Vec::new();
    while let Some(m) = matches.next() {
        if checks.is_some_and(|c| !c.passes(m, rope)) {
            continue;
        }
        let props = query.property_settings(m.pattern_index);
        let mut language = props
            .iter()
            .find(|p| &*p.key == "injection.language")
            .and_then(|p| p.value.as_deref().map(str::to_owned));
        if let Some(i) = language_capture
            && let Some(c) = m.captures().iter().find(|c| c.index as usize == i)
        {
            language = Some(node_text(rope, c.node));
        }
        // nvim-treesitter's `(#set-lang-from-info-string! @lang)`.
        for p in query.general_predicates(m.pattern_index) {
            if &*p.operator == "set-lang-from-info-string!"
                && let Some(tree_sitter::QueryPredicateArg::Capture(i)) = p.args.first()
                && let Some(c) = m.captures().iter().find(|c| c.index == *i)
            {
                language = Some(node_text(rope, c.node));
            }
        }
        let Some(language) = language
            .map(|l| l.trim().to_ascii_lowercase())
            .filter(|l| !l.is_empty())
        else {
            continue;
        };
        let language = crate::languages::pack(&language).map_or(language, |p| p.name);
        let ranges: Vec<Range<usize>> = m
            .captures()
            .iter()
            .filter(|c| c.index as usize == content)
            .map(|c| c.node.byte_range())
            .filter(|r| !r.is_empty())
            .collect();
        if ranges.is_empty() {
            continue;
        }
        if props.iter().any(|p| &*p.key == "injection.combined") {
            match combined.iter_mut().find(|(l, _)| *l == language) {
                Some((_, all)) => all.extend(ranges),
                None => combined.push((language, ranges)),
            }
        } else {
            separate.push((language, ranges));
        }
    }
    for (_, ranges) in &mut combined {
        ranges.sort_by_key(|r| r.start);
        ranges.dedup_by(|b, a| b.start < a.end);
    }
    separate.extend(combined);
    separate.sort_by_key(|(_, r)| r.first().map_or(0, |r| r.start));
    separate
}

/// A rope's bytes in `range`, chunk by chunk, for queries.
pub(crate) fn text_chunks(rope: &Rope, range: Range<usize>) -> impl Iterator<Item = &[u8]> {
    rope.get_byte_slice(range.start.min(rope.len_bytes())..range.end.min(rope.len_bytes()))
        .into_iter()
        .flat_map(|s| s.chunks())
        .map(str::as_bytes)
}

fn node_text(rope: &Rope, node: Node) -> String {
    text_chunks(rope, node.byte_range())
        .map(String::from_utf8_lossy)
        .collect()
}

fn ts_range(rope: &Rope, r: &Range<usize>) -> tree_sitter::Range {
    tree_sitter::Range {
        start_byte: r.start,
        end_byte: r.end,
        start_point: point_at(rope, r.start),
        end_point: point_at(rope, r.end),
    }
}

/// Frees `tree` on a background thread: freeing a large note's tree takes
/// milliseconds, too long for the keystroke path. Trees share nodes with
/// atomic counts, so this is safe while copies are still in use. Without the
/// thread it's freed here.
pub fn drop_tree_later(tree: Option<Tree>) {
    static TRASH: OnceLock<Option<Sender<Tree>>> = OnceLock::new();
    let Some(tree) = tree else {
        return;
    };
    let trash = TRASH.get_or_init(|| {
        let (send, recv) = mpsc::channel::<Tree>();
        thread::Builder::new()
            .name("mnote-tree-drop".into())
            .spawn(move || recv.into_iter().for_each(drop))
            .ok()
            .map(|_| send)
    });
    if let Some(trash) = trash {
        // A failed send hands the tree back; it's then freed here.
        let _ = trash.send(tree);
    }
}

/// Parses `rope` (where the parser's included ranges say), giving up after
/// `limit`.
fn parse(
    parser: &mut Parser,
    rope: &Rope,
    old_tree: Option<&Tree>,
    limit: Duration,
) -> Option<Tree> {
    let len = rope.len_bytes();
    let mut read = |byte: usize, _: Point| -> &[u8] {
        if byte >= len {
            return &[];
        }
        let (chunk, chunk_start, _, _) = rope.chunk_at_byte(byte);
        &chunk.as_bytes()[byte - chunk_start..]
    };
    let deadline = Instant::now() + limit;
    let mut progress = |_: &tree_sitter::ParseState| {
        if Instant::now() > deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut progress);
    let tree = parser.parse_with_options(&mut read, old_tree, Some(options));
    // A parse that gave up leaves the parser mid-way; start clean next time.
    if tree.is_none() {
        parser.reset();
    }
    tree
}

/// The tree-sitter position (row, byte column) of a byte offset in `rope`.
pub fn point_at(rope: &Rope, byte: usize) -> Point {
    let row = rope.byte_to_line(byte);
    Point {
        row,
        column: byte - rope.line_to_byte(row),
    }
}

#[cfg(test)]
mod tests;
