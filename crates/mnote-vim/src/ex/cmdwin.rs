//! The command-line window (`q:`, `q/`, `q?`, or `⌃f` on the command line):
//! the history as a small buffer, edited with all of Vim. `enter` runs the
//! line under the cursor; `esc` in normal mode, `⌃c` or `:q` closes it.

use mnote_core::Buffer;

use super::cmdline::{CommandKind, CommandLine};
use crate::state::Vim;
use crate::text::{line_count, line_len, line_of, line_start};
use crate::{Key, Mode};

/// The command-line window: one history entry per line, and the line being
/// typed last.
pub struct CmdWindow {
    pub kind: CommandKind,
    pub buffer: Buffer,
    pub vim: Vim,
}

impl std::fmt::Debug for CmdWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CmdWindow")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// Lines the window shows, as Vim's `cmdwinheight`.
pub const WINDOW_LINES: usize = 7;

impl CmdWindow {
    /// The lines to show, as `(first, last)` (inclusive), keeping the cursor
    /// in view.
    pub fn visible(&self) -> (usize, usize) {
        let text = self.buffer.text();
        let count = line_count(text).max(1);
        let cursor = line_of(text, self.vim.cursor());
        let first = (cursor + 1)
            .saturating_sub(WINDOW_LINES)
            .min(count.saturating_sub(WINDOW_LINES));
        (first, (first + WINDOW_LINES).min(count) - 1)
    }
}

impl Vim {
    /// The command-line window, while open.
    pub fn cmd_window(&self) -> Option<&CmdWindow> {
        self.cmd.cmdwin.as_deref()
    }

    /// Opens the window on the `kind` history, with `current` as the last
    /// line and the cursor on it.
    pub(crate) fn open_cmd_window(&mut self, kind: CommandKind, current: &str, buf: &Buffer) {
        self.enter(Mode::Normal, buf.text());
        let mut text = self.history(kind).join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(current);
        text.push('\n');
        let buffer = Buffer::new(&text);
        let mut vim = Vim::new();
        vim.options = self.options.clone();
        let last = line_count(buffer.text()).saturating_sub(1);
        vim.set_cursor(line_start(buffer.text(), last));
        self.cmd.cmdwin = Some(Box::new(CmdWindow { kind, buffer, vim }));
    }

    /// A key while the window is open. The note isn't touched until a line
    /// runs.
    pub(crate) fn cmd_window_key(&mut self, key: Key, buf: &mut Buffer) {
        let Some(mut win) = self.cmd.cmdwin.take() else {
            return;
        };
        let mode = win.vim.mode();
        let idle = !win.vim.is_pending();
        match key {
            Key::Ctrl('c') => return,
            Key::Esc if mode == Mode::Normal && idle => return,
            Key::Enter if mode != Mode::Command => {
                if mode != Mode::Normal {
                    win.vim.handle_key(Key::Esc, &mut win.buffer);
                }
                let text = win.buffer.text();
                let line = line_of(text, win.vim.cursor());
                let start = line_start(text, line);
                let picked = text.slice(start..start + line_len(text, line)).to_string();
                return self.run_line(CommandLine::new(win.kind, &picked), buf);
            }
            _ => {
                win.vim.handle_key(key, &mut win.buffer);
            }
        }
        // `:q` (or `:wq`, `:x`) inside the window closes it.
        if win
            .vim
            .take_app_command()
            .is_some_and(|c| matches!(c.name.as_str(), "quit" | "wq" | "xit"))
        {
            return;
        }
        self.cmd.cmdwin = Some(win);
    }
}

#[cfg(test)]
mod tests {
    use crate::{Mode, Vim, parse_keys};
    use mnote_core::Buffer;

    fn type_keys(vim: &mut Vim, buf: &mut Buffer, keys: &str) {
        for key in parse_keys(keys) {
            vim.handle_key(key, buf);
        }
    }

    #[test]
    fn q_colon_edits_history_and_runs_a_line() {
        let mut buf = Buffer::new("one\ntwo\n");
        let mut vim = Vim::new();
        type_keys(&mut vim, &mut buf, ":%s/one/1/<cr>u:2<cr>");
        type_keys(&mut vim, &mut buf, "q:");
        let win = vim.cmd_window().expect("open");
        assert_eq!(win.buffer.to_string(), "%s/one/1/\n2\n\n");
        assert_eq!(win.vim.cursor(), 12, "on the empty last line");

        // Edit the first entry and run it on the note.
        type_keys(&mut vim, &mut buf, "gg$x$a!/<esc>");
        assert_eq!(
            vim.cmd_window().map(|w| w.buffer.to_string()),
            Some("%s/one/1!/\n2\n\n".into())
        );
        type_keys(&mut vim, &mut buf, "gg<cr>");
        assert!(vim.cmd_window().is_none());
        assert_eq!(buf.to_string(), "1!\ntwo\n");
        assert_eq!(
            vim.history(super::CommandKind::Ex)
                .last()
                .map(String::as_str),
            Some("%s/one/1!/")
        );
    }

    #[test]
    fn closing_the_window() {
        crate::ex::tests::test_app_commands();
        let mut buf = Buffer::new("x\n");
        let mut vim = Vim::new();
        type_keys(&mut vim, &mut buf, "q:ihello<esc>");
        assert!(vim.cmd_window().is_some(), "esc from insert stays");
        type_keys(&mut vim, &mut buf, "<esc>");
        assert!(vim.cmd_window().is_none(), "esc in normal closes");
        type_keys(&mut vim, &mut buf, "q:");
        type_keys(&mut vim, &mut buf, ":q<cr>");
        assert!(vim.cmd_window().is_none(), ":q closes");
        type_keys(&mut vim, &mut buf, ":sor<c-f>");
        let win = vim.cmd_window().expect("⌃f opens it");
        assert_eq!(win.buffer.to_string(), "sor\n");
        type_keys(&mut vim, &mut buf, "<c-c>");
        assert_eq!((vim.mode(), buf.to_string()), (Mode::Normal, "x\n".into()));
    }
}
