//! The picker shell every list dialog shares (finder, palette, backlinks,
//! explorer): a `Modal` on the scrim with a Vim query field (D-001), the
//! result list, a preview pane and a hint bar that follows the field's mode.
//!
//! What a picker lists comes from its [`PickerDelegate`]; the shell owns the
//! query, the selection, scrolling and all the drawing. It isn't a view of its
//! own: the window that opens it routes keys here and draws [`Picker::render`]
//! on top of itself.

pub mod backlinks;
pub mod explorer;
pub mod finder;
pub mod list;
pub mod palette;

use std::ops::Range;

use gpui::{
    AnyElement, Font, FontWeight, HighlightStyle, IntoElement, ParentElement, SharedString, Styled,
    StyledText, div, font, prelude::FluentBuilder, px,
};
use mnote_core::NotePath;
use mnote_core::content::FileHits;
use mnote_vim::{InputAction, Key, Mode, VimInput};

use crate::editor::line::{Caret, LineText};
use crate::theme::{Palette, StyledExt, Theme, family, radius, size, space, text};
use crate::ui::{Hint, HintBar};

/// Below this window width the preview moves under the list.
const STACK_BELOW: f32 = 700.;
/// Height of the list (and the preview beside it).
const LIST_H: f32 = 340.;
/// Rows of the list when the preview is stacked under it.
const STACKED_LIST_H: f32 = 200.;
const STACKED_PREVIEW_H: f32 = 170.;
/// Space above the modal, between it and the top bar.
const SCRIM_TOP: f32 = 48.;

/// One result row.
#[derive(Debug, Clone, Default)]
pub struct Row {
    pub label: SharedString,
    /// Characters of `label` to set bold (the fuzzy match).
    pub matches: Vec<usize>,
    /// Quiet text at the right end (a folder, an age).
    pub detail: Option<SharedString>,
    /// A secondary row in `ink-2`, such as "create …".
    pub quiet: bool,
    pub kind: RowKind,
    /// Cut with `dd`, waiting for `p` (explorer): drawn on `insert-wash`.
    pub cut: bool,
    /// Byte ranges of `label` on the `match` fill (content search hits).
    pub marks: Vec<Range<usize>>,
}

/// How a row looks and behaves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RowKind {
    /// An ordinary row.
    #[default]
    Item,
    /// An inline name field (explorer rename and new): the label is the
    /// text being typed, `cursor` the caret's byte offset, `block` a NORMAL
    /// mode caret, `suffix` the quiet part after the field (`.md`).
    Field {
        cursor: usize,
        block: bool,
        suffix: &'static str,
    },
    /// A heading over the rows below it (a file in content search). It can't
    /// be selected.
    Group,
    /// A matching line, shown in mono after its line number (1-based).
    Hit { line: usize },
}

/// Lines of a file around a hit, for the preview.
#[derive(Debug, Clone, Default)]
pub struct CodePreview {
    /// `(line number, text, marked byte ranges)`, 1-based line numbers.
    pub lines: Vec<(usize, SharedString, Vec<Range<usize>>)>,
    /// The line number of the hit, drawn on `well`.
    pub current: usize,
}

/// What the preview pane shows for the selected row.
#[derive(Debug, Clone, Default)]
pub struct PreviewContent {
    pub title: SharedString,
    pub meta: Option<SharedString>,
    pub lines: Vec<SharedString>,
    pub code: Option<CodePreview>,
}

/// What confirming a row asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    OpenNote(NotePath),
    /// Create the note (it doesn't exist yet), then open it.
    CreateNote(NotePath),
    /// Move the cursor to this line (0-based) of the open note.
    GoToLine(usize),
    /// Run an ex command, as if typed after `:`.
    RunEx(String),
    /// Open a note at a line (0-based).
    JumpTo {
        path: NotePath,
        line: usize,
    },
    /// Keep these places as the results list (`⌃q`), and go to the first.
    SendToList(Vec<ListEntry>),
    /// Go to entry `n` of the results list (`:copen`).
    ListGo(usize),
    /// Run [`crate::commands::COMMANDS`]`[n]`.
    Command(usize),
    /// Edit this folder (vault-relative) as text.
    EditFolder(String),
}

/// A place in the results list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntry {
    pub path: NotePath,
    /// 0-based.
    pub line: usize,
    /// The line's text, as it was when found.
    pub text: String,
}

/// Work a picker hands off to a background thread.
pub type Job = Box<dyn FnOnce() -> Option<JobResult> + Send>;

/// What a [`Job`] found.
#[derive(Debug)]
pub enum JobResult {
    /// Content search results for the search started in `generation`.
    Content {
        generation: u64,
        files: Vec<FileHits>,
    },
    /// Plain-text mentions of a note, for backlinks.
    Mentions(Vec<(NotePath, mnote_core::mentions::Mention)>),
    /// Links rewritten after a rename or move; `message` is the toast to
    /// show, with the link count added.
    Relinked {
        done: Vec<mnote_core::links::Relinked>,
        message: String,
        undo: bool,
    },
}

/// Something a picker did that the window around it must know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerEffect {
    /// A short message at the bottom of the window; `undo` adds `u undo`.
    Toast { text: String, undo: bool },
    /// A note or folder moved (vault-relative paths): an open note inside it
    /// follows.
    Moved {
        from: std::path::PathBuf,
        to: std::path::PathBuf,
    },
    /// Notes saved with rewritten links: an open one reloads.
    Relinked(Vec<mnote_core::links::Relinked>),
}

/// What confirming a row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    /// Close the picker and do this.
    Act(PickerAction),
    /// Stay open with this query (a tag narrows the finder to its notes).
    SetQuery(String),
    /// Stay open: the delegate moved (the explorer entered a folder). The
    /// filter clears and row `select` is selected.
    Navigate {
        select: usize,
    },
    /// Nothing to do; stay open.
    Stay,
    Close,
}

/// The result of a key in the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerOutcome {
    /// Still open; redraw.
    Open,
    Close,
    /// Close and do this.
    Act(PickerAction),
    /// `⌃v` / `⌃x`: close and do this in a new pane beside (`vertical`) or
    /// below the focused one.
    ActInSplit {
        action: PickerAction,
        vertical: bool,
    },
}

/// What a particular picker lists and does.
pub trait PickerDelegate {
    /// The character before the query (`›`, `#`, `@`), and how many bytes
    /// at the start of `query` it stands for. A scope character typed first
    /// shows as the prefix instead of in the text.
    fn prefix(&self, _query: &str) -> (&'static str, usize) {
        ("›", 0)
    }

    /// An optional title row: `(title, quieter subtitle)`.
    fn title(&self) -> Option<(SharedString, Option<SharedString>)> {
        None
    }

    /// Quiet text at the right of the query field.
    fn scope(&self, _mode: Mode) -> Option<SharedString> {
        None
    }

    /// The query changed; recompute the results.
    fn set_query(&mut self, query: &str);

    /// Number of results.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn row(&self, ix: usize) -> Row;

    fn preview(&self, ix: usize) -> Option<PreviewContent>;

    /// `enter` (or `l` in NORMAL) on row `ix`.
    fn confirm(&mut self, ix: usize) -> Confirm;

    /// Keys the hint bar shows, after the list keys the shell adds.
    fn hints(&self, _mode: Mode) -> Vec<Hint> {
        vec![Hint::new("⏎", "open")]
    }

    /// The count at the right of the hint bar.
    fn count(&self) -> Option<SharedString> {
        None
    }

    /// Whether the dialog has a preview pane beside (or under) the list.
    fn has_preview(&self) -> bool {
        true
    }

    /// A line under the list about the selected row (the palette's
    /// description), for dialogs without a preview.
    fn footer(&self, _ix: usize) -> Option<SharedString> {
        None
    }

    /// The dialog's width, if narrower than `modal-w`.
    fn width(&self) -> Option<f32> {
        None
    }

    /// NORMAL-mode letters this picker handles itself (the explorer's `h`).
    /// `l` opens unless the picker claims it too.
    fn claims(&self, _c: char) -> bool {
        false
    }

    /// A claimed letter typed in NORMAL mode, with row `ix` selected.
    fn claimed(&mut self, _c: char, _ix: usize) -> Confirm {
        Confirm::Stay
    }

    /// Whether `esc` in NORMAL mode first clears a non-empty filter before a
    /// second `esc` closes (the explorer, per D-001).
    fn clear_on_escape(&self) -> bool {
        false
    }

    /// Whether row `ix` can be selected (group rows can't).
    fn selectable(&self, _ix: usize) -> bool {
        true
    }

    /// Background work to start after the query changed, if any.
    fn take_job(&mut self) -> Option<Job> {
        None
    }

    /// A job's result arrived.
    fn job_done(&mut self, _result: JobResult) {}

    /// Whether every key goes to [`Self::key`] instead of the filter (an
    /// inline name field is open).
    fn capturing(&self) -> bool {
        false
    }

    /// A key while [`Self::capturing`].
    fn key(&mut self, _key: Key) -> Confirm {
        Confirm::Stay
    }

    /// Effects since the last call, for the window to apply.
    fn take_effects(&mut self) -> Vec<PickerEffect> {
        Vec::new()
    }

    /// `⌃q`: the results as a list, if this picker has places to list.
    fn send_to_list(&mut self) -> Option<PickerAction> {
        None
    }
}

/// An open picker.
pub struct Picker {
    input: VimInput,
    delegate: Box<dyn PickerDelegate>,
    selected: usize,
    /// First visible row.
    scroll: usize,
}

impl Picker {
    pub fn new(mut delegate: Box<dyn PickerDelegate>) -> Self {
        delegate.set_query("");
        Picker {
            input: VimInput::default(),
            delegate,
            selected: 0,
            scroll: 0,
        }
    }

    pub fn mode(&self) -> Mode {
        self.input.mode()
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Handles one key.
    pub fn handle_key(&mut self, key: Key) -> PickerOutcome {
        if self.delegate.capturing() {
            let confirm = self.delegate.key(key);
            return self.apply(confirm);
        }
        if let Key::Ctrl(c @ ('v' | 'x')) = key {
            if self.delegate.is_empty() || !self.delegate.selectable(self.selected) {
                return PickerOutcome::Open;
            }
            return match self.delegate.confirm(self.selected) {
                Confirm::Act(
                    action @ (PickerAction::OpenNote(_)
                    | PickerAction::CreateNote(_)
                    | PickerAction::JumpTo { .. }),
                ) => PickerOutcome::ActInSplit {
                    action,
                    vertical: c == 'v',
                },
                // Rows that don't open a note: as if ⏎.
                confirm => self.apply(confirm),
            };
        }
        if key == Key::Ctrl('q') {
            return match self.delegate.send_to_list() {
                Some(action) => PickerOutcome::Act(action),
                None => PickerOutcome::Open,
            };
        }
        let delegate = &self.delegate;
        match self
            .input
            .handle_key(key, |c| c == 'l' || delegate.claims(c))
        {
            InputAction::Edited => {
                self.delegate.set_query(&self.input.text());
                self.reset_selection();
            }
            InputAction::Moved => {}
            InputAction::Select(by) => self.move_selection(by),
            InputAction::Claimed('l') if self.delegate.claims('l') => {
                let confirm = self.delegate.claimed('l', self.selected);
                return self.apply(confirm);
            }
            InputAction::Submit | InputAction::Claimed('l') => {
                if self.delegate.is_empty() || !self.delegate.selectable(self.selected) {
                    return PickerOutcome::Open;
                }
                let confirm = self.delegate.confirm(self.selected);
                return self.apply(confirm);
            }
            InputAction::Claimed(c) => {
                let confirm = self.delegate.claimed(c, self.selected);
                return self.apply(confirm);
            }
            InputAction::Close => {
                if self.delegate.clear_on_escape() && !self.input.text().is_empty() {
                    self.input.clear();
                    self.delegate.set_query("");
                    self.reset_selection();
                    return PickerOutcome::Open;
                }
                return PickerOutcome::Close;
            }
        }
        PickerOutcome::Open
    }

    fn apply(&mut self, confirm: Confirm) -> PickerOutcome {
        match confirm {
            Confirm::Act(action) => PickerOutcome::Act(action),
            Confirm::SetQuery(query) => {
                self.set_query(&query);
                PickerOutcome::Open
            }
            Confirm::Navigate { select } => {
                self.input.clear();
                self.delegate.set_query("");
                self.reset_selection();
                if select < self.delegate.len() && self.delegate.selectable(select) {
                    self.selected = select;
                }
                PickerOutcome::Open
            }
            Confirm::Stay => PickerOutcome::Open,
            Confirm::Close => PickerOutcome::Close,
        }
    }

    /// Selects row `ix`, if it can be.
    pub fn select(&mut self, ix: usize) {
        if ix < self.delegate.len() && self.delegate.selectable(ix) {
            self.selected = ix;
        }
    }

    #[cfg(test)]
    pub(crate) fn row_label(&self, ix: usize) -> String {
        self.delegate.row(ix).label.to_string()
    }

    /// Replaces the query (keeping the field in INSERT) and refreshes.
    pub fn set_query(&mut self, query: &str) {
        self.input = VimInput::new(query);
        self.delegate.set_query(query);
        self.reset_selection();
    }

    /// What the picker did that the window must apply.
    pub fn take_effects(&mut self) -> Vec<PickerEffect> {
        self.delegate.take_effects()
    }

    /// Background work the delegate wants done now.
    pub fn take_job(&mut self) -> Option<Job> {
        self.delegate.take_job()
    }

    /// Hands a finished job back to the delegate.
    pub fn job_done(&mut self, result: JobResult) {
        self.delegate.job_done(result);
        self.reset_selection();
    }

    /// The first selectable row, at the top of the list.
    fn reset_selection(&mut self) {
        self.selected = 0;
        self.scroll = 0;
        if !self.delegate.is_empty() && !self.delegate.selectable(0) {
            self.move_selection(1);
        }
    }

    /// Moves the selection over selectable rows, wrapping at both ends as
    /// Vim's completion menu does.
    fn move_selection(&mut self, by: isize) {
        let len = self.delegate.len();
        if len == 0 {
            return;
        }
        let step = by.signum();
        let mut left = by.unsigned_abs();
        let mut at = self.selected;
        for _ in 0..len * by.unsigned_abs().max(1) {
            at = (at as isize + step).rem_euclid(len as isize) as usize;
            if self.delegate.selectable(at) {
                left -= 1;
                if left == 0 {
                    break;
                }
            }
        }
        if self.delegate.selectable(at) {
            self.selected = at;
        }
    }

    /// Keeps the selection inside the `rows` visible ones.
    fn visible_rows(&mut self, rows: usize) -> Range<usize> {
        let rows = rows.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
        self.scroll..(self.scroll + rows).min(self.delegate.len())
    }

    /// The scrim, the modal and everything in it, for a window
    /// `window_width` wide. `caret_on` is the blink phase of the bar caret.
    pub fn render(&mut self, window_width: f32, caret_on: bool, p: &Palette) -> AnyElement {
        let stacked = window_width < STACK_BELOW;
        let list_h = if stacked { STACKED_LIST_H } else { LIST_H };
        let rows_fit = ((list_h - 12.) / (size::ROW_H + 1.)).floor() as usize;
        let rows = self.visible_rows(rows_fit);
        let mode = self.input.mode();

        let list = div()
            .flex()
            .flex_col()
            .gap(px(1.))
            .p(px(6.))
            .min_w_0()
            .overflow_hidden()
            .children(
                rows.map(|ix| render_row(&self.delegate.row(ix), ix == self.selected, caret_on, p)),
            );
        let preview = self
            .delegate
            .preview(self.selected)
            .filter(|_| !self.delegate.is_empty())
            .map(|content| render_preview(content, p));

        let body = if !self.delegate.has_preview() {
            div().flex().flex_col().child(list.h(px(list_h))).children(
                self.delegate
                    .footer(self.selected)
                    .filter(|_| !self.delegate.is_empty())
                    .map(|about| {
                        div()
                            .px(px(14.))
                            .pt(px(space::S2))
                            .pb(px(10.))
                            .border_t_1()
                            .border_color(Theme::color(p.line))
                            .text_size(px(13.))
                            .line_height(px(20.))
                            .text_color(Theme::color(p.ink_2))
                            .child(about)
                    }),
            )
        } else if stacked {
            div()
                .flex()
                .flex_col()
                .child(
                    list.h(px(list_h))
                        .border_b_1()
                        .border_color(Theme::color(p.line)),
                )
                .children(preview.map(|pv| div().h(px(STACKED_PREVIEW_H)).child(pv)))
        } else {
            div()
                .flex()
                .h(px(list_h))
                // The list gets 1 part to the preview's 1.25.
                .child(
                    list.flex_none()
                        .w(gpui::relative(1. / 2.25))
                        .border_r_1()
                        .border_color(Theme::color(p.line)),
                )
                .child(div().flex_1().min_w_0().children(preview))
        };

        let capturing = self.delegate.capturing();
        let mut hints = match (capturing, mode) {
            (true, _) => Vec::new(),
            (false, Mode::Normal) => vec![Hint::new("j k", "move")],
            (false, _) => vec![Hint::new("⌃n ⌃p", "move")],
        };
        hints.extend(self.delegate.hints(mode));
        if !capturing {
            hints.push(match mode {
                Mode::Normal => Hint::new("esc", "close"),
                _ => Hint::new("esc", "normal mode"),
            });
        }
        let mut hint_bar = HintBar::new(hints);
        if let Some(count) = self.delegate.count() {
            hint_bar = hint_bar.right(count);
        }

        let modal = div()
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(self.delegate.width().unwrap_or(size::MODAL_W)))
            .bg(Theme::color(p.overlay))
            .border_1()
            .border_color(Theme::color(p.line_strong))
            .rounded(px(radius::LG))
            .overflow_hidden()
            .text_color(Theme::color(p.ink))
            .children(self.delegate.title().map(|(title, sub)| {
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(space::S2))
                    .p(px(space::S3))
                    .border_b_1()
                    .border_color(Theme::color(p.line))
                    .text_style(text::UI_STRONG)
                    .child(title)
                    .children(sub.map(|sub| {
                        div()
                            .text_style(text::STATUS)
                            .text_color(Theme::color(p.ink_3))
                            .child(sub)
                    }))
            }))
            .child(self.render_input(mode, caret_on, p))
            .child(body)
            .child(hint_bar);

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(SCRIM_TOP))
            .px(px(space::S4))
            .bg(Theme::color(p.scrim))
            .child(modal)
            .into_any_element()
    }

    /// The query field: prefix, the text with a bar (INSERT) or block (NORMAL)
    /// caret, and the scope at the right. No mode pill (D-001).
    fn render_input(&self, mode: Mode, caret_on: bool, p: &Palette) -> impl IntoElement {
        let style = text::INPUT;
        let full = self.input.text();
        // A scope character at the start shows as the prefix instead.
        let (prefix, skip) = self.delegate.prefix(&full);
        let skip_chars = full[..skip].chars().count();
        let query: SharedString = full[skip..].to_owned().into();
        let cursor = self.input.cursor().saturating_sub(skip_chars);
        let byte = |chars: usize| {
            query
                .char_indices()
                .nth(chars)
                .map_or(query.len(), |(b, _)| b)
        };
        let mono = Font {
            weight: style.font_weight(),
            ..font(family::MONO)
        };
        let mut line = LineText::new(
            query.clone(),
            mono,
            style.size(),
            style.line_height(),
            Theme::color(p.ink),
        );
        if let Some(sel) = self.input.selection() {
            let (from, to) = (
                sel.start.saturating_sub(skip_chars),
                sel.end.saturating_sub(skip_chars),
            );
            line = line.selection(byte(from)..byte(to), Theme::color(p.select));
        }
        if caret_on || mode != Mode::Insert {
            line = line.caret(Some(Caret {
                byte: byte(cursor),
                shape: mode.caret(),
                color: Theme::color(p.cursor),
                on_color: Theme::color(p.on_cursor),
            }));
        }
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(space::S2))
            .h(px(44.))
            .px(px(space::S3))
            .border_b_1()
            .border_color(Theme::color(p.line))
            .text_style(style)
            .child(
                div()
                    .flex_none()
                    .text_color(Theme::color(p.cursor))
                    .font_weight(FontWeight::MEDIUM)
                    .child(SharedString::new_static(prefix)),
            )
            .child(div().flex_1().min_w_0().child(line))
            .children(self.delegate.scope(mode).map(|scope| {
                div()
                    .flex_none()
                    .text_style(text::HINT)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(Theme::color(p.ink_3))
                    .child(scope)
            }))
    }
}

/// Bold for fuzzy-matched characters, the `match` fill for marked ranges.
fn label_styles(row: &Row, strong: u32, p: &Palette) -> Vec<(Range<usize>, HighlightStyle)> {
    let label = &row.label;
    let mut styles: Vec<(Range<usize>, HighlightStyle)> = label
        .char_indices()
        .enumerate()
        .filter(|(i, _)| row.matches.contains(i))
        .map(|(_, (b, c))| {
            let style = HighlightStyle {
                font_weight: Some(FontWeight::SEMIBOLD),
                color: Some(Theme::color(strong)),
                ..HighlightStyle::default()
            };
            (b..b + c.len_utf8(), style)
        })
        .collect();
    styles.extend(marks(&row.marks, label.len(), p));
    styles
}

/// Match-fill styles for byte ranges, clipped to `len` and kept on char
/// boundaries by the caller.
fn marks(ranges: &[Range<usize>], len: usize, p: &Palette) -> Vec<(Range<usize>, HighlightStyle)> {
    ranges
        .iter()
        .filter(|r| r.start < r.end && r.end <= len)
        .map(|r| {
            let style = HighlightStyle {
                background_color: Some(Theme::color(p.match_)),
                color: Some(Theme::color(p.ink)),
                ..HighlightStyle::default()
            };
            (r.clone(), style)
        })
        .collect()
}

/// An inline name field: bordered in `cursor`, mono, the caret in its mode,
/// and a quiet suffix after it.
fn render_field(
    row: &Row,
    cursor: usize,
    block: bool,
    suffix: &'static str,
    caret_on: bool,
    p: &Palette,
) -> AnyElement {
    let style = text::CODE;
    let mono = Font {
        weight: style.font_weight(),
        ..font(family::MONO)
    };
    let mut line = LineText::new(
        row.label.clone(),
        mono,
        style.size(),
        px(22.),
        Theme::color(p.ink),
    );
    if caret_on || block {
        line = line.caret(Some(Caret {
            byte: cursor.min(row.label.len()),
            shape: if block {
                mnote_vim::CaretShape::Block
            } else {
                mnote_vim::CaretShape::Bar
            },
            color: Theme::color(p.cursor),
            on_color: Theme::color(p.on_cursor),
        }));
    }
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(size::ROW_H))
        .px(px(space::S2))
        .rounded(px(radius::SM))
        .border_1()
        .border_color(Theme::color(p.cursor))
        .bg(Theme::color(p.overlay))
        .text_style(style)
        .child(div().flex_1().min_w_0().child(line))
        .child(
            div()
                .flex_none()
                .text_color(Theme::color(p.ink_3))
                .child(SharedString::new_static(suffix)),
        )
        .into_any_element()
}

fn render_row(row: &Row, selected: bool, caret_on: bool, p: &Palette) -> AnyElement {
    if let RowKind::Field {
        cursor,
        block,
        suffix,
    } = row.kind
    {
        return render_field(row, cursor, block, suffix, caret_on, p);
    }
    if row.kind == RowKind::Group {
        return div()
            .flex()
            .flex_none()
            .items_baseline()
            .gap(px(6.))
            .pt(px(6.))
            .pb(px(2.))
            .px(px(space::S2))
            .whitespace_nowrap()
            .overflow_hidden()
            .text_style(text::UI_STRONG)
            .text_color(Theme::color(p.ink))
            .child(row.label.clone())
            .children(row.detail.clone().map(|detail| {
                div()
                    .text_style(text::HINT)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(Theme::color(p.ink_3))
                    .child(detail)
            }))
            .into_any_element();
    }
    let (ink, quiet, strong) = match (selected, row.cut, row.quiet) {
        (true, _, _) => (p.select_ink, p.select_ink, p.select_ink),
        (false, true, _) => (p.insert, p.insert, p.insert),
        (false, false, true) => (p.ink_2, p.ink_3, p.ink),
        (false, false, false) => (p.ink, p.ink_3, p.ink),
    };
    let styles = label_styles(row, strong, p);
    let hit_line = match row.kind {
        RowKind::Hit { line } => Some(line),
        _ => None,
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(space::S2))
        .h(px(size::ROW_H))
        .px(px(space::S2))
        .rounded(px(radius::SM))
        .whitespace_nowrap()
        .overflow_hidden()
        .text_style(text::UI)
        .when(hit_line.is_some(), |row| {
            row.text_style(text::STATUS).line_height(px(22.))
        })
        .text_color(Theme::color(ink))
        .when(row.cut && !selected, |r| r.bg(Theme::color(p.insert_wash)))
        .when(selected, |row| row.bg(Theme::color(p.select)))
        .children(hit_line.map(|line| {
            div()
                .flex_none()
                .min_w(px(26.))
                .text_color(Theme::color(quiet))
                .child(SharedString::from(line.to_string()))
        }))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .child(StyledText::new(row.label.clone()).with_highlights(styles)),
        )
        .child(div().flex_1())
        .children(row.detail.clone().map(|detail| {
            div()
                .flex_none()
                .text_style(text::HINT)
                .font_weight(FontWeight::NORMAL)
                .text_color(Theme::color(quiet))
                .child(detail)
        }))
        .into_any_element()
}

fn render_preview(content: PreviewContent, p: &Palette) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .size_full()
        .min_w_0()
        .overflow_hidden()
        .px(px(space::S4))
        .py(px(space::S3))
        .text_style(text::UI)
        .text_color(Theme::color(p.ink_2))
        .child(
            div()
                .text_size(px(17.))
                .line_height(px(24.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(Theme::color(p.ink))
                .child(content.title),
        )
        .children(content.meta.map(|meta| {
            div()
                .mt(px(2.))
                .mb(px(space::S2))
                .text_style(text::HINT)
                .font_weight(FontWeight::NORMAL)
                .text_color(Theme::color(p.ink_3))
                .child(meta)
        }))
        .children(
            content
                .lines
                .into_iter()
                .map(|line| div().whitespace_nowrap().overflow_hidden().child(line)),
        )
        .children(content.code.map(|code| {
            div()
                .flex()
                .flex_col()
                .text_style(text::STATUS)
                .line_height(px(22.))
                .children(code.lines.into_iter().map(|(number, line, ranges)| {
                    let current = number == code.current;
                    let styles = marks(&ranges, line.len(), p);
                    div()
                        .flex()
                        .gap(px(space::S2))
                        .rounded(px(radius::SM))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .when(current, |row| {
                            row.bg(Theme::color(p.well)).text_color(Theme::color(p.ink))
                        })
                        .child(
                            div()
                                .flex_none()
                                .min_w(px(32.))
                                .text_right()
                                .text_color(Theme::color(p.ink_3))
                                .child(SharedString::from(number.to_string())),
                        )
                        .child(StyledText::new(line).with_highlights(styles))
                }))
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fruits {
        all: Vec<&'static str>,
        shown: Vec<&'static str>,
    }

    impl PickerDelegate for Fruits {
        fn set_query(&mut self, query: &str) {
            self.shown = self
                .all
                .iter()
                .copied()
                .filter(|f| f.contains(query))
                .collect();
        }
        fn len(&self) -> usize {
            self.shown.len()
        }
        fn row(&self, ix: usize) -> Row {
            Row {
                label: self.shown[ix].into(),
                ..Row::default()
            }
        }
        fn preview(&self, _ix: usize) -> Option<PreviewContent> {
            None
        }
        fn confirm(&mut self, ix: usize) -> Confirm {
            match self.shown[ix] {
                "cherry" => Confirm::SetQuery("ch".into()),
                name => NotePath::new(format!("{name}.md"))
                    .map_or(Confirm::Close, |p| Confirm::Act(PickerAction::OpenNote(p))),
            }
        }
    }

    fn picker() -> Picker {
        Picker::new(Box::new(Fruits {
            all: vec!["apple", "banana", "cherry", "grape"],
            shown: Vec::new(),
        }))
    }

    fn keys(p: &mut Picker, keys: &str) -> PickerOutcome {
        let mut last = PickerOutcome::Open;
        for key in mnote_vim::parse_keys(keys) {
            last = p.handle_key(key);
        }
        last
    }

    #[test]
    fn typing_filters_and_resets_the_selection() {
        let mut p = picker();
        keys(&mut p, "<c-n><c-n>");
        assert_eq!(p.selected(), 2);
        keys(&mut p, "an");
        assert_eq!((p.delegate.len(), p.selected()), (1, 0));
        assert_eq!(
            keys(&mut p, "<cr>"),
            PickerOutcome::Act(PickerAction::OpenNote(
                NotePath::new("banana.md").expect("path")
            ))
        );
    }

    #[test]
    fn ctrl_v_and_ctrl_x_open_in_a_split() {
        let mut p = picker();
        let apple = PickerAction::OpenNote(NotePath::new("apple.md").expect("path"));
        assert_eq!(
            keys(&mut p, "<c-v>"),
            PickerOutcome::ActInSplit {
                action: apple.clone(),
                vertical: true
            }
        );
        let mut p = picker();
        assert_eq!(
            keys(&mut p, "<c-x>"),
            PickerOutcome::ActInSplit {
                action: apple,
                vertical: false
            }
        );
        let mut p = picker();
        keys(&mut p, "<c-n><c-n>");
        assert_eq!(
            keys(&mut p, "<c-v>"),
            PickerOutcome::Open,
            "a row that doesn't open a note acts as ⏎"
        );
    }

    #[test]
    fn modes_move_the_list_and_close() {
        let mut p = picker();
        keys(&mut p, "<c-p>");
        assert_eq!(p.selected(), 3, "wraps to the end");
        keys(&mut p, "<esc>");
        assert_eq!(p.mode(), Mode::Normal);
        keys(&mut p, "jj");
        assert_eq!(p.selected(), 1);
        assert!(
            matches!(keys(&mut p, "l"), PickerOutcome::Act(_)),
            "l opens in NORMAL"
        );
        let mut p = picker();
        assert_eq!(keys(&mut p, "<esc><esc>"), PickerOutcome::Close);
        let mut p = picker();
        assert_eq!(
            keys(&mut p, "zzz<cr>"),
            PickerOutcome::Open,
            "nothing to open"
        );
    }

    #[test]
    fn confirming_can_change_the_query() {
        let mut p = picker();
        assert_eq!(keys(&mut p, "cher<cr>"), PickerOutcome::Open);
        assert_eq!(p.input.text(), "ch");
        assert_eq!((p.delegate.len(), p.mode()), (1, Mode::Insert));
    }

    #[test]
    fn scrolling_keeps_the_selection_in_view() {
        let mut p = picker();
        p.selected = 3;
        assert_eq!(p.visible_rows(2), 2..4);
        p.selected = 0;
        assert_eq!(p.visible_rows(2), 0..2);
    }
}
