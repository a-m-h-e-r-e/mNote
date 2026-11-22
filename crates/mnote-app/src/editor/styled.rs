//! How a line looks in the styled view: its font, its inline formatting with
//! markers hidden off the cursor line, and the block it sits in (heading,
//! list item, task, quote, callout, code block, frontmatter, rule).
//!
//! The parsing is `mnote_core::styled`; this module turns it into fonts,
//! colours and the few shapes drawn beside the text (bullets, checkboxes,
//! callout labels).

use gpui::{Font, FontWeight, Hsla, Pixels, SharedString, font, px};
use mnote_core::code::CodeStyle;
use mnote_core::styled::{Inline, LineKind, StyledLine, style_inline};

use super::line::Styling;
use crate::theme::{Palette, TextStyle, Theme, text};

/// Width of one character of note text (IBM Plex Mono at 16px).
pub const CHAR_W: f32 = 9.6;

/// What's drawn before a line's text.
#[derive(Debug, Clone, PartialEq)]
pub enum Prefix {
    None,
    /// `•` for a bullet list item.
    Bullet,
    Checkbox {
        checked: bool,
    },
    /// The label of a callout's first line: its kind, in its colour.
    Callout {
        label: String,
        color: Hsla,
    },
}

/// One line, ready to draw.
pub struct Look {
    /// The text shown, and how source characters map into it.
    pub shown: StyledLine,
    pub styling: Vec<Styling>,
    pub style: TextStyle,
    pub color: Hsla,
    pub prefix: Prefix,
    /// Left inset of the text, in pixels.
    pub indent: f32,
    /// A fill behind the whole row (code blocks, callouts).
    pub background: Option<Hsla>,
    /// Round the fill's top or bottom corners (first or last row).
    pub round_top: bool,
    pub round_bottom: bool,
    /// A quiet label at the right (a code block's language).
    pub label: Option<SharedString>,
    /// Drawn as a horizontal rule.
    pub rule: bool,
    /// Row height for rows that aren't a text line (fence ends).
    pub height: Option<Pixels>,
}

impl Look {
    fn new(shown: StyledLine, style: TextStyle, color: Hsla) -> Self {
        Look {
            shown,
            styling: Vec::new(),
            style,
            color,
            prefix: Prefix::None,
            indent: 0.,
            background: None,
            round_top: false,
            round_bottom: false,
            label: None,
            rule: false,
            height: None,
        }
    }

    pub fn font(&self) -> Font {
        Font {
            weight: self.style.font_weight(),
            ..font(self.style.font_family())
        }
    }
}

/// The text style of a heading level.
fn heading_style(level: u8) -> TextStyle {
    match level {
        1 => text::NOTE_TITLE,
        2 => text::NOTE_H2,
        _ => text::NOTE_H3,
    }
}

/// Inline formatting as styling runs.
fn inline_styling(shown: &StyledLine, p: &Palette) -> Vec<Styling> {
    shown
        .spans
        .iter()
        .map(|span| {
            let mut st = Styling {
                bytes: span.range.clone(),
                ..Styling::default()
            };
            match span.style {
                Inline::Bold => {
                    st.weight = Some(FontWeight::SEMIBOLD);
                    st.color = Some(Theme::color(p.ink));
                }
                // No italic face ships with the app; italic reads as quieter.
                Inline::Italic => st.color = Some(Theme::color(p.ink_2)),
                Inline::Code => {
                    st.color = Some(Theme::color(p.syn_code));
                    st.background = Some(Theme::color(p.well));
                }
                Inline::Strike => {
                    st.strike = true;
                    st.color = Some(Theme::color(p.ink_3));
                }
                Inline::Link => {
                    st.color = Some(Theme::color(p.syn_link));
                    st.underline = true;
                }
                Inline::Url => st.color = Some(Theme::color(p.syn_url)),
                Inline::Tag => {
                    st.color = Some(Theme::color(p.ink_2));
                    st.background = Some(Theme::color(p.well));
                }
                Inline::Marker => st.color = Some(Theme::color(p.ink_3)),
            }
            st
        })
        .collect()
}

/// The colour of a code style.
pub fn code_color(style: CodeStyle, p: &Palette) -> u32 {
    match style {
        CodeStyle::Keyword => p.syn_keyword,
        CodeStyle::String => p.syn_code,
        CodeStyle::Comment => p.ink_3,
        CodeStyle::Function => p.syn_link,
        CodeStyle::Type | CodeStyle::Number => p.syn_strong,
        CodeStyle::Constant => p.syn_url,
        CodeStyle::Property => p.ink,
        CodeStyle::Punctuation => p.ink_2,
    }
}

/// The colour of a callout kind.
fn callout_color(kind: &str, p: &Palette) -> u32 {
    match kind {
        "warning" | "caution" | "attention" | "important" => p.insert,
        "danger" | "error" | "bug" | "failure" => p.danger,
        _ => p.select_ink,
    }
}

/// How `content` (line `kind`) looks. `raw` shows it as typed, markers and
/// all (the cursor line, or a line under a live `:s` preview).
pub fn look(kind: &LineKind, content: &str, raw: bool, p: &Palette) -> Look {
    let body = text::NOTE_BODY;
    let ink = Theme::color(p.ink);
    let plain = |style: TextStyle, color: Hsla, skip: usize| {
        let shown = style_inline(content, raw, if raw { 0 } else { skip });
        let mut look = Look::new(shown, style, color);
        look.styling = inline_styling(&look.shown, p);
        look
    };
    match kind {
        LineKind::Heading { level, marker } => plain(heading_style(*level), ink, *marker),
        LineKind::Bullet {
            indent,
            marker,
            ordered,
        } => {
            if raw || *ordered {
                let mut look = plain(body, ink, 0);
                look.indent = 0.;
                return look;
            }
            let mut look = plain(body, ink, *marker);
            look.prefix = Prefix::Bullet;
            look.indent = *indent as f32 * CHAR_W;
            look
        }
        LineKind::Task {
            indent,
            marker,
            checked,
        } => {
            if raw {
                return plain(body, ink, 0);
            }
            let color = if *checked { Theme::color(p.ink_3) } else { ink };
            let mut look = plain(body, color, *marker);
            if *checked {
                let len = look.shown.text.len();
                look.styling.push(Styling {
                    bytes: 0..len,
                    strike: true,
                    ..Styling::default()
                });
            }
            look.prefix = Prefix::Checkbox { checked: *checked };
            look.indent = *indent as f32 * CHAR_W;
            look
        }
        LineKind::Quote { marker } => {
            let mut look = plain(body, Theme::color(p.ink_2), *marker);
            if !raw {
                look.indent = 2. * CHAR_W;
            }
            look
        }
        LineKind::CalloutHead { kind, marker } => {
            // `> [!note] Title`: the label replaces the marker and `[!note]`.
            let skip = content
                .find(']')
                .map_or(*marker, |i| content[..=i].chars().count())
                + usize::from(content.contains("] "));
            let mut look = plain(body, ink, skip);
            look.background = Some(Theme::color(p.well));
            look.round_top = true;
            if !raw {
                look.prefix = Prefix::Callout {
                    label: kind.to_uppercase(),
                    color: Theme::color(callout_color(kind, p)),
                };
            }
            look
        }
        LineKind::Callout { marker } => {
            let mut look = plain(body, ink, *marker);
            look.background = Some(Theme::color(p.well));
            look
        }
        LineKind::FenceOpen { lang, .. } => {
            let mut look = Look::new(
                StyledLine::plain(content),
                text::CODE,
                Theme::color(p.ink_3),
            );
            look.background = Some(Theme::color(p.well));
            look.round_top = true;
            if !raw {
                look.shown = StyledLine::plain("");
                look.label = (!lang.is_empty()).then(|| lang.clone().into());
                look.height = Some(px(28.));
            }
            look
        }
        LineKind::FenceClose => {
            let mut look = Look::new(
                StyledLine::plain(content),
                text::CODE,
                Theme::color(p.ink_3),
            );
            look.background = Some(Theme::color(p.well));
            look.round_bottom = true;
            if !raw {
                look.shown = StyledLine::plain("");
                look.height = Some(px(12.));
            }
            look
        }
        LineKind::Code { .. } => {
            let mut look = Look::new(StyledLine::plain(content), text::CODE, ink);
            look.background = Some(Theme::color(p.well));
            look
        }
        LineKind::Frontmatter { .. } => Look::new(
            StyledLine::plain(content),
            text::CODE,
            Theme::color(p.ink_2),
        ),
        LineKind::Rule => {
            let mut look = plain(body, Theme::color(p.ink_3), 0);
            if !raw {
                look.rule = true;
                look.shown = StyledLine::plain("");
            }
            look
        }
        LineKind::Table | LineKind::Text => plain(body, ink, 0),
    }
}

/// Code colours for one line of a block, from the block's highlights.
pub fn code_styling(spans: &[(std::ops::Range<usize>, CodeStyle)], p: &Palette) -> Vec<Styling> {
    spans
        .iter()
        .map(|(range, style)| Styling {
            bytes: range.clone(),
            color: Some(Theme::color(code_color(*style, p))),
            ..Styling::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Appearance, Theme};

    #[test]
    fn markers_hide_off_the_cursor_line() {
        let p = Theme::new(Appearance::Dark).palette;
        let heading = LineKind::Heading {
            level: 2,
            marker: 3,
        };
        assert_eq!(
            look(&heading, "## Principles", false, p).shown.text,
            "Principles"
        );
        assert_eq!(
            look(&heading, "## Principles", true, p).shown.text,
            "## Principles"
        );

        let task = LineKind::Task {
            indent: 0,
            marker: 6,
            checked: true,
        };
        let done = look(&task, "- [x] Ship it", false, p);
        assert_eq!(done.shown.text, "Ship it");
        assert_eq!(done.prefix, Prefix::Checkbox { checked: true });
        assert!(done.styling.iter().any(|s| s.strike));

        let callout = LineKind::CalloutHead {
            kind: "note".into(),
            marker: 2,
        };
        let head = look(&callout, "> [!note] Phase 2", false, p);
        assert_eq!(head.shown.text, "Phase 2");
        assert!(matches!(head.prefix, Prefix::Callout { ref label, .. } if label == "NOTE"));

        let fence = LineKind::FenceOpen {
            lang: "rust".into(),
            block: 1..2,
        };
        let open = look(&fence, "```rust", false, p);
        assert_eq!(
            (
                open.shown.text.as_str(),
                open.label.as_ref().map(|l| l.as_ref())
            ),
            ("", Some("rust"))
        );
    }
}
