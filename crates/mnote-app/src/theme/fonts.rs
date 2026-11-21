//! The two font families mNote ships inside the binary: IBM Plex Sans for note
//! text and JetBrains Mono for everything the app says about itself.
//!
//! Both are under the SIL Open Font License 1.1; the licence texts sit next to
//! the files in `assets/fonts/`.

use std::borrow::Cow;

use gpui::App;

macro_rules! font {
    ($file:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fonts/",
            $file
        ))
    };
}

/// Every embedded font file, by file name.
pub const FILES: &[(&str, &[u8])] = &[
    ("IBMPlexSans-Regular.ttf", font!("IBMPlexSans-Regular.ttf")),
    ("IBMPlexSans-Medium.ttf", font!("IBMPlexSans-Medium.ttf")),
    (
        "IBMPlexSans-SemiBold.ttf",
        font!("IBMPlexSans-SemiBold.ttf"),
    ),
    (
        "JetBrainsMono-Regular.ttf",
        font!("JetBrainsMono-Regular.ttf"),
    ),
    (
        "JetBrainsMono-Medium.ttf",
        font!("JetBrainsMono-Medium.ttf"),
    ),
];

/// Registers the embedded fonts with gpui's text system. Called once at
/// startup, before the first window opens. If registration fails, mNote keeps
/// running on the system fonts the design system lists as fallbacks.
pub fn register(cx: &mut App) {
    let fonts = FILES
        .iter()
        .map(|(_, bytes)| Cow::Borrowed(*bytes))
        .collect();
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        eprintln!("mnote: could not load the bundled fonts, using system fonts: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_font_is_a_truetype_file() {
        for (name, bytes) in FILES {
            assert!(
                bytes.starts_with(&[0x00, 0x01, 0x00, 0x00]),
                "{name} is not a TrueType font"
            );
        }
    }

    #[test]
    fn fonts_stay_small() {
        let total: usize = FILES.iter().map(|(_, b)| b.len()).sum();
        assert!(total < 2 * 1024 * 1024, "bundled fonts are {total} bytes");
    }
}
