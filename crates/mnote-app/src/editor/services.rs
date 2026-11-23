//! Background services the editor runs beside the note: the word count,
//! git blame and the latency probe. Each keeps its own state here, with
//! plain methods that can be read and tested on their own; the editor only
//! starts their work (from editor events, see `events`) and shows results.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, IntoElement, SharedString, Styled, Task};
use mnote_vim::Mode;
use mnote_vim::text::{line_of, line_start};

use super::panes::DocKey;
use super::{Editor, Source};

/// How long typing has to pause before words are counted again.
const WORD_COUNT_DELAY: Duration = Duration::from_millis(400);
/// How long the cursor rests on a line before git is asked about it.
const BLAME_DELAY: Duration = Duration::from_millis(300);

// ---- word count -------------------------------------------------------------

/// The focused note's word count, for the status line.
#[derive(Default)]
pub struct WordCount {
    /// The document, its text revision and its words.
    counted: Option<(DocKey, u64, usize)>,
    task: Option<Task<()>>,
}

impl WordCount {
    /// The words of `doc`, if they were counted.
    pub fn count_for(&self, doc: &DocKey) -> Option<usize> {
        self.counted
            .as_ref()
            .filter(|(key, _, _)| key == doc)
            .map(|(_, _, words)| *words)
    }

    /// Whether `doc` at `revision` still needs counting (and nothing is
    /// counting already).
    fn wants(&self, doc: &DocKey, revision: u64) -> bool {
        let current = self
            .counted
            .as_ref()
            .is_some_and(|(k, r, _)| k == doc && *r == revision);
        !current && self.task.is_none()
    }

    fn store(&mut self, doc: DocKey, revision: u64, words: usize) {
        self.counted = Some((doc, revision, words));
        self.task = None;
    }
}

// ---- git blame --------------------------------------------------------------

/// Who last changed the cursor line, from git.
#[derive(Default)]
pub struct Blame {
    /// The document, the line and its label (`None`: git doesn't know).
    known: Option<(DocKey, usize, Option<SharedString>)>,
    task: Option<Task<()>>,
}

impl Blame {
    /// The label for `line` of `doc`, if git was asked and knows.
    pub fn label_for(&self, doc: &DocKey, line: usize) -> Option<&SharedString> {
        match &self.known {
            Some((key, at, Some(label))) if key == doc && *at == line => Some(label),
            _ => None,
        }
    }

    /// Whether git was already asked about `line` of `doc`.
    fn knows(&self, doc: &DocKey, line: usize) -> bool {
        self.known
            .as_ref()
            .is_some_and(|(k, l, _)| k == doc && *l == line)
    }
}

// ---- latency probe ----------------------------------------------------------

/// Per key: milliseconds from the key to its painted frame, and the wait.
type LatencyLog = Rc<RefCell<Vec<(f32, f32)>>>;

/// Keystroke-to-pixels times (`MNOTE_LATENCY=1`), and their summary.
#[derive(Default)]
pub struct LatencyProbe {
    /// Per key: milliseconds from the key to its painted frame, and of those
    /// the wait for the display's next frame.
    log: Option<LatencyLog>,
    key_at: Option<Instant>,
    frame_wait: Option<f32>,
}

impl LatencyProbe {
    /// On when `MNOTE_LATENCY` is set.
    pub fn from_env() -> Self {
        LatencyProbe {
            log: std::env::var_os("MNOTE_LATENCY").map(|_| Rc::default()),
            ..Self::default()
        }
    }

    /// A key arrived.
    pub fn key_pressed(&mut self) {
        if self.log.is_some() {
            self.key_at = Some(Instant::now());
        }
    }

    /// The frame showing it started drawing: the wait ends here.
    pub fn frame_started(&mut self) {
        if let Some(at) = self.key_at {
            self.frame_wait = Some(at.elapsed().as_secs_f32() * 1000.);
        }
    }

    /// An element painted last in the frame, which logs the key's time.
    pub fn frame_probe(&mut self) -> Option<AnyElement> {
        let log = self.log.clone()?;
        let key_at = self.key_at.take();
        let wait = self.frame_wait.take().unwrap_or(0.);
        Some(
            gpui::canvas(
                |_, _, _| (),
                move |_, _, _, _| {
                    if let Some(at) = key_at {
                        log.borrow_mut()
                            .push((at.elapsed().as_secs_f32() * 1000., wait));
                    }
                },
            )
            .absolute()
            .size_0()
            .into_any_element(),
        )
    }

    /// The summary the bench prints, from `(total, wait)` samples.
    pub fn report(samples: &[(f32, f32)]) -> String {
        let stats = |mut xs: Vec<f32>| {
            xs.sort_by(f32::total_cmp);
            let at = |q: f32| {
                xs.get(((xs.len() as f32 - 1.) * q) as usize)
                    .copied()
                    .unwrap_or(0.)
            };
            format!(
                "p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms",
                at(0.5),
                at(0.95),
                xs.last().copied().unwrap_or(0.)
            )
        };
        format!(
            "mnote latency over {} keys\n  mNote's work (key to painted frame, less the wait): {}\n  waiting for the display's next frame:               {}\n  keystroke to painted frame:                        {}",
            samples.len(),
            stats(samples.iter().map(|(t, w)| t - w).collect()),
            stats(samples.iter().map(|(_, w)| *w).collect()),
            stats(samples.iter().map(|(t, _)| *t).collect()),
        )
    }
}

// ---- running them -----------------------------------------------------------

impl Editor {
    /// Counts the focused note's words in the background, at most once per
    /// pause in typing, never on the keystroke path.
    pub(super) fn count_words_soon(&mut self, cx: &mut Context<Self>) {
        if !self
            .words
            .wants(&self.source.doc_key(), self.buffer.revision())
        {
            return;
        }
        self.words.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(WORD_COUNT_DELAY).await;
            // The text as it is now, after the pause.
            let Ok((key, revision, text)) = this.update(cx, |this, _| {
                (
                    this.source.doc_key(),
                    this.buffer.revision(),
                    this.buffer.text().clone(),
                )
            }) else {
                return;
            };
            let words = cx
                .background_executor()
                .spawn(async move { mnote_core::text::count_words(&text) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.words.store(key, revision, words);
                cx.notify();
            });
        }));
    }

    /// The blame label, as a decoration at the right of the cursor line: only
    /// for the line it's about, with blame on and not while inserting.
    pub(super) fn show_blame(&mut self) {
        self.buffer.marks_mut().clear("blame");
        if !self.config.git_blame || self.vim.mode() == Mode::Insert {
            return;
        }
        let rope = self.buffer.text();
        let line = line_of(rope, self.vim.cursor());
        let Some(label) = self.blame.label_for(&self.source.doc_key(), line) else {
            return;
        };
        let start = line_start(rope, line);
        let label = label.to_string();
        self.buffer.marks_mut().add(
            "blame",
            start..start,
            mnote_core::MarkKind::VirtualText {
                text: label,
                color: "ink_3".into(),
                right: true,
            },
        );
    }

    /// Asks git who last changed the cursor line, once the cursor rests on
    /// it (not while typing), off the UI thread.
    pub(super) fn blame_soon(&mut self, cx: &mut Context<Self>) {
        self.show_blame();
        if !self.config.git_blame || self.vim.mode() == Mode::Insert || self.blame.task.is_some() {
            return;
        }
        let Source::Note { .. } = &self.source else {
            return;
        };
        let key = self.source.doc_key();
        let line = line_of(self.buffer.text(), self.vim.cursor());
        if self.blame.knows(&key, line) {
            return;
        }
        self.blame.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(BLAME_DELAY).await;
            let Ok(Some((path, text, line, key))) = this.update(cx, |this, _| {
                let Source::Note { vault, note } = &this.source else {
                    return None;
                };
                let line = line_of(this.buffer.text(), this.vim.cursor());
                Some((
                    vault.path_of(note),
                    this.buffer.text().clone(),
                    line,
                    this.source.doc_key(),
                ))
            }) else {
                let _ = this.update(cx, |this, _| this.blame.task = None);
                return;
            };
            let label = cx
                .background_executor()
                .spawn(async move {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs() as i64);
                    mnote_core::blame::blame_line(&path, &text.to_string(), line)
                        .map(|b| SharedString::from(b.label(now)))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.blame.known = Some((key, line, label));
                this.blame.task = None;
                this.show_blame();
                cx.notify();
            });
        }));
    }

    /// `MNOTE_LATENCY_BENCH=n`: types `n` characters into this window, one
    /// every 25 ms, then prints keystroke-to-pixels times and quits. The
    /// time splits into mNote's work (handling, render, layout, paint),
    /// which the 8 ms budget is for, and the wait for the display's next
    /// frame, which every app has (up to one refresh, 8.3 ms at 120 Hz).
    pub fn run_latency_bench(&mut self, keys: usize, cx: &mut Context<Self>) {
        let log = self.latency.log.get_or_insert_with(Rc::default).clone();
        self._tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let _ = this.update(cx, |this, cx| {
                this.press(mnote_vim::Key::Char('G'), cx);
                this.press(mnote_vim::Key::Char('o'), cx);
            });
            for i in 0..keys {
                let c = if i % 8 == 7 {
                    ' '
                } else {
                    (b'a' + (i % 26) as u8) as char
                };
                let _ = this.update(cx, |this, cx| {
                    this.latency.key_at = Some(Instant::now());
                    this.press(mnote_vim::Key::Char(c), cx);
                });
                cx.background_executor()
                    .timer(Duration::from_millis(25))
                    .await;
            }
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            // The first keys warm caches up; they aren't typing.
            let samples: Vec<(f32, f32)> = log.borrow().iter().skip(10).copied().collect();
            eprintln!("{}", LatencyProbe::report(&samples));
            let _ = this.update(cx, |_, cx| cx.quit());
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnote_core::NotePath;

    fn doc(name: &str) -> DocKey {
        DocKey::Note(NotePath::new(name).expect("a note path"))
    }

    #[test]
    fn word_counts_belong_to_their_note_and_revision() {
        let mut words = WordCount::default();
        assert!(words.wants(&doc("a.md"), 1));
        words.store(doc("a.md"), 1, 42);
        assert_eq!(words.count_for(&doc("a.md")), Some(42));
        assert_eq!(words.count_for(&doc("b.md")), None);
        assert!(!words.wants(&doc("a.md"), 1), "already counted");
        assert!(words.wants(&doc("a.md"), 2), "the text changed");
    }

    #[test]
    fn blame_labels_belong_to_their_line() {
        let mut blame = Blame::default();
        assert!(!blame.knows(&doc("a.md"), 3));
        blame.known = Some((doc("a.md"), 3, Some("Ann, 2 days ago".into())));
        assert_eq!(
            blame.label_for(&doc("a.md"), 3).map(|l| l.as_ref()),
            Some("Ann, 2 days ago")
        );
        assert_eq!(blame.label_for(&doc("a.md"), 4), None);
        blame.known = Some((doc("a.md"), 3, None));
        assert!(blame.knows(&doc("a.md"), 3), "asked, git didn't know");
        assert_eq!(blame.label_for(&doc("a.md"), 3), None);
    }

    #[test]
    fn the_latency_report_splits_work_from_the_wait() {
        let report = LatencyProbe::report(&[(5.0, 4.0), (3.0, 2.0), (9.0, 1.0)]);
        assert!(report.starts_with("mnote latency over 3 keys"));
        assert!(report.contains("less the wait): p50 1.00 ms, p95 1.00 ms, max 8.00 ms"));
        let mut probe = LatencyProbe::default();
        probe.key_pressed();
        assert!(probe.key_at.is_none(), "off without MNOTE_LATENCY");
        assert!(probe.frame_probe().is_none());
    }
}
