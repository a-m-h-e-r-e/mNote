//! Reusable UI components, matching the design system's components of the same
//! names. Each one reads colours and sizes from the theme, never literals.

mod chrome;
mod command_line;
mod hints;
mod toast;

pub use chrome::{ModePill, StatusLine, TopBar, mode_colors};
pub use command_line::{CommandLine, CompletionPopup, MessagePanel, token_color};
pub use hints::{Hint, HintBar, KeyCap};
pub use toast::Toast;
