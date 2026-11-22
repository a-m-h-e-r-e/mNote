//! Quick capture: where a captured line lands in a note. By default that's
//! the daily note, `journal/{date}.md`, at the end of its `## Inbox`
//! section, which is made when the note or the section doesn't exist yet.

use crate::NotePath;

/// The section captures go under.
pub const INBOX: &str = "Inbox";

/// The daily note for a date: `journal/2026-11-22.md`.
pub fn daily_note(year: i32, month: u32, day: u32) -> NotePath {
    NotePath::new(format!("journal/{year:04}-{month:02}-{day:02}.md"))
        .expect("a dated path under journal/ is a valid note path")
}

/// The UTC date `secs` after the Unix epoch, as (year, month, day). The app
/// uses the local date; this is its fallback.
pub fn utc_date(secs: u64) -> (i32, u32, u32) {
    // Howard Hinnant's days-to-civil algorithm.
    let days = (secs / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (year_of_era + era * 400 + i64::from(month <= 2)) as i32;
    (year, month, day)
}

/// Text to insert into a note for one capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Insertion {
    /// Byte offset in the note.
    pub at: usize,
    pub text: String,
    /// The line (from 0) the captured entry ends up on.
    pub line: usize,
}

impl Insertion {
    /// The note with this insertion made.
    pub fn apply(&self, note: &str) -> String {
        let mut out = String::with_capacity(note.len() + self.text.len());
        out.push_str(&note[..self.at]);
        out.push_str(&self.text);
        out.push_str(&note[self.at..]);
        out
    }
}

/// Where `entry` goes in `note`: a list item at the end of `## Inbox`. An
/// empty note gets a `# {title}` heading first; a note without the section
/// gets it at the end.
pub fn insertion(note: &str, title: &str, entry: &str) -> Insertion {
    let entry = entry.trim();
    let item = if ["- ", "* ", "+ "].iter().any(|m| entry.starts_with(m)) {
        entry.to_owned()
    } else {
        format!("- {entry}")
    };

    let lines = lines_with_offsets(note);
    let (at, text) = match find_section(note, &lines) {
        Some((heading, end)) => {
            let last = (heading + 1..end)
                .rev()
                .find(|&i| !line(note, &lines, i).trim().is_empty());
            match last {
                Some(i) => after_line(note, &lines, i, &item, ""),
                None => after_line(note, &lines, heading, &item, "\n"),
            }
        }
        None if note.is_empty() => (0, format!("# {title}\n\n## {INBOX}\n\n{item}\n")),
        None => {
            let mut text = String::new();
            if !note.ends_with('\n') {
                text.push('\n');
            }
            if lines
                .last()
                .is_some_and(|&(s, e)| !note[s..e].trim().is_empty())
            {
                text.push('\n');
            }
            text.push_str(&format!("## {INBOX}\n\n{item}\n"));
            (note.len(), text)
        }
    };
    // Every insertion ends with the item and its '\n'.
    let item_at = text.len() - item.len() - 1;
    let line = note[..at].matches('\n').count() + text[..item_at].matches('\n').count();
    Insertion { at, text, line }
}

/// Inserts `item` on its own line after line `i`, with `gap` between.
fn after_line(
    note: &str,
    lines: &[(usize, usize)],
    i: usize,
    item: &str,
    gap: &str,
) -> (usize, String) {
    let end = lines[i].1;
    if end < note.len() {
        // Just past the line's '\n'.
        (end + 1, format!("{gap}{item}\n"))
    } else {
        (end, format!("\n{gap}{item}\n"))
    }
}

/// Each line's byte range, without its '\n'.
fn lines_with_offsets(note: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in note.char_indices() {
        if c == '\n' {
            out.push((start, i));
            start = i + 1;
        }
    }
    if start < note.len() {
        out.push((start, note.len()));
    }
    out
}

fn line<'a>(note: &'a str, lines: &[(usize, usize)], i: usize) -> &'a str {
    &note[lines[i].0..lines[i].1]
}

/// The `## Inbox` heading's line and the line its section ends before (the
/// next `#` or `##` heading, or the end), skipping code fences.
fn find_section(note: &str, lines: &[(usize, usize)]) -> Option<(usize, usize)> {
    let mut fenced = false;
    let mut heading = None;
    for i in 0..lines.len() {
        let text = line(note, lines, i);
        let trimmed = text.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let level = trimmed.chars().take_while(|&c| c == '#').count();
        let is_heading = level > 0 && trimmed[level..].starts_with(' ');
        if !is_heading {
            continue;
        }
        match heading {
            None if level == 2 && trimmed[level..].trim().eq_ignore_ascii_case(INBOX) => {
                heading = Some(i)
            }
            Some(h) if level <= 2 => return Some((h, i)),
            _ => {}
        }
    }
    heading.map(|h| (h, lines.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(note: &str, entry: &str) -> (String, usize) {
        let ins = insertion(note, "2026-11-22", entry);
        (ins.apply(note), ins.line)
    }

    #[test]
    fn a_new_daily_note_gets_a_title_and_an_inbox() {
        assert_eq!(
            capture("", "look into nucleo"),
            ("# 2026-11-22\n\n## Inbox\n\n- look into nucleo\n".into(), 4)
        );
        assert_eq!(
            daily_note(2026, 1, 5).as_path().to_str(),
            Some("journal/2026-01-05.md")
        );
    }

    #[test]
    fn utc_dates() {
        assert_eq!(utc_date(0), (1970, 1, 1));
        assert_eq!(utc_date(951_782_400), (2000, 2, 29));
        assert_eq!(utc_date(1_795_305_600), (2026, 11, 22));
    }

    #[test]
    fn entries_go_at_the_end_of_the_inbox_section() {
        let note = "# Day\n\n## Inbox\n\n- one\n\n## Done\n- old\n";
        assert_eq!(
            capture(note, "two"),
            (
                "# Day\n\n## Inbox\n\n- one\n- two\n\n## Done\n- old\n".into(),
                5
            )
        );
    }

    #[test]
    fn an_empty_inbox_gets_a_blank_line_after_its_heading() {
        assert_eq!(
            capture("## Inbox\n## Next\n", "a"),
            ("## Inbox\n\n- a\n## Next\n".into(), 2)
        );
        assert_eq!(capture("## inbox", "a"), ("## inbox\n\n- a\n".into(), 2));
    }

    #[test]
    fn a_note_without_an_inbox_gets_one_at_the_end() {
        assert_eq!(
            capture("# Day\nwords", "a"),
            ("# Day\nwords\n\n## Inbox\n\n- a\n".into(), 5)
        );
        assert_eq!(
            capture("# Day\n\n", "a"),
            ("# Day\n\n## Inbox\n\n- a\n".into(), 4)
        );
    }

    #[test]
    fn subsections_stay_inside_and_fences_are_skipped() {
        let note = "## Inbox\n- a\n### Later\n- b\n```\n## Inbox\n```\n# Next\n";
        let (text, line) = capture(note, "c");
        assert_eq!(
            text,
            "## Inbox\n- a\n### Later\n- b\n```\n## Inbox\n```\n- c\n# Next\n"
        );
        assert_eq!(line, 7);
    }

    #[test]
    fn list_markers_and_spaces_are_kept_as_typed() {
        assert_eq!(
            capture("## Inbox\n", "  * star  ").0,
            "## Inbox\n\n* star\n"
        );
        assert_eq!(
            capture("## Inbox\n", "- [ ] task").0,
            "## Inbox\n\n- [ ] task\n"
        );
    }
}
