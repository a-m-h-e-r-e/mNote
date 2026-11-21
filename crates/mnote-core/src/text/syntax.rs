//! Markdown parsing with tree-sitter's block grammar, on a background thread.
//!
//! The Markdown grammar parses roughly 200 ms per megabyte, and re-parsing
//! after an edit costs nearly as much as a fresh parse. That can't run on the
//! keystroke path (budget: 8 ms to pixels), so:
//!
//! - every edit is applied to the current tree with `Tree::edit`, which only
//!   shifts node positions and is cheap, so the tree always lines up with the
//!   text even before it's re-parsed;
//! - a worker thread re-parses a snapshot of the text (a rope clone is O(1));
//! - when the result arrives, edits made since the snapshot are applied to it
//!   too, and another parse is requested if the text moved on.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use ropey::Rope;
use tree_sitter::{InputEdit, Parser, Point, Tree};

/// Called on the worker thread when a fresh tree is ready, so the app can
/// schedule a redraw.
pub type Waker = Box<dyn Fn() + Send + 'static>;

struct Request {
    text: Rope,
    old_tree: Option<Tree>,
    revision: u64,
}

struct Parsed {
    tree: Option<Tree>,
    revision: u64,
}

pub(crate) struct Syntax {
    requests: Sender<Request>,
    results: Receiver<Parsed>,
    waker: std::sync::Arc<std::sync::Mutex<Option<Waker>>>,
    /// The newest tree, with every edit up to `revision` applied.
    tree: Option<Tree>,
    /// The text revision the tree's structure was parsed from.
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
            .finish()
    }
}

impl Syntax {
    /// Starts a Markdown parser thread, or returns `None` if the grammar can't
    /// load or the thread can't start (the buffer then works as plain text).
    pub fn markdown() -> Option<Self> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_md::LANGUAGE.into()).ok()?;

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
                    let tree = parse(&mut parser, &request.text, request.old_tree.as_ref());
                    let sent = outbox.send(Parsed {
                        tree,
                        revision: request.revision,
                    });
                    if sent.is_err() {
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

    /// Records an edit: shifts the current tree and remembers it for the parse
    /// in flight.
    pub fn edit(&mut self, edit: &InputEdit) {
        if let Some(tree) = &mut self.tree {
            tree.edit(edit);
        }
        if self.in_flight.is_some() {
            self.edits_since_request.push(*edit);
        }
        self.revision += 1;
    }

    /// The newest tree without waiting. It always lines up with the current
    /// text; its structure may lag a few edits behind while a parse runs.
    pub fn tree(&mut self, rope: &Rope) -> Option<&Tree> {
        while let Ok(parsed) = self.results.try_recv() {
            self.accept(parsed);
        }
        self.request(rope);
        self.tree.as_ref()
    }

    /// The tree for the current text, waiting for the parse if needed. For
    /// tests, tools and first loads of small notes; never on the keystroke
    /// path.
    pub fn tree_now(&mut self, rope: &Rope) -> Option<&Tree> {
        loop {
            while let Ok(parsed) = self.results.try_recv() {
                self.accept(parsed);
            }
            if self.is_current() {
                return self.tree.as_ref();
            }
            self.request(rope);
            match self.results.recv() {
                Ok(parsed) => self.accept(parsed),
                Err(_) => return self.tree.as_ref(),
            }
        }
    }

    /// Whether the tree's structure reflects the current text.
    pub fn is_current(&self) -> bool {
        self.parsed_revision == Some(self.revision)
    }

    fn request(&mut self, rope: &Rope) {
        if self.in_flight.is_some() || self.is_current() {
            return;
        }
        let request = Request {
            text: rope.clone(),
            old_tree: self.tree.clone(),
            revision: self.revision,
        };
        if self.requests.send(request).is_ok() {
            self.in_flight = Some(self.revision);
            self.edits_since_request.clear();
        }
    }

    fn accept(&mut self, parsed: Parsed) {
        let mut tree = parsed.tree;
        if let Some(tree) = &mut tree {
            for edit in &self.edits_since_request {
                tree.edit(edit);
            }
        }
        if tree.is_some() {
            self.tree = tree;
        }
        self.parsed_revision = Some(parsed.revision);
        self.in_flight = None;
        self.edits_since_request.clear();
    }
}

fn parse(parser: &mut Parser, rope: &Rope, old_tree: Option<&Tree>) -> Option<Tree> {
    let len = rope.len_bytes();
    let mut read = |byte: usize, _: Point| -> &[u8] {
        if byte >= len {
            return &[];
        }
        let (chunk, chunk_start, _, _) = rope.chunk_at_byte(byte);
        &chunk.as_bytes()[byte - chunk_start..]
    };
    parser.parse_with_options(&mut read, old_tree, None)
}

/// The tree-sitter position (row, byte column) of a byte offset in `rope`.
pub(crate) fn point_at(rope: &Rope, byte: usize) -> Point {
    let row = rope.byte_to_line(byte);
    Point {
        row,
        column: byte - rope.line_to_byte(row),
    }
}
