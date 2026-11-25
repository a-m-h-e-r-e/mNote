//! Macros: `q{reg}` records the keys typed into a register (as key
//! notation, so `"ap` shows them and `"ayy` after an edit changes them),
//! `q` stops, `@{reg}` replays, `@@` repeats the last, `@:` the last command
//! line. Replays go through a queue rather than recursion, so a macro that
//! calls itself runs until a motion fails, as in Vim.

use std::collections::VecDeque;

use mnote_core::Buffer;

use crate::keys::{Key, key_notation, parse_keys};
use crate::state::{Effect, Vim};

/// Macro state kept on [`Vim`].
#[derive(Debug, Clone, Default)]
pub(crate) struct Macros {
    /// The register being recorded into, and the keys so far.
    recording: Option<(char, Vec<Key>)>,
    /// What `@@` repeats (`:` for `@:`).
    last: Option<char>,
    /// Keys still to replay.
    queue: VecDeque<Key>,
    replaying: bool,
    /// `handle_key` calls in progress: only keys typed at the top are
    /// recorded, not those a mapping, `.` or a replay feeds in.
    pub(crate) depth: u32,
    /// The last key's motion or search failed; a replay stops.
    pub(crate) failed: bool,
}

/// Registers a macro can be recorded into.
fn recordable(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '"'
}

impl Vim {
    /// The register being recorded into, for the status line.
    pub fn recording(&self) -> Option<char> {
        self.macros.recording.as_ref().map(|(reg, _)| *reg)
    }

    /// Notes a key typed by the user while recording.
    pub(crate) fn record_key(&mut self, key: Key) {
        if self.macros.depth == 1
            && let Some((_, keys)) = &mut self.macros.recording
        {
            keys.push(key);
        }
    }

    /// `q{reg}`: starts recording (an uppercase register appends).
    pub(crate) fn start_recording(&mut self, reg: char) -> Effect {
        self.cancel();
        if recordable(reg) {
            self.macros.recording = Some((reg, Vec::new()));
        }
        Effect::Handled
    }

    /// `q` while recording: the keys (without this `q`) go into the
    /// register.
    pub(crate) fn stop_recording(&mut self) -> Effect {
        self.cancel();
        if let Some((reg, mut keys)) = self.macros.recording.take() {
            keys.pop();
            self.registers.record(reg, &key_notation(&keys));
        }
        Effect::Handled
    }

    /// `@{reg}`, `@@` and `@:`, `count` times.
    pub(crate) fn run_macro(&mut self, reg: char, buf: &mut Buffer) -> Effect {
        let count = self.take_count().unwrap_or(1).max(1);
        self.cancel();
        let reg = if reg == '@' {
            match self.macros.last {
                Some(last) => last,
                None => return self.macro_error("E748: No previously used register"),
            }
        } else {
            reg
        };
        self.macros.last = Some(reg);
        if reg == ':' {
            let Some(line) = self.cmd.ex_history.last().cloned() else {
                return self.macro_error("E30: No previous command line");
            };
            for _ in 0..count {
                self.run_mapped_command(&line, buf);
            }
            return Effect::Handled;
        }
        let Some(stored) = self.registers.get(Some(reg)) else {
            return Effect::Handled;
        };
        // A linewise register's line break runs as Enter, as in Vim.
        let keys: Vec<Key> = parse_keys(&stored.text)
            .into_iter()
            .map(|key| {
                if key == Key::Char('\n') {
                    Key::Enter
                } else {
                    key
                }
            })
            .collect();
        let mut all = Vec::with_capacity(keys.len() * count);
        for _ in 0..count {
            all.extend_from_slice(&keys);
        }
        self.replay(all, buf);
        Effect::Handled
    }

    /// Runs `keys` as typed, mappings included. Inside a replay they go to
    /// the front of the queue instead, so recursion doesn't grow the stack.
    fn replay(&mut self, keys: Vec<Key>, buf: &mut Buffer) {
        for key in keys.into_iter().rev() {
            self.macros.queue.push_front(key);
        }
        if self.macros.replaying {
            return;
        }
        self.macros.replaying = true;
        while let Some(key) = self.macros.queue.pop_front() {
            self.macros.failed = false;
            self.handle_key(key, buf);
            let error = self.cmd.message.as_ref().is_some_and(|m| m.error);
            if self.macros.failed || error {
                self.macros.queue.clear();
            }
        }
        self.macros.replaying = false;
    }

    fn macro_error(&mut self, text: &str) -> Effect {
        self.cmd.message = Some(crate::ex::cmdline::Message {
            text: text.to_owned(),
            error: true,
            line: None,
        });
        Effect::Handled
    }
}

#[cfg(test)]
mod tests {
    use crate::harness::{check, run};

    #[test]
    fn record_and_replay() {
        check("|a\nb\nc\n", "qaA!<Esc>jq@a", "a!\nb!\n|c\n");
        check("|a\nb\nc\nd\n", "qaA!<Esc>jq2@a", "a!\nb!\nc!\n|d\n");
        check("|a\nb\nc\nd\n", "qaA!<Esc>jq@a@@", "a!\nb!\nc!\n|d\n");
    }

    #[test]
    fn the_register_holds_the_keys() {
        let (_, mut vim, _) = run("|a\n", "qaA<lt>x<Esc>q");
        let reg = vim.register('a').expect("recorded");
        assert_eq!(reg.text, "A<lt>x<Esc>");
        // `"ap` shows them; editing and `"ayy` changes the macro.
        check("|z\n", "qahq\"ap", "z|h\n");
        check("|abc\n", "o~<Esc>0\"ay$dd@a", "A|bc\n");
        // A linewise register's line break runs as Enter.
        check("|ab\nl\n", "jo0x<Esc>\"addk@a", "b\n|l\n");
    }

    #[test]
    fn uppercase_appends() {
        check("|abc\n", "qaxqqAxq@a", "|\n");
    }

    #[test]
    fn recording_shows_and_stops() {
        let (_, vim, _) = run("|a\n", "qb");
        assert_eq!(vim.recording(), Some('b'));
        let (_, vim, _) = run("|a\n", "qbq");
        assert_eq!(vim.recording(), None);
        // Typing `q` in INSERT is just a `q`.
        check("|\n", "qaiq<Esc>q", "|q\n");
    }

    #[test]
    fn a_failing_motion_stops_the_macro() {
        // `j` on the last line fails, so a big count stops at the end.
        check("|a\nb\nc\n", "qaA!<Esc>jq99@a", "a!\nb!\nc|!\n");
        // A search with no match.
        check("|a x\nb\n", "qa/x<CR>rYq@a", "a |Y\nb\n");
    }

    #[test]
    fn recursive_macro_runs_to_the_end() {
        let lines: String = (0..300).map(|_| "a\n").collect();
        let keys = "qaqqaA!<Esc>j@aq@a";
        let (got, _, _) = run(&format!("|{lines}"), keys);
        // (The cursor marker sits inside the last one.)
        assert_eq!(got.replace('|', "").matches("a!").count(), 300);
    }

    #[test]
    fn at_colon_repeats_the_command_line() {
        check("|a\nb\nc\n", ":s/$/!/<CR>j@:j@@", "a!\nb!\n|c!\n");
    }

    #[test]
    fn q_colon_still_opens_the_command_line_window() {
        let (_, vim, _) = run("|a\n", "q:");
        assert!(vim.cmd_window().is_some());
        assert_eq!(vim.recording(), None);
    }
}
