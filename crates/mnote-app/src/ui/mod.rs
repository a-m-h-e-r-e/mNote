//! Reusable UI components, matching the design system's components of the same
//! names. Each one reads colours and sizes from the theme, never literals.

mod chrome;

pub use chrome::{ModePill, StatusLine, TopBar, mode_colors};
