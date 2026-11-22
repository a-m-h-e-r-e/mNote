//! The mNote app's UI building blocks. The `mnote` binary in `main.rs` wires them
//! into windows.

pub mod capture;
pub mod commands;
pub mod editor;
pub mod folder_edit;
pub mod picker;
#[cfg(target_os = "macos")]
pub mod summon;

/// Today's date, local where the platform tells it, as (year, month, day).
pub fn today() -> (i32, u32, u32) {
    #[cfg(target_os = "macos")]
    {
        summon::today()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        mnote_core::capture::utc_date(secs)
    }
}
pub mod theme;
pub mod ui;
