//! The mNote app's UI building blocks. The `mnote` binary in `main.rs` wires them
//! into windows.

pub mod capture;
pub mod commands;
pub mod editor;
pub mod folder_edit;
pub mod picker;
#[cfg(target_os = "macos")]
pub mod summon;
pub mod theme;
pub mod ui;
