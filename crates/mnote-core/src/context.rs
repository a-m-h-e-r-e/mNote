//! One context API for completion, plugins and agents (ADR-0011, AMI-241):
//! "what is around this position", asked the same way by everyone.
//!
//! A [`Request`] names the providers it wants and a size limit; the
//! [`Input`] is a snapshot of what the providers read (the text, its
//! syntax, the selection, recent edits, open documents, links), taken on
//! the UI thread and cheap to take, so [`gather`] can run anywhere: on a
//! background thread, versioned by the text's revision, and given up when
//! a newer request comes ([`Cancel`]). The answer is items, each with its
//! kind, document, range (characters at the answer's revision: map them on
//! with the buffer's change log) and priority, trimmed to the limit by
//! priority. The API knows nothing about models: a token is ~4 characters.
//!
//! Phase 1b's providers read only the syntax, the buffer and the index;
//! diagnostics and LSP symbols join in phase 2. Plugins add their own
//! (`mnote.context.register`), which the app runs and merges.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use mnote_lang::outline::Symbol;
use mnote_lang::view::SyntaxSnapshot;
use ropey::Rope;

/// The built-in providers, by name.
pub const PROVIDERS: [&str; 7] = [
    "selection",
    "enclosing",
    "recent_edits",
    "outline",
    "links",
    "backlinks",
    "open_documents",
];

/// How large an answer may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    Chars(usize),
    Lines(usize),
    /// An estimate: four characters a token.
    Tokens(usize),
}

impl Limit {
    /// How much of the limit `text` takes.
    pub fn cost(&self, text: &str) -> usize {
        match self {
            Limit::Chars(_) => text.chars().count(),
            Limit::Lines(_) => text.lines().count().max(1),
            Limit::Tokens(_) => text.chars().count().div_ceil(4),
        }
    }

    pub fn size(&self) -> usize {
        match self {
            Limit::Chars(n) | Limit::Lines(n) | Limit::Tokens(n) => *n,
        }
    }
}

/// What's asked: the providers wanted (none: all) and the size limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub providers: Vec<String>,
    pub limit: Limit,
}

impl Request {
    pub fn wants(&self, provider: &str) -> bool {
        self.providers.is_empty() || self.providers.iter().any(|p| p == provider)
    }
}

/// One piece of context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The provider that gave it (`enclosing`, `outline`, a plugin's).
    pub kind: String,
    /// The document it's from (a note path, a file name).
    pub document: String,
    /// The characters it covers in that document, when it's a part of it.
    pub range: Option<Range<usize>>,
    pub text: String,
    /// Higher goes first, and stays when the limit trims.
    pub priority: i32,
}

/// The answer: the items that fit, by priority, and the text revision
/// their ranges are at.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Answer {
    pub items: Vec<Item>,
    pub revision: u64,
}

/// An edit to show as context: where it is now, and whether the user made
/// it (theirs come first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub by_user: bool,
}

/// Another document, for `links`, `backlinks` and `open_documents`: its
/// name and the text to give (a line linking here, or its start).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Other {
    pub document: String,
    pub text: String,
}

/// What the providers read, taken when the request is made.
#[derive(Debug, Clone)]
pub struct Input {
    pub document: String,
    pub text: Rope,
    pub revision: u64,
    /// The position asked about, in characters.
    pub position: usize,
    pub syntax: Option<SyntaxSnapshot>,
    pub selection: Option<Range<usize>>,
    /// Recent edits, newest first.
    pub edits: Vec<Edit>,
    /// Notes this one links to, notes linking here, other open documents.
    pub links: Vec<Other>,
    pub backlinks: Vec<Other>,
    pub open: Vec<Other>,
}

/// Gives up a request when a newer one is made: each request takes a
/// ticket, and only the newest ticket's still current.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicU64>);

impl Cancel {
    /// A ticket for a new request, which makes every older one stale.
    pub fn ticket(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn current(&self, ticket: u64) -> bool {
        self.0.load(Ordering::SeqCst) == ticket
    }
}

/// How many lines of an enclosing symbol, or of another document, at most.
const MAX_LINES: usize = 200;

/// The built-in providers' items for `request` from `input`, plus `extra`
/// (plugins' items), trimmed to the limit; `None` if `cancel` says a newer
/// request came meanwhile.
pub fn gather(
    request: &Request,
    input: &Input,
    extra: Vec<Item>,
    cancel: Option<(&Cancel, u64)>,
) -> Option<Answer> {
    let stale = || cancel.is_some_and(|(c, t)| !c.current(t));
    let mut items = extra;
    let doc = || input.document.clone();
    let slice = |r: &Range<usize>| -> String {
        let len = input.text.len_chars();
        let r = r.start.min(len)..r.end.min(len);
        let first = input.text.char_to_line(r.start);
        let last = (first + MAX_LINES).min(input.text.len_lines());
        let end = r.end.min(input.text.line_to_char(last));
        input.text.slice(r.start..end.max(r.start)).to_string()
    };

    if request.wants("selection")
        && let Some(sel) = input.selection.clone().filter(|s| !s.is_empty())
    {
        items.push(Item {
            kind: "selection".into(),
            document: doc(),
            text: slice(&sel),
            range: Some(sel),
            priority: 110,
        });
    }

    let outline: Vec<Symbol> = match (
        &input.syntax,
        request.wants("enclosing") || request.wants("outline"),
    ) {
        (Some(syntax), true) => syntax.outline(),
        _ => Vec::new(),
    };
    if stale() {
        return None;
    }
    if request.wants("enclosing") {
        // The symbols around the position, innermost first and highest.
        let around = SyntaxSnapshot::symbols_at(&outline, input.position);
        let n = around.len() as i32;
        for (depth, s) in around.into_iter().enumerate() {
            items.push(Item {
                kind: "enclosing".into(),
                document: doc(),
                text: slice(&s.range),
                range: Some(s.range.clone()),
                priority: 100 - (n - 1 - depth as i32) * 5,
            });
        }
    }
    if request.wants("recent_edits") {
        for (i, edit) in input.edits.iter().take(10).enumerate() {
            // The lines it's on.
            let len = input.text.len_chars();
            let first = input.text.char_to_line(edit.range.start.min(len));
            let last = input.text.char_to_line(edit.range.end.min(len));
            let range = input.text.line_to_char(first)
                ..input
                    .text
                    .line_to_char((last + 1).min(input.text.len_lines()));
            items.push(Item {
                kind: "recent_edits".into(),
                document: doc(),
                text: slice(&range),
                range: Some(range),
                priority: if edit.by_user { 80 } else { 60 } - i as i32,
            });
        }
    }
    if request.wants("outline") && !outline.is_empty() {
        let text: String = outline
            .iter()
            .map(|s| format!("{}{} {}\n", "  ".repeat(s.depth), s.kind, s.name))
            .collect();
        items.push(Item {
            kind: "outline".into(),
            document: doc(),
            text,
            range: None,
            priority: 50,
        });
    }
    let others = [
        ("links", &input.links, 40),
        ("backlinks", &input.backlinks, 35),
        ("open_documents", &input.open, 20),
    ];
    for (kind, list, priority) in others {
        if request.wants(kind) {
            items.extend(list.iter().map(|o| {
                Item {
                    kind: kind.into(),
                    document: o.document.clone(),
                    text: o
                        .text
                        .lines()
                        .take(MAX_LINES)
                        .collect::<Vec<_>>()
                        .join("\n"),
                    range: None,
                    priority,
                }
            }));
        }
    }
    if stale() {
        return None;
    }
    Some(Answer {
        items: trim(items, request.limit),
        revision: input.revision,
    })
}

/// The items that fit `limit`, highest priority first (stable among
/// equals); one too big for what's left is skipped for smaller ones.
pub fn trim(mut items: Vec<Item>, limit: Limit) -> Vec<Item> {
    items.sort_by_key(|i| std::cmp::Reverse(i.priority));
    let mut left = limit.size();
    items
        .into_iter()
        .filter(|item| {
            let cost = limit.cost(&item.text);
            if cost <= left {
                left -= cost;
                true
            } else {
                false
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Buffer;

    const RUST: &str = "struct S;\n\nimpl S {\n    fn go(&self) -> u8 {\n        let x = 1;\n        x\n    }\n}\n";

    fn input(text: &str, at: &str) -> Input {
        let mut buf = Buffer::with_language(text, "rust");
        let _ = buf.syntax_tree_now();
        let position = text.find(at).expect("in the text");
        Input {
            document: "src/s.rs".into(),
            text: buf.text().clone(),
            revision: buf.revision(),
            position,
            syntax: buf.syntax_snapshot(),
            selection: None,
            edits: Vec::new(),
            links: Vec::new(),
            backlinks: Vec::new(),
            open: Vec::new(),
        }
    }

    fn all(limit: Limit) -> Request {
        Request {
            providers: Vec::new(),
            limit,
        }
    }

    #[test]
    fn enclosing_symbols_innermost_highest_and_the_outline() {
        let answer = gather(
            &all(Limit::Chars(10_000)),
            &input(RUST, "x = 1"),
            Vec::new(),
            None,
        )
        .expect("an answer");
        let enclosing: Vec<&Item> = answer
            .items
            .iter()
            .filter(|i| i.kind == "enclosing")
            .collect();
        assert_eq!(enclosing.len(), 1, "{enclosing:?}");
        assert!(enclosing[0].text.starts_with("fn go"));
        let outline = answer
            .items
            .iter()
            .find(|i| i.kind == "outline")
            .expect("outline");
        assert!(outline.text.contains("method go"), "{}", outline.text);
        assert!(outline.text.contains("class S"), "{}", outline.text);
        assert_eq!(answer.items[0].kind, "enclosing", "highest first");
    }

    #[test]
    fn selection_edits_and_other_documents() {
        let mut input = input(RUST, "x = 1");
        input.selection = Some(0..9);
        input.edits = vec![
            Edit {
                range: 40..41,
                by_user: false,
            },
            Edit {
                range: 60..61,
                by_user: true,
            },
        ];
        input.links = vec![Other {
            document: "design.md".into(),
            text: "# Design\nNotes.".into(),
        }];
        input.backlinks = vec![Other {
            document: "todo.md".into(),
            text: "see [[s.rs]]".into(),
        }];
        input.open = vec![Other {
            document: "other.rs".into(),
            text: "fn other() {}".into(),
        }];
        let answer = gather(&all(Limit::Chars(10_000)), &input, Vec::new(), None).expect("answer");
        let kinds: Vec<&str> = answer.items.iter().map(|i| i.kind.as_str()).collect();
        assert_eq!(kinds[0], "selection");
        assert_eq!(answer.items[0].text, "struct S;");
        let edits: Vec<&Item> = answer
            .items
            .iter()
            .filter(|i| i.kind == "recent_edits")
            .collect();
        assert_eq!(edits.len(), 2);
        assert!(edits[0].priority > edits[1].priority, "the user's first");
        for kind in ["links", "backlinks", "open_documents"] {
            assert!(kinds.contains(&kind), "{kind}: {kinds:?}");
        }
        // Only what's asked for.
        let only = Request {
            providers: vec!["links".into()],
            limit: Limit::Lines(100),
        };
        let answer = gather(&only, &input, Vec::new(), None).expect("answer");
        assert_eq!(answer.items.len(), 1);
        assert_eq!(answer.items[0].document, "design.md");
    }

    #[test]
    fn trimming_keeps_the_highest_that_fit() {
        let item = |text: &str, priority| Item {
            kind: "x".into(),
            document: "d".into(),
            range: None,
            text: text.into(),
            priority,
        };
        let items = vec![item("aaaaaaaa", 1), item("bbbb", 3), item("cccccccccc", 2)];
        let kept = trim(items.clone(), Limit::Chars(12));
        let texts: Vec<&str> = kept.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(
            texts,
            ["bbbb", "aaaaaaaa"],
            "the 10 doesn't fit after 4; the 8 does"
        );
        // 4, 10 and 8 characters are 1, 3 and 2 tokens.
        assert_eq!(trim(items.clone(), Limit::Tokens(1)).len(), 1);
        assert_eq!(
            trim(items, Limit::Tokens(3)).len(),
            2,
            "1 + 2; the 3 doesn't fit"
        );
        // Plugins' items merge in by priority.
        let answer = gather(
            &all(Limit::Chars(10_000)),
            &input(RUST, "x = 1"),
            vec![item("from a plugin", 1000)],
            None,
        )
        .expect("answer");
        assert_eq!(answer.items[0].text, "from a plugin");
    }

    #[test]
    fn a_newer_request_cancels_an_older_one() {
        let cancel = Cancel::default();
        let old = cancel.ticket();
        let new = cancel.ticket();
        let input = input(RUST, "x = 1");
        assert!(
            gather(
                &all(Limit::Chars(100)),
                &input,
                Vec::new(),
                Some((&cancel, old))
            )
            .is_none()
        );
        assert!(
            gather(
                &all(Limit::Chars(100)),
                &input,
                Vec::new(),
                Some((&cancel, new))
            )
            .is_some()
        );
    }
}
