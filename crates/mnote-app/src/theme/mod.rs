//! The app theme: the design-system tokens plus the dark or light choice, which
//! follows the system appearance.
//!
//! Colours, type, spacing, radii and sizes are generated into [`tokens`] from
//! `design/tokens.json` by `tools/gen_theme.py`. Never hard-code a colour or size
//! in a view; read it from [`Theme`] or the token modules.

pub mod fonts;
mod tokens;

pub use tokens::{Family, Palette, ShadowLayer, TextStyle, family, radius, size, space, text};

use std::cell::RefCell;

use gpui::{
    App, BoxShadow, FontWeight, Global, Hsla, Pixels, SharedString, Window, WindowAppearance,
    point, px, rgba,
};

/// Dark or light. Dark is mNote's primary theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Appearance {
    #[default]
    Dark,
    Light,
}

impl From<WindowAppearance> for Appearance {
    fn from(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Appearance::Dark,
            WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
        }
    }
}

/// The fonts in use: config.toml's `font`, `ui_font` and `font_size` over
/// the tokens' defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct Fonts {
    /// For mono styles: note text, code, inputs, the status line.
    pub mono: SharedString,
    /// For prose styles: headings and the interface.
    pub prose: SharedString,
    /// The note body's size; the other note styles scale with it.
    pub note_size: f32,
    /// The interface's text size; its styles, bars and rows scale with it.
    pub ui_size: f32,
}

impl Default for Fonts {
    fn default() -> Self {
        Fonts {
            mono: SharedString::new_static(family::MONO),
            prose: SharedString::new_static(family::PROSE),
            note_size: text::NOTE_BODY.size,
            ui_size: text::UI.size,
        }
    }
}

thread_local! {
    // Read by every text style as it's drawn, so a cell on the UI thread
    // rather than a lock.
    static FONTS: RefCell<Fonts> = RefCell::new(Fonts::default());
}

/// Puts `fonts` in use. Returns whether anything changed (lines then need
/// measuring again).
pub fn set_fonts(fonts: Fonts) -> bool {
    FONTS.with(|current| {
        let changed = *current.borrow() != fonts;
        *current.borrow_mut() = fonts;
        changed
    })
}

/// The mono family in use.
pub fn mono() -> SharedString {
    FONTS.with(|f| f.borrow().mono.clone())
}

/// The prose family in use.
pub fn prose() -> SharedString {
    FONTS.with(|f| f.borrow().prose.clone())
}

/// How much bigger than the design the note text is (`font_size` / 16).
pub fn note_scale() -> f32 {
    FONTS.with(|f| f.borrow().note_size / text::NOTE_BODY.size)
}

/// How much bigger than the design the interface is (`ui_font_size` / 15).
pub fn ui_scale() -> f32 {
    FONTS.with(|f| f.borrow().ui_size / text::UI.size)
}

/// The height of the top bar, status line, command line and hint bars.
pub fn bar_h() -> f32 {
    (size::BAR_H * ui_scale()).round()
}

/// The height of a list row in pickers and popups.
pub fn row_h() -> f32 {
    (size::ROW_H * ui_scale()).round()
}

/// The appearance `config.toml` fixes, if it does (`theme = "dark"`).
#[derive(Debug, Clone, Copy, Default)]
struct FixedAppearance(Option<Appearance>);

impl Global for FixedAppearance {}

/// The active theme, stored as a gpui global.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub appearance: Appearance,
    pub palette: &'static Palette,
    modal_shadow: &'static [ShadowLayer],
}

impl Global for Theme {}

impl Theme {
    pub fn new(appearance: Appearance) -> Self {
        match appearance {
            Appearance::Dark => Self {
                appearance,
                palette: &tokens::DARK,
                modal_shadow: tokens::SHADOW_MODAL_DARK,
            },
            Appearance::Light => Self {
                appearance,
                palette: &tokens::LIGHT,
                modal_shadow: tokens::SHADOW_MODAL_LIGHT,
            },
        }
    }

    /// Registers the bundled fonts and installs the theme for the system's
    /// current appearance.
    pub fn init(cx: &mut App) {
        fonts::register(cx);
        cx.set_global(Self::new(cx.window_appearance().into()));
    }

    /// Follows the config's `theme`: the system's appearance, or always dark
    /// or light.
    pub fn set_choice(choice: mnote_core::config::ThemeChoice, cx: &mut App) {
        use mnote_core::config::ThemeChoice;
        let fixed = match choice {
            ThemeChoice::System => None,
            ThemeChoice::Dark => Some(Appearance::Dark),
            ThemeChoice::Light => Some(Appearance::Light),
        };
        cx.set_global(FixedAppearance(fixed));
        let appearance = fixed.unwrap_or_else(|| cx.window_appearance().into());
        if cx.theme().appearance != appearance {
            cx.set_global(Self::new(appearance));
        }
    }

    /// Switches the theme when the window's appearance changed (unless the
    /// config fixes it). Returns true when it did, so the caller can re-render.
    pub fn sync_with(window: &Window, cx: &mut App) -> bool {
        if cx
            .try_global::<FixedAppearance>()
            .is_some_and(|f| f.0.is_some())
        {
            return false;
        }
        let appearance = Appearance::from(window.appearance());
        if cx.theme().appearance == appearance {
            return false;
        }
        cx.set_global(Self::new(appearance));
        true
    }

    /// A palette colour (`0xRRGGBBAA`) as a gpui colour.
    pub fn color(value: u32) -> Hsla {
        rgba(value).into()
    }

    /// The shadow every modal and toast carries. It's the only shadow in mNote.
    pub fn modal_shadow(&self) -> Vec<BoxShadow> {
        self.modal_shadow
            .iter()
            .map(|l| BoxShadow {
                color: Self::color(l.color),
                offset: point(px(l.x), px(l.y)),
                blur_radius: px(l.blur),
                spread_radius: px(l.spread),
            })
            .collect()
    }
}

impl TextStyle {
    pub fn font_family(&self) -> SharedString {
        match self.family {
            Family::Prose => prose(),
            Family::Mono => mono(),
        }
    }

    /// The note's own styles, which `font_size` scales; the interface's
    /// stay as designed.
    fn is_note(&self) -> bool {
        [
            text::NOTE_TITLE,
            text::NOTE_H2,
            text::NOTE_H3,
            text::NOTE_BODY,
            text::CODE,
        ]
        .contains(self)
    }

    fn scale(&self) -> f32 {
        if self.is_note() {
            note_scale()
        } else {
            ui_scale()
        }
    }

    pub fn font_weight(&self) -> FontWeight {
        FontWeight(f32::from(self.weight))
    }

    pub fn size(&self) -> Pixels {
        px(self.size * self.scale())
    }

    /// Rounded to whole pixels, so scaled lines stay crisp.
    pub fn line_height(&self) -> Pixels {
        px((self.line_height * self.scale()).round())
    }
}

/// Applies a design-system text style (family, size, line height, weight) to
/// any styled element: `div().text_style(text::STATUS)`.
pub trait StyledExt: gpui::Styled + Sized {
    fn text_style(self, style: TextStyle) -> Self {
        self.font_family(style.font_family())
            .text_size(style.size())
            .line_height(style.line_height())
            .font_weight(style.font_weight())
    }
}

impl<E: gpui::Styled> StyledExt for E {}

/// Access to the active theme from any gpui context.
pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG 2 relative luminance of an opaque `0xRRGGBBAA` colour.
    fn luminance(c: u32) -> f64 {
        let channel = |shift: u32| {
            let v = f64::from((c >> shift) & 0xff) / 255.0;
            if v <= 0.039_28 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(24) + 0.7152 * channel(16) + 0.0722 * channel(8)
    }

    fn contrast(a: u32, b: u32) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// Text pairs the design system promises at 4.5:1, and marks at 3:1.
    fn pairs(p: &Palette) -> Vec<(&'static str, u32, u32, f64)> {
        vec![
            ("ink on paper", p.ink, p.paper, 4.5),
            ("ink-2 on paper", p.ink_2, p.paper, 4.5),
            ("ink-3 on paper", p.ink_3, p.paper, 4.5),
            ("ink-3 on chrome", p.ink_3, p.chrome, 4.5),
            ("ink-3 on overlay", p.ink_3, p.overlay, 4.5),
            ("ink-3 on well", p.ink_3, p.well, 4.5),
            ("ink-2 on overlay", p.ink_2, p.overlay, 4.5),
            ("select-ink on select", p.select_ink, p.select, 4.5),
            ("ink on match", p.ink, p.match_, 4.5),
            ("on-cursor on cursor", p.on_cursor, p.cursor, 4.5),
            ("insert on insert-wash", p.insert, p.insert_wash, 4.5),
            ("visual on visual-wash", p.visual, p.visual_wash, 4.5),
            ("danger on danger-wash", p.danger, p.danger_wash, 4.5),
            ("syn-heading on paper", p.syn_heading, p.paper, 4.5),
            ("syn-strong on paper", p.syn_strong, p.paper, 4.5),
            ("syn-link on paper", p.syn_link, p.paper, 4.5),
            ("syn-url on paper", p.syn_url, p.paper, 4.5),
            ("syn-code on well", p.syn_code, p.well, 4.5),
            ("syn-keyword on well", p.syn_keyword, p.well, 4.5),
            ("cursor on paper", p.cursor, p.paper, 3.0),
            ("line-strong on overlay", p.line_strong, p.overlay, 3.0),
        ]
    }

    #[test]
    fn every_theme_meets_its_contrast_promises() {
        for (name, palette) in [("dark", &tokens::DARK), ("light", &tokens::LIGHT)] {
            for (pair, fg, bg, min) in pairs(palette) {
                let ratio = contrast(fg, bg);
                assert!(
                    ratio >= min,
                    "{name}: {pair} is {ratio:.2}:1, needs {min}:1"
                );
            }
        }
    }

    #[test]
    fn system_appearance_picks_the_theme() {
        assert_eq!(
            Appearance::from(WindowAppearance::VibrantDark),
            Appearance::Dark
        );
        assert_eq!(Appearance::from(WindowAppearance::Light), Appearance::Light);
        assert_eq!(Theme::new(Appearance::Dark).palette.paper, 0x282c34ff);
        assert_eq!(Theme::new(Appearance::Light).palette.paper, 0xfbfcfbff);
    }

    #[test]
    fn modal_shadow_has_its_layers() {
        assert_eq!(Theme::new(Appearance::Dark).modal_shadow().len(), 2);
        assert_eq!(Theme::new(Appearance::Light).modal_shadow().len(), 2);
    }
}

#[cfg(test)]
mod font_tests {
    use super::*;

    #[test]
    fn font_size_scales_note_styles_only() {
        assert!(
            !set_fonts(Fonts::default()),
            "the defaults are already in use"
        );
        assert!(set_fonts(Fonts {
            note_size: 20.,
            mono: "Menlo".into(),
            ..Fonts::default()
        }));
        assert_eq!(text::NOTE_BODY.size(), px(20.));
        assert_eq!(text::NOTE_BODY.line_height(), px(33.), "26 × 1.25, rounded");
        assert_eq!(text::NOTE_TITLE.size(), px(35.));
        assert_eq!(
            text::UI.size(),
            px(text::UI.size),
            "the interface keeps its size"
        );
        assert_eq!(text::NOTE_BODY.font_family().as_ref(), "Menlo");
        assert_eq!(text::UI.font_family().as_ref(), family::PROSE);

        // ui_font_size scales the interface, not the note.
        set_fonts(Fonts {
            ui_size: 18.,
            ..Fonts::default()
        });
        assert_eq!(text::UI.size(), px(18.));
        assert_eq!(bar_h(), 38., "32 × 1.2, rounded");
        assert_eq!(
            text::NOTE_BODY.size(),
            px(16.),
            "note text keeps its own size"
        );
        set_fonts(Fonts::default());
    }
}
