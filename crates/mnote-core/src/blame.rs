//! Inline git blame: who last changed a line, and when, for the cursor line
//! (`git_blame = true` in config.toml). Runs `git blame` on the note as it
//! is in the editor (unsaved edits included), so line numbers match. Call
//! it off the UI thread; with no git, or outside a repository, there's
//! simply no blame.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// The last change to one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineBlame {
    pub author: String,
    /// Seconds since the Unix epoch.
    pub time: i64,
    /// The line has changes that aren't committed.
    pub uncommitted: bool,
}

impl LineBlame {
    /// As the cursor line shows it: `Amir, 3 days ago`.
    pub fn label(&self, now: i64) -> String {
        if self.uncommitted {
            return "Not committed yet".into();
        }
        format!("{}, {}", self.author, age(now - self.time))
    }
}

/// Blames line `line` (from 0) of `file`, whose text in the editor is
/// `contents`.
pub fn blame_line(file: &Path, contents: &str, line: usize) -> Option<LineBlame> {
    let dir = file.parent()?;
    let name = file.file_name()?;
    let n = (line + 1).to_string();
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["blame", "--porcelain", "--contents", "-", "-L"])
        .arg(format!("{n},{n}"))
        .arg("--")
        .arg(name)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Written from here while git reads; a note bigger than the pipe would
    // otherwise block both.
    let mut stdin = child.stdin.take()?;
    let text = contents.to_owned();
    let writer = std::thread::spawn(move || stdin.write_all(text.as_bytes()));
    let output = child.wait_with_output().ok()?;
    let _ = writer.join();
    if !output.status.success() {
        return None;
    }
    parse_porcelain(&String::from_utf8_lossy(&output.stdout))
}

/// Reads `git blame --porcelain` output for one line.
fn parse_porcelain(out: &str) -> Option<LineBlame> {
    let mut lines = out.lines();
    let header = lines.next()?;
    let uncommitted = header.starts_with("0000000000000000000000000000000000000000");
    let mut author = None;
    let mut time = None;
    for line in lines {
        if let Some(name) = line.strip_prefix("author ") {
            author = Some(name.to_owned());
        } else if let Some(secs) = line.strip_prefix("author-time ") {
            time = secs.trim().parse().ok();
        } else if line.starts_with('\t') {
            break;
        }
    }
    Some(LineBlame {
        author: author?,
        time: time?,
        uncommitted,
    })
}

/// A duration in seconds as people say it: `just now`, `5 minutes ago`,
/// `3 days ago`, `2 years ago`.
pub fn age(secs: i64) -> String {
    let secs = secs.max(0);
    let (n, unit) = match secs {
        0..60 => return "just now".into(),
        60..3_600 => (secs / 60, "minute"),
        3_600..86_400 => (secs / 3_600, "hour"),
        86_400..2_592_000 => (secs / 86_400, "day"),
        2_592_000..31_536_000 => (secs / 2_592_000, "month"),
        _ => (secs / 31_536_000, "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_porcelain_output() {
        let out = "3f2a9c1e5d7b2a4c6e8f0a1b3c5d7e9f1a2b3c4d 4 4 1\n\
                   author Amir\n\
                   author-mail <a@example.com>\n\
                   author-time 1795000000\n\
                   summary Notes\n\
                   filename inbox.md\n\
                   \tthe line\n";
        let blame = parse_porcelain(out).expect("parsed");
        assert_eq!(blame.author, "Amir");
        assert!(!blame.uncommitted);
        assert_eq!(blame.label(1795000000 + 3 * 86_400), "Amir, 3 days ago");
        let new = parse_porcelain(&out.replace(
            "3f2a9c1e5d7b2a4c6e8f0a1b3c5d7e9f1a2b3c4d",
            "0000000000000000000000000000000000000000",
        ))
        .expect("parsed");
        assert_eq!(new.label(0), "Not committed yet");
    }

    #[test]
    fn ages() {
        assert_eq!(age(5), "just now");
        assert_eq!(age(60), "1 minute ago");
        assert_eq!(age(7_200), "2 hours ago");
        assert_eq!(age(40 * 86_400), "1 month ago");
        assert_eq!(age(800 * 86_400), "2 years ago");
    }

    #[test]
    fn blames_a_real_repository() {
        let has_git = Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !has_git {
            return;
        }
        let dir = std::env::temp_dir().join(format!("mnote-blame-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .expect("git runs")
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("note.md"), "one\ntwo\n").expect("write");
        git(&["add", "note.md"]);
        git(&[
            "-c",
            "user.name=Tester",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "-m",
            "first",
        ]);
        let file = dir.join("note.md");
        let blame = blame_line(&file, "one\ntwo\n", 1).expect("blamed");
        assert_eq!(
            (blame.author.as_str(), blame.uncommitted),
            ("Tester", false)
        );
        let edited = blame_line(&file, "one\nTWO\n", 1).expect("blamed");
        assert!(edited.uncommitted, "an unsaved change isn't committed");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
