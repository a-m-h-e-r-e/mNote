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
    /// The focused document's text changed; `revision` is its buffer's.
    TextChanged { doc: DocKey, revision: u64 },
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
        }
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
                    revision: 2
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
