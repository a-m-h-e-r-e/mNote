//! mNote's Vim layer: modes, motions, operators and the ex command parser.
//!
//! Pure logic with no UI or file-system access, so every behaviour can be unit
//! tested. Motions and operators arrive in AMI-157 and AMI-158, the ex parser
//! in AMI-160.

use std::fmt;

/// The Vim mode an editor or text input is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
    Command,
}

/// How the caret is drawn for a mode (design decision D-001: the caret alone
/// shows the mode inside text inputs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaretShape {
    Block,
    Bar,
}

impl Mode {
    /// The label shown in the status line's mode pill.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
            Mode::Command => "COMMAND",
        }
    }

    pub fn caret(self) -> CaretShape {
        match self {
            Mode::Insert | Mode::Command => CaretShape::Bar,
            Mode::Normal | Mode::Visual | Mode::VisualLine => CaretShape::Block,
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_starts_in_normal_mode() {
        assert_eq!(Mode::default(), Mode::Normal);
    }

    #[test]
    fn caret_follows_mode() {
        assert_eq!(Mode::Normal.caret(), CaretShape::Block);
        assert_eq!(Mode::Visual.caret(), CaretShape::Block);
        assert_eq!(Mode::Insert.caret(), CaretShape::Bar);
        assert_eq!(Mode::Command.caret(), CaretShape::Bar);
    }

    #[test]
    fn labels_match_the_status_line() {
        assert_eq!(Mode::Insert.to_string(), "INSERT");
        assert_eq!(Mode::VisualLine.label(), "V-LINE");
    }
}
