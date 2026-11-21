//! The finder (`space f`): one picker, four scopes chosen by the first
//! character of the query.
//!
//! | Prefix | Lists |
//! | --- | --- |
//! | (none, shown as `›`) | notes by path, title or alias, recent first; a last row creates a note |
//! | `#` | tags with their note counts; `⏎` narrows to the tag's notes (`#tag query`) |
//! | `@` | headings in the open note, in order; `⏎` jumps to one |
//! | `>` | commands, each with its ex equivalent |
//! | `/` | lines of note text, grouped by note; `⏎` jumps there, `⌃q` keeps them as a list |
//!
//! Everything but `/` comes from the note index and the open note's text, so
//! moving through results never reads the disk. `/` searches the files on a
//! background thread; each new keystroke cancels the search before it.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::SystemTime;

use gpui::SharedString;
use mnote_core::content::{Cancel, ContentQuery, FileHits, search_notes};
use mnote_core::index::{HeadingHit, HitField, NoteHit, StringHit, TagHit};
use mnote_core::{NoteEntry, NoteIndex, NotePath, NoteSearch, Vault};
use mnote_vim::Mode;

use super::{
    CodePreview, Confirm, Job, JobResult, ListEntry, PickerAction, PickerDelegate, PreviewContent,
    Row, RowKind,
};
use crate::ui::Hint;

/// Results kept per query; more than anyone scrolls through.
const LIMIT: usize = 200;
/// Headings (or lines) a preview lists.
const PREVIEW_LINES: usize = 12;
/// Content hits kept per search, across all notes.
const CONTENT_LIMIT: usize = 500;
/// Characters of a hit line shown in its row.
const HIT_CHARS: usize = 160;
/// Lines shown above a hit in the preview.
const CONTEXT_ABOVE: usize = 3;

/// Commands the `>` scope offers: label and the ex command that runs it. The
/// full command palette is AMI-167.
const COMMANDS: &[(&str, &str)] = &[
    ("Save note", "w"),
    ("Clear search highlights", "nohlsearch"),
    ("Toggle relative line numbers", "set relativenumber!"),
    ("Toggle ignore case in search", "set ignorecase!"),
    ("Sort lines", "%sort"),
    ("Undo", "undo"),
    ("Redo", "redo"),
    ("Show registers", "registers"),
    ("Quit", "q"),
];

/// The open note, for the `@` scope and the create row.
pub struct CurrentNote {
    pub path: Option<NotePath>,
    pub text: String,
}

enum Results {
    Notes {
        hits: Vec<NoteHit>,
        /// The note the last row would create, if any.
        create: Option<NotePath>,
    },
    Tags(Vec<TagHit>),
    Headings(Vec<HeadingHit>),
    Commands(Vec<StringHit>),
    Content(Content),
}

/// Content search results, as rows: a group row per note, then its hits.
#[derive(Default)]
struct Content {
    files: Vec<FileHits>,
    /// `(file, Some(hit))` for a hit row, `(file, None)` for a group row.
    rows: Vec<(usize, Option<usize>)>,
    /// A search for the current query is running.
    searching: bool,
}

impl Content {
    fn new(files: Vec<FileHits>) -> Self {
        let mut rows = Vec::new();
        for (f, file) in files.iter().enumerate() {
            rows.push((f, None));
            rows.extend((0..file.hits.len()).map(|h| (f, Some(h))));
        }
        Content {
            files,
            rows,
            searching: false,
        }
    }

    fn hit_count(&self) -> usize {
        self.files.iter().map(|f| f.hits.len()).sum()
    }
}

pub struct Finder {
    index: Rc<RefCell<NoteIndex>>,
    /// The open note as a one-note index, for heading search.
    current: NoteIndex,
    current_text: String,
    search: NoteSearch,
    results: Results,
    /// `label :ex` for each command.
    command_haystacks: Vec<String>,
    /// Where `/` searches; `None` for a scratch note.
    vault: Option<Vault>,
    cancel: Cancel,
    /// The generation of the newest content search.
    generation: u64,
    job: Option<Job>,
}

impl Finder {
    pub fn new(index: Rc<RefCell<NoteIndex>>, current: CurrentNote, vault: Option<Vault>) -> Self {
        let path = current
            .path
            .unwrap_or_else(|| NotePath::new("scratch.md").expect("a valid note path"));
        let entry = NoteEntry::new(path, &current.text, None);
        Finder {
            index,
            current: NoteIndex::from_entries(vec![entry]),
            current_text: current.text,
            search: NoteSearch::new(),
            command_haystacks: COMMANDS
                .iter()
                .map(|(label, ex)| format!("{label} :{ex}"))
                .collect(),
            results: Results::Notes {
                hits: Vec::new(),
                create: None,
            },
            vault,
            cancel: Cancel::default(),
            generation: 0,
            job: None,
        }
    }

    /// Starts a content search for `query` on a background job; the results
    /// shown stay until it's done.
    fn content_query(&mut self, query: &str) {
        let generation = self.cancel.next();
        self.generation = generation;
        self.job = None;
        let (Some(content), Some(vault)) = (ContentQuery::new(query), self.vault.clone()) else {
            self.results = Results::Content(Content::default());
            return;
        };
        let notes: Vec<NotePath> = self
            .index
            .borrow()
            .notes()
            .iter()
            .map(|n| n.path.clone())
            .collect();
        let cancel = self.cancel.clone();
        self.job = Some(Box::new(move || {
            search_notes(&vault, &notes, &content, CONTENT_LIMIT, &cancel, generation)
                .map(|files| JobResult::Content { generation, files })
        }));
        match &mut self.results {
            Results::Content(c) => c.searching = true,
            other => {
                *other = Results::Content(Content {
                    searching: true,
                    ..Content::default()
                })
            }
        }
    }

    fn hit_row(file: &FileHits, h: usize) -> Row {
        let hit = &file.hits[h];
        let line = file.line_text(h);
        // Leading indentation isn't worth the room; long lines are cut.
        let lead = line.len() - line.trim_start().len();
        let cut = line[lead..]
            .char_indices()
            .nth(HIT_CHARS)
            .map_or(line.len(), |(i, _)| lead + i);
        let marks = hit
            .marks
            .iter()
            .filter(|m| m.start >= lead && m.end <= cut)
            .map(|m| m.start - lead..m.end - lead)
            .collect();
        Row {
            label: line[lead..cut].to_owned().into(),
            kind: RowKind::Hit { line: hit.line + 1 },
            marks,
            ..Row::default()
        }
    }

    fn notes_query(&mut self, query: &str, tag: Option<&str>) {
        let index = self.index.borrow();
        let hits = match tag {
            Some(tag) => self.search.notes_where(&index, query, LIMIT, |n| {
                n.tags.iter().any(|t| t.eq_ignore_ascii_case(tag))
            }),
            None => self.search.notes(&index, query, LIMIT),
        };
        let create = if tag.is_none() {
            new_note_path(query, &index)
        } else {
            None
        };
        self.results = Results::Notes { hits, create };
    }
}

/// The note a query would create: `ideas/gpu` → `ideas/gpu.md`, unless a note
/// with that path exists or the name isn't a valid note path.
fn new_note_path(query: &str, index: &NoteIndex) -> Option<NotePath> {
    let name = query.trim();
    if name.is_empty() {
        return None;
    }
    let file = if name.ends_with(".md") || name.ends_with(".markdown") {
        name.to_owned()
    } else {
        format!("{name}.md")
    };
    let path = NotePath::new(file).ok()?;
    index.get(&path).is_none().then_some(path)
}

impl PickerDelegate for Finder {
    fn prefix(&self, query: &str) -> (&'static str, usize) {
        match query.chars().next() {
            Some('#') => ("#", 1),
            Some('@') => ("@", 1),
            Some('>') => (">", 1),
            Some('/') => ("/", 1),
            _ => ("›", 0),
        }
    }

    fn scope(&self, mode: Mode) -> Option<SharedString> {
        Some(SharedString::new_static(match (mode, &self.results) {
            (Mode::Normal, _) => "j k move · i edit query",
            (_, Results::Notes { .. }) => "› notes   / text   # tag   @ heading   > command",
            (_, Results::Content(_)) => "searching note text",
            (_, Results::Tags(_)) => "tags",
            (_, Results::Headings(_)) => "headings in this note",
            (_, Results::Commands(_)) => "commands",
        }))
    }

    fn set_query(&mut self, query: &str) {
        match query.chars().next() {
            Some('#') => {
                let rest = &query[1..];
                // `#tag rest`: a complete tag narrows the notes.
                match rest.split_once(' ') {
                    Some((tag, more)) if !tag.is_empty() => {
                        let tag = tag.to_owned();
                        self.notes_query(more, Some(&tag));
                    }
                    _ => {
                        let index = self.index.borrow();
                        let hits = self.search.tags(&index, rest, LIMIT);
                        self.results = Results::Tags(hits);
                    }
                }
            }
            Some('@') => {
                let hits = self.search.headings(&self.current, &query[1..], LIMIT);
                self.results = Results::Headings(hits);
            }
            Some('>') => {
                // Commands match by label or ex name (`>noh`).
                let haystacks = self.command_haystacks.iter().map(String::as_str);
                let hits = self.search.strings(&query[1..], haystacks, LIMIT);
                self.results = Results::Commands(hits);
            }
            Some('/') => self.content_query(&query[1..]),
            _ => self.notes_query(query, None),
        }
    }

    fn len(&self) -> usize {
        match &self.results {
            Results::Notes { hits, create } => hits.len() + usize::from(create.is_some()),
            Results::Tags(hits) => hits.len(),
            Results::Headings(hits) => hits.len(),
            Results::Commands(hits) => hits.len(),
            Results::Content(c) => c.rows.len(),
        }
    }

    fn selectable(&self, ix: usize) -> bool {
        match &self.results {
            Results::Content(c) => c.rows.get(ix).is_some_and(|(_, hit)| hit.is_some()),
            _ => true,
        }
    }

    fn take_job(&mut self) -> Option<Job> {
        self.job.take()
    }

    fn job_done(&mut self, result: JobResult) {
        let JobResult::Content { generation, files } = result;
        if generation == self.generation && matches!(self.results, Results::Content(_)) {
            self.results = Results::Content(Content::new(files));
        }
    }

    fn send_to_list(&mut self) -> Option<PickerAction> {
        let Results::Content(c) = &self.results else {
            return None;
        };
        let entries: Vec<ListEntry> = c
            .files
            .iter()
            .flat_map(|file| {
                (0..file.hits.len()).map(move |h| ListEntry {
                    path: file.path.clone(),
                    line: file.hits[h].line,
                    text: file.line_text(h).trim().to_owned(),
                })
            })
            .collect();
        (!entries.is_empty()).then_some(PickerAction::SendToList(entries))
    }

    fn row(&self, ix: usize) -> Row {
        match &self.results {
            Results::Notes { hits, create } => match hits.get(ix) {
                Some(hit) => note_row(&self.index.borrow(), hit),
                None => Row {
                    label: format!(
                        "create \"{}\"",
                        create.as_ref().map(ToString::to_string).unwrap_or_default()
                    )
                    .into(),
                    detail: Some("new note".into()),
                    quiet: true,
                    ..Row::default()
                },
            },
            Results::Tags(hits) => {
                let hit = &hits[ix];
                Row {
                    label: format!("#{}", hit.tag).into(),
                    matches: hit.indices.iter().map(|&i| i as usize + 1).collect(),
                    detail: Some(plural(hit.count, "note").into()),
                    quiet: false,
                    ..Row::default()
                }
            }
            Results::Headings(hits) => {
                let hit = &hits[ix];
                let heading = &self.current.notes()[hit.note].headings[hit.heading];
                let indent = usize::from(heading.level.saturating_sub(1)) * 2;
                Row {
                    label: format!("{}{}", " ".repeat(indent), heading.text).into(),
                    matches: hit.indices.iter().map(|&i| i as usize + indent).collect(),
                    detail: Some(format!("line {}", heading.line + 1).into()),
                    quiet: false,
                    ..Row::default()
                }
            }
            Results::Commands(hits) => {
                let hit = &hits[ix];
                let (label, ex) = COMMANDS[hit.item];
                let len = label.chars().count();
                Row {
                    label: label.into(),
                    // Letters matched in the ex name aren't in the label.
                    matches: hit
                        .indices
                        .iter()
                        .map(|&i| i as usize)
                        .filter(|&i| i < len)
                        .collect(),
                    detail: Some(format!(":{ex}").into()),
                    quiet: false,
                    ..Row::default()
                }
            }
            Results::Content(c) => {
                let (f, hit) = c.rows[ix];
                let file = &c.files[f];
                match hit {
                    Some(h) => Self::hit_row(file, h),
                    None => {
                        let text = file.path.to_string();
                        let folder = text.rsplit_once('/').map(|(dir, _)| dir.to_owned());
                        Row {
                            label: file.path.file_name().to_owned().into(),
                            detail: folder.map(Into::into),
                            kind: RowKind::Group,
                            ..Row::default()
                        }
                    }
                }
            }
        }
    }

    fn preview(&self, ix: usize) -> Option<PreviewContent> {
        match &self.results {
            Results::Notes { hits, create } => match hits.get(ix) {
                Some(hit) => Some(note_preview(&self.index.borrow().notes()[hit.note])),
                None => create.as_ref().map(|path| PreviewContent {
                    title: path.name().to_owned().into(),
                    meta: Some(format!("{path} · new note").into()),
                    lines: vec!["⏎ creates it and opens it.".into()],
                    code: None,
                }),
            },
            Results::Tags(hits) => {
                let hit = hits.get(ix)?;
                let index = self.index.borrow();
                let lines = index
                    .notes()
                    .iter()
                    .filter(|n| n.tags.iter().any(|t| t.eq_ignore_ascii_case(&hit.tag)))
                    .take(PREVIEW_LINES)
                    .map(|n| SharedString::from(n.path_text.clone()))
                    .collect();
                Some(PreviewContent {
                    title: format!("#{}", hit.tag).into(),
                    meta: Some(format!("{} · ⏎ lists them", plural(hit.count, "note")).into()),
                    lines,
                    code: None,
                })
            }
            Results::Headings(hits) => {
                let hit = hits.get(ix)?;
                let heading = &self.current.notes()[hit.note].headings[hit.heading];
                let lines = self
                    .current_text
                    .lines()
                    .skip(heading.line + 1)
                    .take(PREVIEW_LINES)
                    .map(|l| SharedString::from(l.to_owned()))
                    .collect();
                Some(PreviewContent {
                    title: heading.text.clone().into(),
                    meta: Some(format!("line {}", heading.line + 1).into()),
                    lines,
                    code: None,
                })
            }
            Results::Commands(hits) => {
                let (label, ex) = COMMANDS[hits.get(ix)?.item];
                Some(PreviewContent {
                    title: label.into(),
                    meta: Some(format!(":{ex}").into()),
                    lines: vec!["Runs as if typed on the command line.".into()],
                    code: None,
                })
            }
            Results::Content(c) => {
                let (f, Some(h)) = *c.rows.get(ix)? else {
                    return None;
                };
                let file = &c.files[f];
                let hit = &file.hits[h];
                let title = self
                    .index
                    .borrow()
                    .get(&file.path)
                    .map_or_else(|| file.path.name().to_owned(), |n| n.title.clone());
                let first = hit.line.saturating_sub(CONTEXT_ABOVE);
                let lines = file
                    .text
                    .lines()
                    .enumerate()
                    .skip(first)
                    .take(PREVIEW_LINES)
                    .map(|(n, text)| {
                        let marks = if n == hit.line {
                            hit.marks.clone()
                        } else {
                            Vec::new()
                        };
                        (n + 1, SharedString::from(text.to_owned()), marks)
                    })
                    .collect();
                Some(PreviewContent {
                    title: title.into(),
                    meta: Some(format!("{} · line {}", file.path, hit.line + 1).into()),
                    lines: Vec::new(),
                    code: Some(CodePreview {
                        lines,
                        current: hit.line + 1,
                    }),
                })
            }
        }
    }

    fn confirm(&mut self, ix: usize) -> Confirm {
        match &self.results {
            Results::Notes { hits, create } => match hits.get(ix) {
                Some(hit) => {
                    let path = self.index.borrow().notes()[hit.note].path.clone();
                    Confirm::Act(PickerAction::OpenNote(path))
                }
                None => create.clone().map_or(Confirm::Close, |p| {
                    Confirm::Act(PickerAction::CreateNote(p))
                }),
            },
            Results::Tags(hits) => Confirm::SetQuery(format!("#{} ", hits[ix].tag)),
            Results::Headings(hits) => {
                let hit = &hits[ix];
                let line = self.current.notes()[hit.note].headings[hit.heading].line;
                Confirm::Act(PickerAction::GoToLine(line))
            }
            Results::Commands(hits) => {
                Confirm::Act(PickerAction::RunEx(COMMANDS[hits[ix].item].1.to_owned()))
            }
            Results::Content(c) => match c.rows.get(ix) {
                Some(&(f, Some(h))) => Confirm::Act(PickerAction::JumpTo {
                    path: c.files[f].path.clone(),
                    line: c.files[f].hits[h].line,
                }),
                _ => Confirm::Close,
            },
        }
    }

    fn hints(&self, _mode: Mode) -> Vec<Hint> {
        if let Results::Content(_) = &self.results {
            return vec![
                Hint::new("⏎", "jump to line"),
                Hint::new("⌃q", "send to list"),
            ];
        }
        vec![match &self.results {
            Results::Notes { .. } => Hint::new("⏎", "open"),
            Results::Tags(_) => Hint::new("⏎", "list notes"),
            Results::Headings(_) => Hint::new("⏎", "jump"),
            Results::Commands(_) => Hint::new("⏎", "run"),
            Results::Content(_) => Hint::new("⏎", "jump to line"),
        }]
    }

    fn count(&self) -> Option<SharedString> {
        let total = match &self.results {
            Results::Notes { hits, .. } => {
                return Some(format!("{} / {}", hits.len(), self.index.borrow().len()).into());
            }
            Results::Tags(hits) => hits.len(),
            Results::Headings(hits) => hits.len(),
            Results::Commands(hits) => hits.len(),
            Results::Content(c) => {
                return Some(match (c.searching, c.files.len()) {
                    (true, 0) => "searching…".into(),
                    (_, files) => format!(
                        "{} in {}",
                        plural(c.hit_count(), "hit"),
                        plural(files, "note")
                    )
                    .into(),
                });
            }
        };
        Some(total.to_string().into())
    }
}

fn note_row(index: &NoteIndex, hit: &NoteHit) -> Row {
    let note = &index.notes()[hit.note];
    let name_start = note.file_name_start();
    let matches = match hit.field {
        HitField::Path => hit
            .indices
            .iter()
            .filter_map(|&i| (i as usize).checked_sub(name_start))
            .collect(),
        _ => Vec::new(),
    };
    let folder: String = note.path_text.chars().take(name_start).collect();
    Row {
        label: note.path.file_name().to_owned().into(),
        matches,
        detail: Some(if folder.is_empty() {
            age(note.modified).into()
        } else {
            folder.into()
        }),
        quiet: false,
        ..Row::default()
    }
}

fn note_preview(note: &NoteEntry) -> PreviewContent {
    let mut meta = vec![note.path_text.clone()];
    if note.modified.is_some() {
        meta.push(format!("edited {} ago", age(note.modified)));
    }
    if !note.tags.is_empty() {
        let tags: Vec<String> = note.tags.iter().take(4).map(|t| format!("#{t}")).collect();
        meta.push(tags.join(" "));
    }
    let lines = note
        .headings
        .iter()
        .filter(|h| h.level > 1 || h.text != note.title)
        .take(PREVIEW_LINES)
        .map(|h| {
            let indent = "    ".repeat(usize::from(h.level.saturating_sub(2)));
            SharedString::from(format!("{indent}{}", h.text))
        })
        .collect();
    PreviewContent {
        title: note.title.clone().into(),
        meta: Some(meta.join(" · ").into()),
        lines,
        code: None,
    }
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// `4m`, `3h`, `2d`, `5w`, `1y`: how long ago `when` was.
fn age(when: Option<SystemTime>) -> String {
    let Some(ago) = when.and_then(|w| SystemTime::now().duration_since(w).ok()) else {
        return String::new();
    };
    let s = ago.as_secs();
    let (n, unit) = match s {
        0..60 => return "now".to_owned(),
        60..3_600 => (s / 60, "m"),
        3_600..86_400 => (s / 3_600, "h"),
        86_400..604_800 => (s / 86_400, "d"),
        604_800..31_536_000 => (s / 604_800, "w"),
        _ => (s / 31_536_000, "y"),
    };
    format!("{n}{unit}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn finder() -> Finder {
        let entry = |p: &str, text: &str| {
            NoteEntry::new(
                NotePath::new(p).expect("path"),
                text,
                Some(SystemTime::now() - Duration::from_secs(7_200)),
            )
        };
        let index = NoteIndex::from_entries(vec![
            entry(
                "projects/gpui-notes.md",
                "# GPUI notes\n## Render\n### Elements\n#rust\n",
            ),
            entry("inbox.md", "#rust #todo\n"),
        ]);
        Finder::new(
            Rc::new(RefCell::new(index)),
            CurrentNote {
                path: NotePath::new("ui-design.md").ok(),
                text: "# UI design\n\n## Principles\nKeyboard first\n## Open questions\n".into(),
            },
            None,
        )
    }

    fn labels(f: &Finder) -> Vec<String> {
        (0..f.len()).map(|i| f.row(i).label.to_string()).collect()
    }

    #[test]
    fn notes_bold_the_file_name_and_offer_to_create() {
        let mut f = finder();
        f.set_query("gpu");
        assert_eq!(labels(&f), ["gpui-notes.md", "create \"gpu.md\""]);
        let row = f.row(0);
        assert_eq!(row.matches, [0, 1, 2]);
        assert_eq!(row.detail.as_ref().map(|d| d.as_ref()), Some("projects/"));
        assert!(f.row(1).quiet);
        assert_eq!(
            f.confirm(1),
            Confirm::Act(PickerAction::CreateNote(
                NotePath::new("gpu.md").expect("path")
            ))
        );
        f.set_query("inbox");
        assert_eq!(
            labels(&f),
            ["inbox.md"],
            "no create row for an existing note"
        );
        f.set_query("");
        assert_eq!(f.len(), 2, "recent notes, no create row");
        f.set_query("../x");
        assert_eq!(f.len(), 0, "invalid paths can't be created");
    }

    #[test]
    fn tags_narrow_to_their_notes() {
        let mut f = finder();
        assert_eq!(f.prefix("#ru"), ("#", 1));
        f.set_query("#ru");
        assert_eq!(labels(&f), ["#rust"]);
        assert_eq!(f.row(0).matches, [1, 2]);
        assert_eq!(
            f.row(0).detail.as_ref().map(|d| d.as_ref()),
            Some("2 notes")
        );
        assert_eq!(f.confirm(0), Confirm::SetQuery("#rust ".into()));
        f.set_query("#rust ");
        assert_eq!(f.len(), 2);
        f.set_query("#rust inb");
        assert_eq!(labels(&f), ["inbox.md"]);
    }

    #[test]
    fn headings_of_this_note_and_commands() {
        let mut f = finder();
        f.set_query("@");
        assert_eq!(
            labels(&f),
            ["UI design", "  Principles", "  Open questions"]
        );
        f.set_query("@open");
        assert_eq!(f.confirm(0), Confirm::Act(PickerAction::GoToLine(4)));
        let preview = f.preview(0).expect("preview");
        assert_eq!(preview.meta.as_ref().map(|m| m.as_ref()), Some("line 5"));

        f.set_query(">noh");
        assert_eq!(
            f.confirm(0),
            Confirm::Act(PickerAction::RunEx("nohlsearch".into()))
        );
        assert_eq!(
            f.row(0).detail.as_ref().map(|d| d.as_ref()),
            Some(":nohlsearch")
        );
    }

    #[test]
    fn content_search_groups_hits_and_jumps() {
        let dir = std::env::temp_dir().join(format!("mnote-finder-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(
            dir.join("hotkeys.md"),
            "# Hotkeys\n\nneeds a CGEventTap\nthe event tap waits\n",
        )
        .expect("write");
        std::fs::write(dir.join("other.md"), "no match here\n").expect("write");
        let vault = Vault::open(&dir).expect("vault");
        let index = NoteIndex::build(&vault).expect("index");
        let mut f = Finder::new(
            Rc::new(RefCell::new(index)),
            CurrentNote {
                path: None,
                text: String::new(),
            },
            Some(vault),
        );
        assert_eq!(f.prefix("/event"), ("/", 1));
        f.set_query("/event tap");
        assert_eq!(f.count().as_ref().map(|c| c.as_ref()), Some("searching…"));
        let job = f.take_job().expect("a background search");
        f.job_done(job().expect("not cancelled"));
        assert_eq!(
            labels(&f),
            ["hotkeys.md", "needs a CGEventTap", "the event tap waits"]
        );
        assert!(!f.selectable(0), "the file row is a heading");
        assert_eq!(f.row(1).kind, RowKind::Hit { line: 3 });
        assert_eq!(f.row(1).marks, [10..15, 15..18]);
        assert_eq!(
            f.confirm(1),
            Confirm::Act(PickerAction::JumpTo {
                path: NotePath::new("hotkeys.md").expect("path"),
                line: 2
            })
        );
        let preview = f.preview(2).expect("preview").code.expect("code");
        assert_eq!(preview.current, 4);
        assert_eq!(preview.lines.first().map(|l| l.0), Some(1));
        match f.send_to_list() {
            Some(PickerAction::SendToList(entries)) => assert_eq!(entries.len(), 2),
            other => panic!("expected a list, got {other:?}"),
        }
        assert_eq!(
            f.count().as_ref().map(|c| c.as_ref()),
            Some("2 hits in 1 note")
        );

        // A newer query makes an older result stale.
        f.set_query("/event");
        let stale = f.take_job().expect("job");
        f.set_query("/tap");
        assert!(stale().is_none(), "cancelled by the newer search");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
