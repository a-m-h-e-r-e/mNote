//! Vim registers: where yanks and deletes go, and where `p` reads from.
//!
//! - `"` unnamed: the last yank or delete.
//! - `0`: the last yank. `1` to `9`: line or multi-line deletes, newest first.
//!   `-`: the last small (within one line) delete.
//! - `a` to `z`: named; `A` to `Z` append to them.
//! - `+` and `*`: the system clipboard. `_`: the black hole, which keeps
//!   nothing.
//!
//! With the clipboard shared (Vim's `clipboard=unnamedplus`, mNote's
//! default), the unnamed register is the system clipboard too: yanks and
//! deletes go to it, and `p` pastes what was last copied anywhere.

use std::collections::HashMap;

/// Text stored in a register. Linewise text pastes as whole lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Register {
    pub text: String,
    pub linewise: bool,
    /// A VISUAL BLOCK yank: its lines paste as a column.
    pub block: bool,
}

/// The system clipboard, supplied by the app. Tests use [`MemoryClipboard`].
pub trait Clipboard {
    fn read(&mut self) -> Option<String>;
    fn write(&mut self, text: &str);
}

/// An in-process clipboard, the default until the app plugs in the real one.
#[derive(Debug, Default)]
pub struct MemoryClipboard(Option<String>);

impl Clipboard for MemoryClipboard {
    fn read(&mut self) -> Option<String> {
        self.0.clone()
    }

    fn write(&mut self, text: &str) {
        self.0 = Some(text.to_owned());
    }
}

pub(crate) struct Registers {
    unnamed: Register,
    stored: HashMap<char, Register>,
    clipboard: Box<dyn Clipboard>,
    /// The unnamed register is the system clipboard too.
    shared: bool,
}

impl std::fmt::Debug for Registers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registers")
            .field("unnamed", &self.unnamed)
            .field("stored", &self.stored)
            .finish_non_exhaustive()
    }
}

impl Default for Registers {
    fn default() -> Self {
        Registers {
            unnamed: Register::default(),
            stored: HashMap::new(),
            clipboard: Box::new(MemoryClipboard::default()),
            shared: false,
        }
    }
}

impl Registers {
    pub fn set_clipboard(&mut self, clipboard: Box<dyn Clipboard>) {
        self.clipboard = clipboard;
    }

    /// Shares the unnamed register with the system clipboard, or not.
    pub fn share_clipboard(&mut self, shared: bool) {
        self.shared = shared;
    }

    /// A yank or delete into the unnamed register reaches the clipboard
    /// when it's shared.
    fn share(&mut self, reg: &Register) {
        if self.shared {
            self.clipboard.write(&reg.text);
        }
    }

    /// Stores a yank: into `name` if given, else into `0`.
    pub fn yank(&mut self, name: Option<char>, reg: Register) {
        match name {
            Some(_) => self.store_named(name, reg),
            None => {
                self.stored.insert('0', reg.clone());
                self.share(&reg);
                self.unnamed = reg;
            }
        }
    }

    /// Stores a delete or change: into `name` if given, else into `1` (shifting
    /// `1`..`8` down) for line deletes, or `-` for small ones.
    pub fn delete(&mut self, name: Option<char>, reg: Register) {
        match name {
            Some(_) => self.store_named(name, reg),
            None => {
                if reg.linewise || reg.text.contains('\n') {
                    for n in (1..9u8).rev() {
                        if let Some(r) = self.stored.remove(&char::from(b'0' + n)) {
                            self.stored.insert(char::from(b'1' + n), r);
                        }
                    }
                    self.stored.insert('1', reg.clone());
                } else {
                    self.stored.insert('-', reg.clone());
                }
                self.share(&reg);
                self.unnamed = reg;
            }
        }
    }

    /// Stores a recorded macro in `name` (appending for an uppercase name),
    /// leaving the unnamed register alone, as Vim does.
    pub(crate) fn record(&mut self, name: char, text: &str) {
        let reg = Register {
            text: text.to_owned(),
            linewise: false,
            block: false,
        };
        match name {
            '"' => self.unnamed = reg,
            c if c.is_ascii_uppercase() => {
                let entry = self.stored.entry(c.to_ascii_lowercase()).or_default();
                entry.text.push_str(text);
            }
            c => {
                self.stored.insert(c, reg);
            }
        }
    }

    fn store_named(&mut self, name: Option<char>, reg: Register) {
        match name {
            Some('_') => {}
            Some('+' | '*') => {
                self.clipboard.write(&reg.text);
                self.unnamed = reg;
            }
            Some(c) if c.is_ascii_uppercase() => {
                let lower = c.to_ascii_lowercase();
                let entry = self.stored.entry(lower).or_default();
                if entry.linewise && !reg.linewise && !entry.text.ends_with('\n') {
                    entry.text.push('\n');
                }
                entry.text.push_str(&reg.text);
                entry.linewise |= reg.linewise;
                self.unnamed = entry.clone();
            }
            Some(c) => {
                self.stored.insert(c, reg.clone());
                self.unnamed = reg;
            }
            None => self.unnamed = reg,
        }
    }

    /// Reads a register for pasting. `None` or `"` is the unnamed register.
    pub fn get(&mut self, name: Option<char>) -> Option<Register> {
        match name {
            // Shared: text copied in another app since mNote's last yank wins
            // (mNote's own text keeps its linewise-ness).
            None | Some('"') if self.shared => match self.clipboard.read() {
                Some(text) if !text.is_empty() && text != self.unnamed.text => Some(Register {
                    linewise: text.ends_with('\n'),
                    text,
                    block: false,
                }),
                _ => (!self.unnamed.text.is_empty()).then(|| self.unnamed.clone()),
            },
            None | Some('"') => (!self.unnamed.text.is_empty()).then(|| self.unnamed.clone()),
            Some('+' | '*') => self.clipboard.read().map(|text| Register {
                linewise: text.ends_with('\n'),
                text,
                block: false,
            }),
            Some('_') => None,
            Some(c) => self.stored.get(&c.to_ascii_lowercase()).cloned(),
        }
    }
}

/// Whether `c` names a register (for `"x`).
pub(crate) fn is_register_name(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '"' | '-' | '+' | '*' | '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(text: &str, linewise: bool) -> Register {
        Register {
            text: text.into(),
            linewise,
            block: false,
        }
    }

    #[test]
    fn yanks_go_to_zero_and_line_deletes_shift_through_the_numbers() {
        let mut regs = Registers::default();
        regs.yank(None, r("word", false));
        regs.delete(None, r("line one\n", true));
        regs.delete(None, r("line two\n", true));
        regs.delete(None, r("x", false));
        assert_eq!(regs.get(Some('0')), Some(r("word", false)));
        assert_eq!(regs.get(Some('1')), Some(r("line two\n", true)));
        assert_eq!(regs.get(Some('2')), Some(r("line one\n", true)));
        assert_eq!(regs.get(Some('-')), Some(r("x", false)));
        assert_eq!(regs.get(None), Some(r("x", false)));
    }

    #[test]
    fn named_registers_append_clipboard_and_black_hole() {
        let mut regs = Registers::default();
        regs.yank(Some('a'), r("one", false));
        regs.yank(Some('A'), r(" two", false));
        assert_eq!(regs.get(Some('a')), Some(r("one two", false)));
        regs.yank(Some('+'), r("shared\n", true));
        assert_eq!(regs.get(Some('*')), Some(r("shared\n", true)));
        regs.delete(Some('_'), r("gone", false));
        assert_eq!(
            regs.get(None),
            Some(r("shared\n", true)),
            "black hole keeps nothing"
        );
    }
}
