//! Code files (design phase 1a): files of the vault that aren't notes, such
//! as `.rs` or `.py`, open as code. They're monospace, never styled as
//! Markdown, and coloured from their syntax map (the root tree in their
//! language and its injections). `:edit` opens them by path, and
//! `:setfiletype` (or `:set filetype=`) changes a buffer's language.
//!
//! Colours are worked out for up to 64 visible rows at a time and kept per
//! line. An edit drops only the lines it touched (read from the buffer's
//! change log) and moves the ones below; a parse landing drops them all.
//! Before a file's first parse lands, the visible rows are parsed on their
//! own for provisional colours. A language's highlight query compiles off
//! the UI thread, starting when the file opens.

use std::collections::HashSet;
use std::ops::Range;
use std::time::Instant;

use mnote_core::lang::scopes::Scope;

use super::*;

/// Rows coloured together, and kept together in the cache.
const CHUNK: usize = 64;

/// Rows parsed around the visible ones for colours before the first parse.
const PROVISIONAL_CONTEXT: usize = 32;

/// Languages whose highlight query is compiling off the UI thread.
fn compiling() -> &'static std::sync::Mutex<HashSet<String>> {
    static COMPILING: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> =
        std::sync::OnceLock::new();
    COMPILING.get_or_init(Default::default)
}

impl Editor {
    /// Whether the focused document is a code file: a file of the vault that
    /// isn't a note.
    pub(super) fn is_code_file(&self) -> bool {
        matches!(&self.source, Source::Note { note, .. } if !note.is_note())
    }

    /// Whether `lang`'s colours can be drawn now. If its query isn't
    /// compiled yet, it's compiled in the background (and the view redrawn
    /// when it's done); if it has no grammar, one may be loadable.
    pub(super) fn colours_ready(&mut self, lang: &str, cx: &mut Context<Self>) -> bool {
        if !mnote_core::code::has_grammar(lang) {
            self.want_grammar(lang, cx);
            return false;
        }
        if mnote_core::code::ready(lang) {
            return true;
        }
        let started = compiling()
            .lock()
            .map(|mut set| set.insert(lang.to_owned()))
            .unwrap_or(false);
        if started {
            let name = lang.to_owned();
            let task = cx
                .background_executor()
                .spawn(async move { mnote_core::code::supported(&name) });
            let name = lang.to_owned();
            cx.spawn(async move |this, cx| {
                task.await;
                if let Ok(mut set) = compiling().lock() {
                    set.remove(&name);
                }
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
        false
    }

    /// Line `ix`'s colours in a code file, as byte ranges of the line.
    pub(super) fn code_file_spans(
        &mut self,
        ix: usize,
        cx: &mut Context<Self>,
    ) -> Option<Vec<(Range<usize>, Scope)>> {
        if self.code_opened_frame.is_none() {
            self.code_opened_frame = Some(Instant::now());
        }
        let rope = self.buffer.text();
        if rope.len_bytes() > mnote_core::lang::syntax::limits().highlight {
            return None;
        }
        let lang = self.buffer.language()?.to_owned();
        if !self.colours_ready(&lang, cx) {
            return None;
        }
        self.line_cache.sync(&mut self.buffer);
        if let Some(line) = self.line_cache.lines.get(&ix) {
            return Some(line.as_ref().clone());
        }
        // Work out the rows from here that aren't kept, up to a chunk.
        let count = line_count(self.buffer.text());
        let mut last = ix + 1;
        while last < count.min(ix + CHUNK) && !self.line_cache.lines.contains_key(&last) {
            last += 1;
        }
        let provisional = self.buffer.syntax_tree().is_none();
        let spans = if provisional {
            // The file's first parse hasn't landed: these rows alone, parsed
            // now, so colours show at once; replaced when the tree is in.
            provisional_spans(&lang, self.buffer.text(), ix, last)
        } else {
            let tree = self.buffer.syntax_tree()?.clone();
            let layers = self.buffer.syntax_layers().to_vec();
            let rope = self.buffer.text();
            let byte = |row: usize| rope.char_to_byte(line_start(rope, row));
            let end = if last < count {
                byte(last)
            } else {
                rope.len_bytes()
            };
            mnote_core::lang::highlight::highlight_layers(
                Some((&lang, &tree)),
                &layers,
                rope,
                byte(ix)..end,
            )
        };
        let rope = self.buffer.text();
        let mut coloured = false;
        for row in ix..last {
            let from = rope.char_to_byte(line_start(rope, row));
            let to = rope.char_to_byte(line_start(rope, row) + line_len(rope, row));
            let line: Vec<(Range<usize>, Scope)> = spans
                .iter()
                .filter(|s| s.range.start < to && s.range.end > from)
                .map(|s| {
                    (
                        s.range.start.max(from) - from..s.range.end.min(to) - from,
                        s.scope,
                    )
                })
                .collect();
            coloured |= !line.is_empty();
            self.line_cache.lines.insert(row, Rc::new(line));
        }
        if provisional {
            self.line_cache.provisional = true;
        }
        if coloured {
            self.first_colours_shown();
        }
        self.line_cache.lines.get(&ix).map(|l| l.as_ref().clone())
    }

    /// With `MNOTE_LATENCY`, says how long the code file took from opening
    /// to its first colours (once per file opened).
    fn first_colours_shown(&mut self) {
        if self.code_opened.take().is_none() {
            return;
        }
        // Timed from the file's first frame of text (startup isn't the
        // file's doing).
        if let Some(opened) = self.code_opened_frame
            && std::env::var_os("MNOTE_LATENCY").is_some()
        {
            eprintln!(
                "mnote first colours: {:.1} ms",
                opened.elapsed().as_secs_f32() * 1000.
            );
        }
    }

    /// Notes when a code file was opened, for [`Self::first_colours_shown`].
    /// Its highlight query starts compiling now, alongside the first frame,
    /// so it's usually ready by the time the frame asks for colours.
    pub(super) fn code_file_opened(&mut self, cx: &mut Context<Self>) {
        self.code_opened = self.is_code_file().then(Instant::now);
        self.code_opened_frame = None;
        self.line_cache = LineCache::default();
        if self.code_opened.is_some()
            && let Some(lang) = self.buffer.language().map(str::to_owned)
        {
            self.colours_ready(&lang, cx);
        }
    }

    /// `:edit {path}`: a note or a file of the vault, by its path. A note
    /// can leave out `.md`; a code file that doesn't exist yet opens empty
    /// and is made when it's saved.
    pub(super) fn edit_path(&mut self, arg: Option<&str>, cx: &mut Context<Self>) {
        let Some(arg) = arg.map(str::trim).filter(|a| !a.is_empty()) else {
            self.reply = Some(("E32: No file name".into(), true));
            return;
        };
        let Some(vault) = self.vault.clone() else {
            self.reply = Some((
                "A scratch note has no vault to open files from".into(),
                true,
            ));
            return;
        };
        let note = NotePath::new(arg)
            .ok()
            .filter(|n| vault.path_of(n).exists())
            .or_else(|| {
                NotePath::new(format!("{arg}.md"))
                    .ok()
                    .filter(|n| vault.path_of(n).exists())
            })
            .or_else(|| {
                NotePath::file(arg)
                    .ok()
                    .filter(|n| vault.path_of(n).exists())
            });
        if let Some(note) = note {
            self.switch_to(note, cx);
            return;
        }
        // Doesn't exist yet: a note is made from its name, a file opens empty.
        if let Ok(note) = NotePath::new(arg).or_else(|_| NotePath::new(format!("{arg}.md")))
            && !std::path::Path::new(arg)
                .extension()
                .is_some_and(|e| e != "md" && e != "markdown")
        {
            self.picker_action(crate::picker::PickerAction::CreateNote(note), cx);
            return;
        }
        match NotePath::file(arg) {
            Ok(file) => self.open_new_file(vault, file, cx),
            Err(_) => self.reply = Some((format!("E499: Can't open {arg}").into(), true)),
        }
    }

    /// A code file that doesn't exist yet, empty, in its language.
    fn open_new_file(&mut self, vault: Vault, file: NotePath, cx: &mut Context<Self>) {
        self.save_now(cx);
        let language = mnote_core::filetype::for_path(file.as_path()).and_then(|f| f.language);
        let buffer = match &language {
            Some(language) => Buffer::with_language("", language),
            None => Buffer::new_plain(""),
        };
        let doc = self.new_doc(buffer, Source::Note { vault, note: file });
        self.show_doc(doc);
        self.code_file_opened(cx);
        cx.notify();
    }

    /// `:setfiletype {language}` (and `:set filetype=`): parses and colours
    /// the buffer as `language` from now on; with no name, says which it is.
    pub(super) fn set_filetype(&mut self, arg: Option<&str>, cx: &mut Context<Self>) {
        let Some(name) = arg.map(str::trim).filter(|a| !a.is_empty()) else {
            let shown = self.buffer.language().unwrap_or("none").to_owned();
            self.reply = Some((format!("filetype={shown}").into(), false));
            return;
        };
        let language =
            mnote_core::lang::languages::pack(name).map_or_else(|| name.to_owned(), |p| p.name);
        self.buffer.set_language(&language);
        let tx = self.syntax_ready.clone();
        self.buffer.on_syntax_ready(move || {
            let _ = tx.try_send(());
        });
        self.code_cache.clear();
        self.line_cache = LineCache::default();
        if !mnote_core::code::has_grammar(&language) {
            self.want_grammar(&language, cx);
        }
        self.reply = Some((format!("filetype={language}").into(), false));
        cx.notify();
    }
}

/// One line's colours: byte ranges of the line.
type LineSpans = Vec<(Range<usize>, Scope)>;

/// A code file's colours per line, kept as the text changes: an edit drops
/// only the rows it touched (through the buffer's change log), and the rows
/// below move with it; a parse landing drops them all.
#[derive(Default)]
pub(super) struct LineCache {
    consumer: Option<mnote_core::text::ConsumerId>,
    revision: u64,
    generation: u64,
    /// Kept while the file's first parse hadn't landed.
    provisional: bool,
    lines: std::collections::HashMap<usize, Rc<LineSpans>>,
}

impl LineCache {
    /// Catches up with `buffer`'s edits and parses.
    fn sync(&mut self, buffer: &mut Buffer) {
        let consumer = *self
            .consumer
            .get_or_insert_with(|| buffer.subscribe_changes());
        let generation = buffer.syntax_stamp() & 0xF_FFFF;
        let revision = buffer.revision();
        if generation != self.generation || (self.provisional && buffer.syntax_is_current()) {
            self.lines.clear();
            self.provisional = false;
        } else if revision != self.revision {
            match buffer.changes_since(self.revision) {
                Some(changes) => {
                    for change in changes {
                        let first = change.start.row;
                        let old_last = change.old_end.row;
                        let new_last = change.new_end.row;
                        let delta = new_last as isize - old_last as isize;
                        let kept: std::collections::HashMap<_, _> = self
                            .lines
                            .drain()
                            .filter_map(|(row, line)| {
                                if row < first {
                                    Some((row, line))
                                } else if row > old_last {
                                    Some(((row as isize + delta) as usize, line))
                                } else {
                                    None
                                }
                            })
                            .collect();
                        self.lines = kept;
                    }
                }
                None => self.lines.clear(),
            }
        }
        buffer.ack_changes(consumer, revision);
        self.revision = revision;
        self.generation = generation;
    }
}

/// Rows `first..last` coloured by parsing them (and a few around) alone,
/// for before the file's first parse lands. Byte ranges are of the file.
fn provisional_spans(
    lang: &str,
    rope: &mnote_core::Rope,
    first: usize,
    last: usize,
) -> Vec<mnote_core::lang::highlight::Span> {
    let from_row = first.saturating_sub(PROVISIONAL_CONTEXT);
    let to_row = (last + PROVISIONAL_CONTEXT).min(line_count(rope));
    let from = rope.char_to_byte(line_start(rope, from_row));
    let to = if to_row < line_count(rope) {
        rope.char_to_byte(line_start(rope, to_row))
    } else {
        rope.len_bytes()
    };
    let snippet = rope.byte_slice(from..to).to_string();
    let Some(spans) = mnote_core::lang::highlight::highlight_text_spans(lang, &snippet) else {
        return Vec::new();
    };
    spans
        .into_iter()
        .map(|mut s| {
            s.range = s.range.start + from..s.range.end + from;
            s
        })
        .collect()
}
