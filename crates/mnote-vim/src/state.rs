//! [`Vim`]: the mode state machine. It reads keys and says what to do; the
//! editor applies the result to its buffer and keeps the text.
//!
//! This task covers modes and motions. Operators, text objects, registers and
//! `.` arrive in AMI-158; `:` command parsing in AMI-160.

use ropey::Rope;

use crate::Mode;
use crate::keys::Key;
use crate::motion::{Find, Motion, Target, repeat_find};
use crate::text::{clamp_normal, column, first_non_blank, line_end, line_of};

/// What the editor should do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing visible: the key started or continued a sequence (`g`, `2`, `f`),
    /// or a motion couldn't move.
    None,
    /// The cursor moved. In visual modes, the selection runs from
    /// [`Vim::anchor`] to the cursor.
    Moved,
    /// The mode changed (the cursor may have moved too).
    ModeChanged(Mode),
    /// Insert mode: insert this text at the cursor, then move the cursor past
    /// it with [`Vim::set_cursor`].
    Insert(String),
    /// Insert mode: delete the character before the cursor.
    Backspace,
    /// The key isn't bound yet in this mode.
    Unhandled(Key),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Pending {
    #[default]
    None,
    /// `g` was pressed.
    G,
    /// `]` or `[` was pressed.
    Bracket { forward: bool },
    /// `f`/`t`/`F`/`T` waiting for the character.
    Find { forward: bool, till: bool },
}

/// Vim's state for one editor or text input.
#[derive(Debug, Clone, Default)]
pub struct Vim {
    mode: Mode,
    cursor: usize,
    anchor: usize,
    /// Column `j`/`k` aim for, kept across short lines.
    want_col: Option<usize>,
    count: Option<usize>,
    pending: Pending,
    last_find: Option<Find>,
}

impl Vim {
    pub fn new() -> Self {
        Self::default()
    }

    /// A Vim state for a text input (D-001): starts in insert mode.
    pub fn for_input() -> Self {
        Vim {
            mode: Mode::Insert,
            ..Self::default()
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Where a visual selection started.
    pub fn anchor(&self) -> usize {
        self.anchor
    }

    /// The selected range in visual mode, as `start..end` with `end`
    /// exclusive (the character under the cursor is selected, as in Vim).
    pub fn selection(&self) -> Option<std::ops::Range<usize>> {
        match self.mode {
            Mode::Visual => {
                let (a, b) = (self.anchor.min(self.cursor), self.anchor.max(self.cursor));
                Some(a..b + 1)
            }
            _ => None,
        }
    }

    /// Moves the cursor after the editor changed the text (for example after
    /// inserting what an [`Action::Insert`] asked for).
    pub fn set_cursor(&mut self, pos: usize) {
        self.cursor = pos;
        self.want_col = None;
    }

    /// Whether a key sequence is half typed (shown in the status line).
    pub fn is_pending(&self) -> bool {
        self.pending != Pending::None || self.count.is_some()
    }

    /// Handles one key against the current text.
    pub fn handle_key(&mut self, key: Key, text: &Rope) -> Action {
        match self.mode {
            Mode::Insert => self.insert_key(key, text),
            Mode::Command => match key {
                Key::Esc => self.enter(Mode::Normal, text),
                _ => Action::Unhandled(key),
            },
            Mode::Normal | Mode::Visual | Mode::VisualLine => self.normal_key(key, text),
        }
    }

    fn insert_key(&mut self, key: Key, text: &Rope) -> Action {
        match key {
            Key::Esc => {
                // Leaving insert mode steps back onto the last typed character.
                let line_start = text.line_to_char(line_of(text, self.cursor));
                if self.cursor > line_start {
                    self.cursor -= 1;
                }
                self.enter(Mode::Normal, text)
            }
            Key::Char(c) => Action::Insert(c.to_string()),
            Key::Enter => Action::Insert("\n".into()),
            Key::Tab => Action::Insert("\t".into()),
            Key::Backspace => Action::Backspace,
            Key::Ctrl(_) => Action::Unhandled(key),
        }
    }

    fn normal_key(&mut self, key: Key, text: &Rope) -> Action {
        let pending = std::mem::take(&mut self.pending);
        let motion = match (pending, key) {
            (Pending::Find { forward, till }, Key::Char(ch)) => {
                let find = Find { ch, forward, till };
                self.last_find = Some(find);
                Some(Motion::Find(find))
            }
            (Pending::G, Key::Char('g')) => Some(Motion::FirstLine),
            (Pending::Bracket { forward }, Key::Char(c))
                if c == if forward { ']' } else { '[' } =>
            {
                Some(if forward {
                    Motion::NextHeading
                } else {
                    Motion::PrevHeading
                })
            }
            (Pending::None, key) => return self.normal_start(key, text),
            // Any other key cancels the half-typed sequence, as in Vim.
            _ => {
                self.count = None;
                return Action::None;
            }
        };
        match motion {
            Some(motion) => self.run_motion(motion, text),
            None => Action::None,
        }
    }

    fn normal_start(&mut self, key: Key, text: &Rope) -> Action {
        let Key::Char(c) = key else {
            return match key {
                Key::Esc => {
                    self.count = None;
                    if self.mode == Mode::Normal {
                        Action::None
                    } else {
                        self.enter(Mode::Normal, text)
                    }
                }
                _ => Action::Unhandled(key),
            };
        };

        // Counts: `0` is a motion unless a count is already being typed.
        if let Some(d) = c.to_digit(10)
            && (d != 0 || self.count.is_some())
        {
            let count = self
                .count
                .unwrap_or(0)
                .saturating_mul(10)
                .saturating_add(d as usize);
            self.count = Some(count.min(999_999));
            return Action::None;
        }

        let motion = match c {
            'h' => Motion::Left,
            'l' => Motion::Right,
            'j' => Motion::Down,
            'k' => Motion::Up,
            'w' => Motion::WordForward { big: false },
            'W' => Motion::WordForward { big: true },
            'b' => Motion::WordBackward { big: false },
            'B' => Motion::WordBackward { big: true },
            'e' => Motion::WordEnd { big: false },
            'E' => Motion::WordEnd { big: true },
            '0' => Motion::LineStart,
            '^' => Motion::FirstNonBlank,
            '$' => Motion::LineEnd,
            'G' => Motion::LastLine,
            '%' => Motion::MatchingBracket,
            '}' => Motion::ParagraphForward,
            '{' => Motion::ParagraphBackward,
            'g' => return self.wait(Pending::G),
            ']' => return self.wait(Pending::Bracket { forward: true }),
            '[' => return self.wait(Pending::Bracket { forward: false }),
            'f' => {
                return self.wait(Pending::Find {
                    forward: true,
                    till: false,
                });
            }
            't' => {
                return self.wait(Pending::Find {
                    forward: true,
                    till: true,
                });
            }
            'F' => {
                return self.wait(Pending::Find {
                    forward: false,
                    till: false,
                });
            }
            'T' => {
                return self.wait(Pending::Find {
                    forward: false,
                    till: true,
                });
            }
            ';' | ',' => {
                let count = self.count.take();
                let Some(find) = self.last_find else {
                    return Action::None;
                };
                let find = if c == ',' { find.reversed() } else { find };
                return match repeat_find(text, self.cursor, find, count) {
                    Some(target) => self.move_to(target, text),
                    None => Action::None,
                };
            }
            _ => {
                self.count = None;
                return self.mode_key(c, text);
            }
        };
        self.run_motion(motion, text)
    }

    /// Keys that switch mode from normal or visual.
    fn mode_key(&mut self, c: char, text: &Rope) -> Action {
        let line = line_of(text, self.cursor);
        match (self.mode, c) {
            (Mode::Normal, 'i') => self.enter(Mode::Insert, text),
            (Mode::Normal, 'a') => {
                self.cursor = (self.cursor + 1).min(line_end(text, line));
                self.enter(Mode::Insert, text)
            }
            (Mode::Normal, 'I') => {
                self.cursor = first_non_blank(text, line).min(line_end(text, line));
                self.enter(Mode::Insert, text)
            }
            (Mode::Normal, 'A') => {
                self.cursor = line_end(text, line);
                self.enter(Mode::Insert, text)
            }
            (Mode::Normal, ':') | (Mode::Visual | Mode::VisualLine, ':') => {
                self.enter(Mode::Command, text)
            }
            (Mode::Normal | Mode::VisualLine, 'v') => self.enter(Mode::Visual, text),
            (Mode::Normal | Mode::Visual, 'V') => self.enter(Mode::VisualLine, text),
            (Mode::Visual, 'v') | (Mode::VisualLine, 'V') => self.enter(Mode::Normal, text),
            (Mode::Visual | Mode::VisualLine, 'o') => {
                std::mem::swap(&mut self.anchor, &mut self.cursor);
                self.want_col = None;
                Action::Moved
            }
            _ => Action::Unhandled(Key::Char(c)),
        }
    }

    fn wait(&mut self, pending: Pending) -> Action {
        self.pending = pending;
        Action::None
    }

    fn run_motion(&mut self, motion: Motion, text: &Rope) -> Action {
        let count = self.count.take();
        let past_end = self.mode != Mode::Normal;
        match motion.apply(text, self.cursor, count, self.want_col, past_end) {
            Some(target) => self.move_to(target, text),
            None => Action::None,
        }
    }

    fn move_to(&mut self, target: Target, text: &Rope) -> Action {
        if target.keep_column {
            if self.want_col.is_none() {
                self.want_col = Some(column(text, self.cursor));
            }
        } else {
            self.want_col = None;
        }
        self.cursor = if self.mode == Mode::Normal {
            clamp_normal(text, target.pos)
        } else {
            target.pos.min(text.len_chars())
        };
        Action::Moved
    }

    fn enter(&mut self, mode: Mode, text: &Rope) -> Action {
        if matches!(mode, Mode::Visual | Mode::VisualLine) && !self.mode.is_visual() {
            self.anchor = self.cursor;
        }
        if mode == Mode::Normal {
            self.cursor = clamp_normal(text, self.cursor);
        }
        self.mode = mode;
        self.pending = Pending::None;
        self.count = None;
        self.want_col = None;
        Action::ModeChanged(mode)
    }
}

impl Mode {
    pub fn is_visual(self) -> bool {
        matches!(self, Mode::Visual | Mode::VisualLine)
    }
}
