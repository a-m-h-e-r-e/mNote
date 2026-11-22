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

/// How notes are shown, cycled with `space m`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    /// Formatting shown, markers hidden off the cursor line.
    #[default]
    Styled,
    /// Every marker shown, in syntax colours.
    Source,
    /// Every marker shown, no colour.
    Plain,
}

impl View {
    pub fn name(self) -> &'static str {
        match self {
            View::Styled => "styled",
            View::Source => "source",
            View::Plain => "plain",
        }
    }

    pub fn next(self) -> Self {
        match self {
            View::Styled => View::Source,
            View::Source => View::Plain,
            View::Plain => View::Styled,
        }
    }

    /// The view config.toml names.
    pub fn from_config(view: mnote_core::config::ViewChoice) -> Self {
        match view {
            mnote_core::config::ViewChoice::Styled => View::Styled,
            mnote_core::config::ViewChoice::Source => View::Source,
            mnote_core::config::ViewChoice::Plain => View::Plain,
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [View::Styled, View::Source, View::Plain]
            .into_iter()
            .find(|v| v.name().starts_with(name) && !name.is_empty())
    }
}

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
pub fn inline_styling(shown: &StyledLine, p: &Palette) -> Vec<Styling> {
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
        LineKind::Table { .. } | LineKind::Image { .. } | LineKind::Text => plain(body, ink, 0),
    }
}

/// A line in the source view (every marker shown, syntax colours) or, with
/// `plain`, the plain view (no colour at all).
pub fn source_look(kind: &LineKind, content: &str, plain: bool, p: &Palette) -> Look {
    let shown = style_inline(content, true, 0);
    let mut look = Look::new(shown, text::NOTE_BODY, Theme::color(p.ink));
    if plain {
        return look;
    }
    let colour = |c: u32| Some(Theme::color(c));
    // Markers take the colour of what they mark: `**` with the bold text.
    let spans = &look.shown.spans;
    let style_colour = |style: Inline| match style {
        Inline::Bold => colour(p.syn_strong),
        Inline::Italic => colour(p.ink_2),
        Inline::Code => colour(p.syn_code),
        Inline::Strike => colour(p.ink_3),
        Inline::Link | Inline::Tag => colour(p.syn_link),
        Inline::Url => colour(p.syn_url),
        Inline::Marker => colour(p.syn_marker),
    };
    let mut styling: Vec<Styling> = spans
        .iter()
        .enumerate()
        .map(|(i, span)| {
            let style = if span.style == Inline::Marker {
                // The marked span next to it, if they touch.
                let next = spans
                    .get(i + 1)
                    .filter(|n| n.range.start == span.range.end && n.style != Inline::Marker);
                let prev = i
                    .checked_sub(1)
                    .and_then(|k| spans.get(k))
                    .filter(|p| p.range.end == span.range.start && p.style != Inline::Marker);
                next.or(prev).map_or(Inline::Marker, |s| s.style)
            } else {
                span.style
            };
            Styling {
                bytes: span.range.clone(),
                color: style_colour(style),
                weight: (style == Inline::Bold).then_some(FontWeight::SEMIBOLD),
                ..Styling::default()
            }
        })
        .collect();
    let whole = |c: u32| Styling {
        bytes: 0..content.len(),
        color: colour(c),
        ..Styling::default()
    };
    let lead = |chars: usize| {
        let end = content
            .char_indices()
            .nth(chars)
            .map_or(content.len(), |(b, _)| b);
        Styling {
            bytes: 0..end,
            color: colour(p.syn_marker),
            ..Styling::default()
        }
    };
    match kind {
        LineKind::Heading { .. } => styling = vec![whole(p.syn_heading)],
        LineKind::Bullet { marker, .. }
        | LineKind::Task { marker, .. }
        | LineKind::Quote { marker }
        | LineKind::Callout { marker }
        | LineKind::CalloutHead { marker, .. } => styling.insert(0, lead(*marker)),
        LineKind::FenceOpen { .. } | LineKind::FenceClose => styling = vec![whole(p.ink_3)],
        LineKind::Frontmatter { .. } => styling = vec![whole(p.ink_2)],
        LineKind::Rule => styling = vec![whole(p.syn_marker)],
        LineKind::Table { .. } => {
            styling = content
                .char_indices()
                .filter(|(_, c)| *c == '|')
                .map(|(b, _)| Styling {
                    bytes: b..b + 1,
                    color: colour(p.syn_marker),
                    ..Styling::default()
                })
                .collect();
        }
        LineKind::Code { .. } => styling.clear(),
        LineKind::Image { .. } | LineKind::Text => {}
    }
    look.styling = styling;
    look
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
    fn views_cycle_and_parse() {
        assert_eq!(View::Styled.next().next().next(), View::Styled);
        assert_eq!(View::from_name("so"), Some(View::Source));
        assert_eq!(View::from_name("plain"), Some(View::Plain));
        assert_eq!(View::from_name("x"), None);
    }

    #[test]
    fn source_shows_markers_in_colour_plain_in_none() {
        let p = Theme::new(Appearance::Dark).palette;
        let heading = LineKind::Heading {
            level: 2,
            marker: 3,
        };
        let source = source_look(&heading, "## Principles", false, p);
        assert_eq!(source.shown.text, "## Principles");
        assert_eq!(source.styling[0].color, Some(Theme::color(p.syn_heading)));
        let bold = source_look(&LineKind::Text, "a **b** c", false, p);
        assert!(
            bold.styling
                .iter()
                .all(|s| s.color == Some(Theme::color(p.syn_strong))),
            "markers share the bold colour"
        );
        let plain = source_look(&heading, "## Principles", true, p);
        assert!(plain.styling.is_empty());
    }

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
