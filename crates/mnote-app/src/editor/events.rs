//! Editor events: what happened, for whoever needs to react (word count,
//! git blame, later plugins), without hooking into the editor by hand.
//!
//! Most events come from one diff: after every change the editor compares a
//! small [`Snapshot`] of its state (the focused pane, its note, the text
//! revision, the mode and the cursor) with the last one ([`diff`]). Saving,
//! vault changes and config are emitted where they happen.
//!
//! Subscribers run just after the next frame is drawn, so they never delay
//! a keystroke's pixels; they should be quick and push slow work to a
//! background task. [`EditorEvent::NoteSavePre`] is the exception: it runs
//! at once, before the note is written, so a subscriber can still change
//! it.

use std::rc::Rc;

use gpui::Context;
use mnote_core::NotePath;
use mnote_core::text::Origin;
use mnote_vim::Mode;

use super::Editor;
use super::panes::{DocKey, PaneId};

/// Something that happened in the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorEvent {
    /// The focused pane shows another document: `doc` (a note, config.toml
    /// or the scratch note). Also sent once at start.
    NoteOpened { doc: DocKey },
    /// `note` is about to be written. Runs at once, before the write:
    /// subscribers may still edit the text.
    NoteSavePre { note: NotePath },
    /// `note` was written to disk.
    NoteSaved { note: NotePath },
    /// The focused document's text changed; `revision` is its buffer's, and
    /// `origin` says who made the latest change (the user, a plugin…).
    TextChanged {
        doc: DocKey,
        revision: u64,
        origin: Origin,
    },
    /// Vim's mode changed.
    ModeChanged { from: Mode, to: Mode },
    /// INSERT (or REPLACE) ended; also sent as [`EditorEvent::ModeChanged`].
    InsertLeave,
    /// The cursor moved to `line` and `column` (0-based characters). Sent
    /// once per change, however many keys it took.
    CursorMoved { line: usize, column: usize },
    /// Notes changed on disk, outside mNote.
    VaultChanged,
    /// config.toml was read and applied.
    ConfigApplied,
    /// Another pane took focus.
    PaneFocused { pane: PaneId },
    /// A tab opened in `pane` on `doc` (a preview one, or kept).
    TabOpened {
        pane: PaneId,
        doc: DocKey,
        preview: bool,
    },
    /// `pane`'s tab on `doc` closed (or the pane went with it).
    TabClosed { pane: PaneId, doc: DocKey },
    /// `pane` shows its tab on `doc`.
    TabActivated { pane: PaneId, doc: DocKey },
    /// `pane`'s tab on `doc` moved from place `from` to `to` (from 0).
    TabMoved {
        pane: PaneId,
        doc: DocKey,
        from: usize,
        to: usize,
    },
}

/// An event's kind, for subscribing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    NoteOpened,
    NoteSavePre,
    NoteSaved,
    TextChanged,
    ModeChanged,
    InsertLeave,
    CursorMoved,
    VaultChanged,
    ConfigApplied,
    PaneFocused,
    TabOpened,
    TabClosed,
    TabActivated,
    TabMoved,
}

impl EditorEvent {
    pub fn kind(&self) -> EventKind {
        match self {
            EditorEvent::NoteOpened { .. } => EventKind::NoteOpened,
            EditorEvent::NoteSavePre { .. } => EventKind::NoteSavePre,
            EditorEvent::NoteSaved { .. } => EventKind::NoteSaved,
            EditorEvent::TextChanged { .. } => EventKind::TextChanged,
            EditorEvent::ModeChanged { .. } => EventKind::ModeChanged,
            EditorEvent::InsertLeave => EventKind::InsertLeave,
            EditorEvent::CursorMoved { .. } => EventKind::CursorMoved,
            EditorEvent::VaultChanged => EventKind::VaultChanged,
            EditorEvent::ConfigApplied => EventKind::ConfigApplied,
            EditorEvent::PaneFocused { .. } => EventKind::PaneFocused,
            EditorEvent::TabOpened { .. } => EventKind::TabOpened,
            EditorEvent::TabClosed { .. } => EventKind::TabClosed,
            EditorEvent::TabActivated { .. } => EventKind::TabActivated,
            EditorEvent::TabMoved { .. } => EventKind::TabMoved,
        }
    }

    /// The document a tab event is about.
    pub fn tab_doc(&self) -> Option<&DocKey> {
        match self {
            EditorEvent::TabOpened { doc, .. }
            | EditorEvent::TabClosed { doc, .. }
            | EditorEvent::TabActivated { doc, .. }
            | EditorEvent::TabMoved { doc, .. } => Some(doc),
            _ => None,
        }
    }
}

/// The editor's state the diff compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub pane: PaneId,
    pub doc: DocKey,
    pub revision: u64,
    pub mode: Mode,
    /// Line and column.
    pub cursor: (usize, usize),
    /// Who made the latest edit.
    pub origin: Origin,
    /// Every pane's tabs, in layout order.
    pub tabs: Vec<PaneTabs>,
}

/// One pane's tabs, for the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneTabs {
    pub pane: PaneId,
    pub docs: Vec<DocKey>,
    pub previews: Vec<bool>,
    pub active: usize,
}

/// The tab events between two lists of panes' tabs.
fn tab_diff(old: &[PaneTabs], new: &[PaneTabs], events: &mut Vec<EditorEvent>) {
    let none = PaneTabs {
        pane: PaneId(u32::MAX),
        docs: Vec::new(),
        previews: Vec::new(),
        active: usize::MAX,
    };
    for now in new {
        let was = old.iter().find(|p| p.pane == now.pane).unwrap_or(&none);
        for (i, doc) in now.docs.iter().enumerate() {
            if !was.docs.contains(doc) {
                events.push(EditorEvent::TabOpened {
                    pane: now.pane,
                    doc: doc.clone(),
                    preview: now.previews.get(i).copied().unwrap_or(false),
                });
            }
        }
        for doc in &was.docs {
            if !now.docs.contains(doc) {
                events.push(EditorEvent::TabClosed {
                    pane: now.pane,
                    doc: doc.clone(),
                });
            }
        }
        // The tabs both have, in each order: the one that moved furthest.
        let kept_was: Vec<&DocKey> = was.docs.iter().filter(|d| now.docs.contains(d)).collect();
        let kept_now: Vec<&DocKey> = now.docs.iter().filter(|d| was.docs.contains(d)).collect();
        if kept_was != kept_now
            && let Some((doc, _, _)) = kept_now
                .iter()
                .enumerate()
                .filter_map(|(to, d)| Some((*d, kept_was.iter().position(|w| w == d)?, to)))
                .max_by_key(|(_, from, to)| from.abs_diff(*to))
        {
            // Its places in the whole rows, before and after.
            let place = |list: &[DocKey]| list.iter().position(|d| d == doc).unwrap_or(0);
            events.push(EditorEvent::TabMoved {
                pane: now.pane,
                doc: doc.clone(),
                from: place(&was.docs),
                to: place(&now.docs),
            });
        }
        let active = now.docs.get(now.active);
        if active.is_some() && active != was.docs.get(was.active) {
            events.push(EditorEvent::TabActivated {
                pane: now.pane,
                doc: active.cloned().unwrap_or(DocKey::Scratch),
            });
        }
    }
    for was in old {
        if !new.iter().any(|p| p.pane == was.pane) {
            for doc in &was.docs {
                events.push(EditorEvent::TabClosed {
                    pane: was.pane,
                    doc: doc.clone(),
                });
            }
        }
    }
}

/// The events between two snapshots (`None`: the first one).
pub fn diff(old: Option<&Snapshot>, new: &Snapshot) -> Vec<EditorEvent> {
    let mut events = Vec::new();
    let Some(old) = old else {
        events.push(EditorEvent::NoteOpened {
            doc: new.doc.clone(),
        });
        return events;
    };
    if new.pane != old.pane {
        events.push(EditorEvent::PaneFocused { pane: new.pane });
    } else if new.doc != old.doc {
        events.push(EditorEvent::NoteOpened {
            doc: new.doc.clone(),
        });
    } else if new.revision != old.revision {
        events.push(EditorEvent::TextChanged {
            doc: new.doc.clone(),
            revision: new.revision,
            origin: new.origin.clone(),
        });
    }
    if new.mode != old.mode {
        events.push(EditorEvent::ModeChanged {
            from: old.mode,
            to: new.mode,
        });
        if matches!(old.mode, Mode::Insert | Mode::Replace)
            && !matches!(new.mode, Mode::Insert | Mode::Replace)
        {
            events.push(EditorEvent::InsertLeave);
        }
    }
    tab_diff(&old.tabs, &new.tabs, &mut events);
    if new.cursor != old.cursor || new.pane != old.pane || new.doc != old.doc {
        events.push(EditorEvent::CursorMoved {
            line: new.cursor.0,
            column: new.cursor.1,
        });
    }
    events
}

/// Code that reacts to events.
pub type Handler = Rc<dyn Fn(&mut Editor, &EditorEvent, &mut Context<Editor>)>;

/// Subscriptions, the last snapshot and the events waiting to go out.
#[derive(Default)]
pub struct Bus {
    handlers: Vec<(Vec<EventKind>, Handler)>,
    last: Option<Snapshot>,
    queue: Vec<EditorEvent>,
    /// A flush after the next frame is already asked for.
    scheduled: bool,
}

impl Bus {
    /// Calls `handler` for events of these kinds.
    pub fn on(&mut self, kinds: &[EventKind], handler: Handler) {
        self.handlers.push((kinds.to_vec(), handler));
    }

    /// Queues `event` for the next flush.
    pub fn emit(&mut self, event: EditorEvent) {
        // A burst of one kind (cursor moves, text changes) goes out once,
        // with the latest payload.
        if let Some(last) = self.queue.last_mut()
            && last.kind() == event.kind()
            && matches!(
                event.kind(),
                EventKind::CursorMoved | EventKind::TextChanged | EventKind::VaultChanged
            )
        {
            *last = event;
            return;
        }
        self.queue.push(event);
    }

    /// Diffs `now` against the last snapshot and queues what changed.
    pub fn observe(&mut self, now: Snapshot) {
        if self.last.as_ref() == Some(&now) {
            return;
        }
        for event in diff(self.last.as_ref(), &now) {
            self.emit(event);
        }
        self.last = Some(now);
    }

    /// Takes `now` as the last snapshot without sending events: what a
    /// subscriber itself changed doesn't come back to it (a TextChanged
    /// handler that edits would otherwise run forever).
    pub fn resync(&mut self, now: Snapshot) {
        self.last = Some(now);
    }

    /// Whether events wait and no flush is asked for yet (then asks).
    pub fn needs_flush(&mut self) -> bool {
        let needed = !self.queue.is_empty() && !self.scheduled;
        self.scheduled |= needed;
        needed
    }

    /// The waiting events, and who handles each.
    fn take(&mut self) -> Vec<(EditorEvent, Vec<Handler>)> {
        self.scheduled = false;
        std::mem::take(&mut self.queue)
            .into_iter()
            .map(|event| {
                let handlers = self.handlers_for(&event);
                (event, handlers)
            })
            .collect()
    }

    fn handlers_for(&self, event: &EditorEvent) -> Vec<Handler> {
        self.handlers
            .iter()
            .filter(|(kinds, _)| kinds.contains(&event.kind()))
            .map(|(_, h)| h.clone())
            .collect()
    }
}

impl Editor {
    /// The state the event diff compares.
    pub(super) fn snapshot(&self) -> Snapshot {
        let text = self.buffer.text();
        let cursor = self.vim.cursor().min(text.len_chars());
        let line = text.char_to_line(cursor);
        Snapshot {
            pane: self.focused,
            doc: self.source.doc_key(),
            revision: self.buffer.revision(),
            mode: self.vim.mode(),
            cursor: (line, cursor - text.line_to_char(line)),
            origin: self.buffer.last_origin().cloned().unwrap_or_default(),
            tabs: self
                .layout
                .panes()
                .into_iter()
                .filter_map(|id| self.panes.iter().find(|p| p.id == id))
                .map(|p| PaneTabs {
                    pane: p.id,
                    docs: p.tabs.iter().map(|t| t.doc.clone()).collect(),
                    previews: p.tabs.iter().map(|t| t.preview).collect(),
                    active: p.active,
                })
                .collect(),
        }
    }

    /// Queues an event that isn't seen by the diff (saves, vault, config).
    pub(super) fn emit(&mut self, event: EditorEvent, cx: &mut Context<Self>) {
        self.events.emit(event);
        cx.notify();
    }

    /// Runs the subscribers of `event` now (for [`EditorEvent::NoteSavePre`]).
    pub(super) fn dispatch_now(&mut self, event: EditorEvent, cx: &mut Context<Self>) {
        for handler in self.events.handlers_for(&event) {
            handler(self, &event, cx);
        }
    }

    /// Sends the waiting events to their subscribers.
    pub(super) fn flush_events(&mut self, cx: &mut Context<Self>) {
        for (event, handlers) in self.events.take() {
            for handler in handlers {
                handler(self, &event, cx);
            }
        }
        // A query read for a note just opened may have had a problem.
        self.report_language_problems(cx);
    }

    /// The built-in subscribers: word count and git blame.
    pub(super) fn subscribe_builtin(&mut self) {
        use EventKind::*;
        self.events.on(
            &[NoteOpened, TextChanged, PaneFocused],
            Rc::new(|e, _, cx| e.count_words_soon(cx)),
        );
        self.events.on(
            &[
                NoteOpened,
                CursorMoved,
                ModeChanged,
                NoteSaved,
                ConfigApplied,
                PaneFocused,
            ],
            Rc::new(|e, _, cx| e.blame_soon(cx)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(pane: u32, doc: &str, revision: u64, mode: Mode, cursor: (usize, usize)) -> Snapshot {
        Snapshot {
            pane: PaneId(pane),
            doc: if doc.is_empty() {
                DocKey::Scratch
            } else {
                DocKey::Note(NotePath::new(doc).expect("a note path"))
            },
            revision,
            mode,
            cursor,
            origin: Origin::User,
            tabs: Vec::new(),
        }
    }

    fn tabs(pane: u32, docs: &[&str], active: usize) -> PaneTabs {
        PaneTabs {
            pane: PaneId(pane),
            docs: docs
                .iter()
                .map(|d| DocKey::Note(NotePath::new(*d).expect("a note path")))
                .collect(),
            previews: vec![false; docs.len()],
            active,
        }
    }

    fn note(path: &str) -> DocKey {
        DocKey::Note(NotePath::new(path).expect("a note path"))
    }

    #[test]
    fn tabs_opening_closing_showing_and_moving_are_events() {
        let mut events = Vec::new();
        tab_diff(
            &[tabs(0, &["a.md"], 0)],
            &[tabs(0, &["a.md", "b.md"], 1)],
            &mut events,
        );
        assert_eq!(
            events,
            [
                EditorEvent::TabOpened {
                    pane: PaneId(0),
                    doc: note("b.md"),
                    preview: false
                },
                EditorEvent::TabActivated {
                    pane: PaneId(0),
                    doc: note("b.md")
                },
            ]
        );
        events.clear();
        tab_diff(
            &[tabs(0, &["a.md", "b.md", "c.md"], 0)],
            &[tabs(0, &["c.md", "a.md", "b.md"], 1)],
            &mut events,
        );
        assert_eq!(
            events,
            [EditorEvent::TabMoved {
                pane: PaneId(0),
                doc: note("c.md"),
                from: 2,
                to: 0
            }]
        );
        events.clear();
        // A pane gone: its tabs closed.
        tab_diff(
            &[tabs(0, &["a.md"], 0), tabs(1, &["b.md"], 0)],
            &[tabs(0, &["a.md"], 0)],
            &mut events,
        );
        assert_eq!(
            events,
            [EditorEvent::TabClosed {
                pane: PaneId(1),
                doc: note("b.md")
            }]
        );
    }

    fn kinds(events: &[EditorEvent]) -> Vec<EventKind> {
        events.iter().map(EditorEvent::kind).collect()
    }

    #[test]
    fn the_first_snapshot_opens_the_note() {
        let now = snap(0, "a.md", 0, Mode::Normal, (0, 0));
        assert_eq!(kinds(&diff(None, &now)), [EventKind::NoteOpened]);
    }

    #[test]
    fn typing_changes_text_mode_and_cursor() {
        let a = snap(0, "a.md", 1, Mode::Normal, (0, 0));
        let b = snap(0, "a.md", 2, Mode::Insert, (0, 1));
        assert_eq!(
            diff(Some(&a), &b),
            [
                EditorEvent::TextChanged {
                    doc: b.doc.clone(),
                    revision: 2,
                    origin: Origin::User,
                },
                EditorEvent::ModeChanged {
                    from: Mode::Normal,
                    to: Mode::Insert
                },
                EditorEvent::CursorMoved { line: 0, column: 1 },
            ]
        );
        let c = snap(0, "a.md", 2, Mode::Normal, (0, 0));
        assert_eq!(
            kinds(&diff(Some(&b), &c)),
            [
                EventKind::ModeChanged,
                EventKind::InsertLeave,
                EventKind::CursorMoved
            ]
        );
        assert!(diff(Some(&c), &c).is_empty());
    }

    #[test]
    fn another_note_or_pane_is_not_a_text_change() {
        let a = snap(0, "a.md", 5, Mode::Normal, (3, 0));
        let b = snap(0, "b.md", 1, Mode::Normal, (0, 0));
        assert_eq!(
            kinds(&diff(Some(&a), &b)),
            [EventKind::NoteOpened, EventKind::CursorMoved]
        );
        let c = snap(1, "a.md", 5, Mode::Normal, (3, 0));
        assert_eq!(
            kinds(&diff(Some(&b), &c)),
            [EventKind::PaneFocused, EventKind::CursorMoved]
        );
    }

    #[test]
    fn bursts_go_out_once_and_flushes_are_asked_once() {
        let mut bus = Bus::default();
        bus.observe(snap(0, "a.md", 0, Mode::Normal, (0, 0)));
        for i in 1..5 {
            bus.observe(snap(0, "a.md", 0, Mode::Normal, (0, i)));
        }
        let kinds: Vec<_> = bus.queue.iter().map(EditorEvent::kind).collect();
        assert_eq!(kinds, [EventKind::NoteOpened, EventKind::CursorMoved]);
        assert_eq!(
            bus.queue.last(),
            Some(&EditorEvent::CursorMoved { line: 0, column: 4 }),
            "the latest position"
        );
        assert!(bus.needs_flush());
        assert!(!bus.needs_flush(), "asked once");
        assert_eq!(bus.take().len(), 2);
        assert!(!bus.needs_flush(), "nothing waits");
    }

    #[test]
    fn handlers_get_their_kinds_only() {
        let mut bus = Bus::default();
        bus.on(&[EventKind::NoteSaved], Rc::new(|_, _, _| {}));
        bus.on(
            &[EventKind::NoteSaved, EventKind::VaultChanged],
            Rc::new(|_, _, _| {}),
        );
        let saved = EditorEvent::NoteSaved {
            note: NotePath::new("a.md").expect("a note path"),
        };
        assert_eq!(bus.handlers_for(&saved).len(), 2);
        assert_eq!(bus.handlers_for(&EditorEvent::VaultChanged).len(), 1);
        assert_eq!(bus.handlers_for(&EditorEvent::ConfigApplied).len(), 0);
    }
}
