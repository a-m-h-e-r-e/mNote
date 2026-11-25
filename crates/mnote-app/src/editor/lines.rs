//! Drawing one line of the note: its text and styling, decorations,
//! selection and caret, the gutter, and the blocks (tables, images,
//! frontmatter) the block renderers draw.

use super::*;

impl Editor {
    pub(super) fn render_line(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let rope = self.buffer.text();
        // A final newline is followed by an empty rope line Vim doesn't show.
        if ix >= line_count(rope) {
            return div().into_any_element();
        }
        let p = cx.theme().palette;
        let start = line_start(rope, ix);
        let len = line_len(rope, ix);
        let content: SharedString = rope.slice(start..start + len).to_string().into();

        let cursor = self.vim.cursor();
        let cursor_line = line_of(rope, cursor);
        // An unfocused pane shows no cursor line.
        let current = ix == cursor_line && !self.drawing_other;
        let number = if current || !self.vim.options().relativenumber {
            ix + 1
        } else {
            ix.abs_diff(cursor_line)
        };

        let kind = line_kind(self.tree.as_ref(), rope, ix);
        // Decorations on this line (see `mnote_core::text::marks`).
        let marks: Vec<mnote_core::Mark> = if self.buffer.marks().is_empty() {
            Vec::new()
        } else {
            self.buffer
                .marks()
                .touching(start..start + len + 1)
                .cloned()
                .collect()
        };
        let (shown, highlights) = self.decorate(ix, start, len, &content, p);
        let substituted = shown != content;
        let raw = current || substituted;
        let gutter_number = SharedString::from(number.to_string());

        // Source and plain views show every line as typed: no hidden
        // markers, no collapsed frontmatter, tables or images.
        // config.toml shows as typed, its `#` comments not headings.
        let config = matches!(self.source, Source::Config(_));
        let styled_view = self.view == styled::View::Styled && !config;
        let plain_view = self.view == styled::View::Plain || config;

        // Blocks (frontmatter, images, tables, and any registered) draw
        // the line their own way in the styled view.
        if styled_view {
            let ctx = blocks::BlockCtx {
                ix,
                kind: &kind,
                content: &content,
                current,
                cursor_line,
                raw,
                gutter: gutter_number.clone(),
                palette: p,
            };
            let renderers = self.blocks.clone();
            for (_, render) in renderers.iter() {
                if let Some(element) = render(self, &ctx, cx) {
                    return element;
                }
            }
        }

        let text = if substituted { &shown } else { &content };
        let mut look = if styled_view {
            styled::look(&kind, text, raw, p)
        } else {
            styled::source_look(&kind, text, plain_view, p)
        };
        if substituted {
            look.shown = StyledLine::plain(&shown);
            look.styling.clear();
        }
        if let LineKind::Code { lang, block } = &kind
            && !plain_view
            && let Some(spans) = self.code_spans(lang, block.clone(), ix, cx)
        {
            look.styling = styled::code_styling(&spans, p);
        }

        // Source positions (bytes of `content`) to positions in the shown
        // text, where markers may be hidden.
        let to_shown = |byte: usize| {
            if substituted {
                byte
            } else {
                look.shown
                    .display_byte(content[..byte.min(content.len())].chars().count())
            }
        };
        let mut highlights: Vec<Highlight> = highlights
            .into_iter()
            .map(|mut h| {
                if !substituted {
                    h.bytes = to_shown(h.bytes.start)..to_shown(h.bytes.end);
                }
                h
            })
            .collect();
        // Highlight decorations, under search matches where they meet.
        let color = |name: &str| p.token(name).map(Theme::color);
        for mark in &marks {
            let mnote_core::MarkKind::Highlight {
                color: fg,
                background,
            } = &mark.kind
            else {
                continue;
            };
            if substituted {
                continue;
            }
            let from = mark.range.start.clamp(start, start + len) - start;
            let to = mark.range.end.clamp(start, start + len) - start;
            if to <= from {
                continue;
            }
            let bytes =
                to_shown(char_to_byte(&content, from))..to_shown(char_to_byte(&content, to));
            if highlights
                .iter()
                .any(|h| h.bytes.start < bytes.end && bytes.start < h.bytes.end)
            {
                continue;
            }
            highlights.push(Highlight {
                bytes,
                background: background
                    .as_deref()
                    .and_then(color)
                    .unwrap_or(gpui::transparent_black()),
                color: fg.as_deref().and_then(color).unwrap_or(Theme::color(p.ink)),
                outline: None,
            });
        }
        highlights.sort_by_key(|h| h.bytes.start);

        // Text still being composed shows at the caret, underlined. Only the
        // cursor line has a caret (elsewhere the cursor may be before `start`).
        let caret_byte = if current {
            to_shown(char_to_byte(&content, cursor - start))
        } else {
            0
        };
        let marked = self
            .marked
            .clone()
            .filter(|_| current && self.vim.mode() == Mode::Insert);
        let marked_len = marked.as_ref().map_or(0, String::len);
        let shown_len = look.shown.text.len();

        // Text drawn inside the line that isn't part of the note: what's
        // being composed at the caret, then ghost decorations. Positions are
        // bytes of the shown text; `inserts` maps them to what's drawn.
        let mut added: Vec<(usize, String, line::Styling)> = Vec::new();
        if let Some(marked) = &marked {
            added.push((
                caret_byte.min(shown_len),
                marked.clone(),
                line::Styling {
                    color: Some(Theme::color(p.cursor)),
                    underline: true,
                    ..line::Styling::default()
                },
            ));
        }
        if !substituted {
            for mark in &marks {
                let mnote_core::MarkKind::Ghost {
                    text: ghost,
                    color: c,
                } = &mark.kind
                else {
                    continue;
                };
                let at = mark.range.start;
                if at < start || at > start + len || ghost.is_empty() {
                    continue;
                }
                added.push((
                    to_shown(char_to_byte(&content, at - start)).min(shown_len),
                    ghost.clone(),
                    line::Styling {
                        color: Some(color(c).unwrap_or(Theme::color(p.ink_3))),
                        ..line::Styling::default()
                    },
                ));
            }
        }
        // Stable: at one position, composed text comes before ghost text.
        added.sort_by_key(|(at, ..)| *at);
        let inserts = Inserts(added.iter().map(|(at, t, _)| (*at, t.len())).collect());
        let mut text: String = look.shown.text.to_string();
        let mut styling = look.styling.clone();
        for s in &mut styling {
            s.bytes = inserts.range(s.bytes.clone());
        }
        for h in &mut highlights {
            h.bytes = inserts.range(h.bytes.clone());
        }
        let mut offset = 0;
        for (at, piece, style) in &added {
            let drawn = at + offset;
            text.insert_str(drawn, piece);
            styling.push(line::Styling {
                bytes: drawn..drawn + piece.len(),
                ..style.clone()
            });
            offset += piece.len();
        }
        let drawn_len = text.len();

        let mut line = LineText::new(
            text.into(),
            look.font(),
            look.style.size(),
            look.style.line_height(),
            look.color,
        )
        .styling(styling)
        .highlights(highlights);
        if !self.vim.options().wrap {
            line = line.nowrap(self.scroll_x.clone());
        }

        // The mouse: the byte under the pointer back to the note's
        // character, then a click, drag or selection in this pane.
        {
            let pane = if self.drawing_other {
                self.drawing_pane
            } else {
                self.focused
            };
            let view = cx.entity().downgrade();
            let map = Rc::new((
                content.clone(),
                look.shown.clone(),
                substituted,
                inserts.clone(),
            ));
            // A drawn byte to a character of the note's line (inside
            // inserted text: where it's inserted).
            let to_char = move |byte: usize| {
                let (content, shown, substituted, inserts) = &*map;
                let byte = inserts.back(byte);
                if *substituted {
                    let mut b = byte.min(content.len());
                    while !content.is_char_boundary(b) {
                        b -= 1;
                    }
                    content[..b].chars().count()
                } else {
                    (0..=content.chars().count())
                        .take_while(|&c| shown.display_byte(c) <= byte)
                        .last()
                        .unwrap_or(0)
                }
            };
            // Where this line wraps, for `gj` / `gk` (the focused pane's).
            if !self.drawing_other {
                let wraps = self.wraps.clone();
                let to_char = to_char.clone();
                line = line.on_wraps(Rc::new(move |bytes| {
                    let starts = bytes.into_iter().map(&to_char).collect();
                    wraps.borrow_mut().insert(ix, starts);
                }));
            }
            line = line.on_mouse(Rc::new(move |byte, hit, _window, cx| {
                let ch = to_char(byte);
                let _ = view.update(cx, |this, cx| this.mouse_on_line(pane, ix, ch, hit, cx));
            }));
        }

        if let Some(chars) = self.selection_on_line(ix, start, len) {
            let to_byte = |c: usize| to_shown(char_to_byte(&content, c));
            let drawn = inserts.range(to_byte(chars.start)..to_byte(chars.end.min(len)));
            let end = if chars.end > len {
                drawn_len + 1
            } else {
                drawn.end
            };
            line = line.selection(drawn.start..end, Theme::color(p.select));
        }
        let mode = self.vim.mode();
        // In command mode the caret is in the command line.
        if current && mode != Mode::Command && (self.caret_on || mode != Mode::Insert) {
            line = line.caret(Some(Caret {
                // Before ghost text at the same place, after composed text.
                byte: inserts.before(caret_byte) + marked_len,
                shape: self.vim.mode().caret(),
                color: Theme::color(p.cursor),
                on_color: Theme::color(p.on_cursor),
            }));
        }

        let line_height = look.style.line_height();
        let prefix: Option<AnyElement> = match &look.prefix {
            styled::Prefix::None => None,
            styled::Prefix::Bullet => Some(
                div()
                    .flex_none()
                    .w(px(2. * styled::char_w()))
                    .text_style(text::NOTE_BODY)
                    .text_color(Theme::color(p.ink_3))
                    .child(SharedString::new_static("•"))
                    .into_any_element(),
            ),
            styled::Prefix::Checkbox { checked } => Some(
                div()
                    .flex_none()
                    .w(px(2. * styled::char_w()))
                    .h(line_height)
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .size(px(14.))
                            .rounded(px(3.))
                            .border_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(11.))
                            .line_height(px(14.))
                            .when(*checked, |b| {
                                b.bg(Theme::color(p.cursor))
                                    .border_color(Theme::color(p.cursor))
                                    .text_color(Theme::color(p.on_cursor))
                                    .child(SharedString::new_static("✓"))
                            })
                            .when(!checked, |b| b.border_color(Theme::color(p.line_strong))),
                    )
                    .into_any_element(),
            ),
            styled::Prefix::Callout { label, color } => Some(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .pr(px(space::S2))
                    .h(line_height)
                    .text_style(text::HINT)
                    .text_color(*color)
                    .child(div().size(px(6.)).rounded(px(1.)).bg(*color))
                    .child(SharedString::from(label.clone()))
                    .into_any_element(),
            ),
        };

        let body: AnyElement = if look.rule {
            div()
                .h(look.height.unwrap_or(line_height))
                .flex()
                .items_center()
                .child(div().w_full().h(px(1.)).bg(Theme::color(p.line)))
                .into_any_element()
        } else {
            div()
                .flex()
                .items_start()
                .pl(px(look.indent))
                .when(look.background.is_some(), |row| row.px(px(space::S3)))
                .when_some(look.background, |row, bg| row.bg(bg))
                .when(look.round_top, |row| row.rounded_t(px(radius::MD)))
                .when(look.round_bottom, |row| row.rounded_b(px(radius::MD)))
                .when_some(look.height, |row, h| row.h(h).overflow_hidden())
                .children(prefix)
                .child(div().flex_1().min_w_0().child(line))
                .children(marks.iter().filter_map(|m| {
                    match &m.kind {
                        mnote_core::MarkKind::VirtualText {
                            text: label,
                            color: c,
                            right: false,
                        } => Some(
                            div()
                                .flex_none()
                                .pl(px(space::S3))
                                .h(line_height)
                                .flex()
                                .items_center()
                                .text_style(text::HINT)
                                .text_color(color(c).unwrap_or(Theme::color(p.ink_3)))
                                .child(SharedString::from(label.clone())),
                        ),
                        _ => None,
                    }
                }))
                .children(look.label.clone().map(|label| {
                    div()
                        .flex_none()
                        .pl(px(space::S2))
                        .h(px(28.))
                        .flex()
                        .items_center()
                        .text_style(text::HINT)
                        .font_weight(gpui::FontWeight::NORMAL)
                        .text_color(Theme::color(p.ink_3))
                        .child(label)
                }))
                .into_any_element()
        };

        // Line decorations: a background, a gutter sign, text at the right.
        let line_bg = marks.iter().find_map(|m| match &m.kind {
            mnote_core::MarkKind::LineBackground { background } => color(background),
            _ => None,
        });
        let sign = marks.iter().find_map(|m| match &m.kind {
            mnote_core::MarkKind::Sign { text: t, color: c } => Some((
                SharedString::from(t.clone()),
                color(c).unwrap_or(Theme::color(p.ink_3)),
            )),
            _ => None,
        });
        let row_bg = Theme::color(if current { p.well } else { p.paper });
        let right_text: Vec<(SharedString, gpui::Hsla)> = marks
            .iter()
            .filter_map(|m| match &m.kind {
                mnote_core::MarkKind::VirtualText {
                    text: t,
                    color: c,
                    right: true,
                } => Some((
                    SharedString::from(t.clone()),
                    color(c).unwrap_or(Theme::color(p.ink_3)),
                )),
                _ => None,
            })
            .collect();

        // Line numbers in the note's own font and size, as in Zed, centred
        // on the first row of the line.
        div()
            .flex()
            .relative()
            .w_full()
            .when(current, |row| row.bg(Theme::color(p.well)))
            .when_some(line_bg, |row, bg| row.bg(bg))
            .when(self.in_visual_range(ix), |row| {
                row.bg(Theme::color(p.select))
            })
            .child(
                div()
                    .relative()
                    .flex_none()
                    .w(px(size::GUTTER_W))
                    .pr(px(space::S8))
                    .text_right()
                    .text_style(text::NOTE_BODY)
                    .line_height(look.height.unwrap_or(line_height))
                    .text_color(Theme::color(if current { p.ink } else { p.ink_3 }))
                    .children(sign.map(|(t, c)| {
                        div()
                            .absolute()
                            .left(px(space::S4))
                            .top_0()
                            .text_color(c)
                            .child(t)
                    }))
                    .child(gutter_number),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .max_w(px(size::MEASURE))
                    .child(body),
            )
            .children(right_text.into_iter().map(|(label, c)| {
                div()
                    .absolute()
                    .right_0()
                    .top_0()
                    .h(line_height)
                    .flex()
                    .items_center()
                    .pl(px(space::S4))
                    .pr(px(space::S2))
                    .bg(line_bg.unwrap_or(row_bg))
                    .text_style(text::HINT)
                    .text_color(c)
                    .child(label)
            }))
            .into_any_element()
    }

    /// One row of a table as cells side by side. Each cell wraps its own
    /// text; column widths come from the longest cell in each column, so
    /// every row of the table lines up. The `|---|` row isn't drawn.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn table_row(
        &mut self,
        ix: usize,
        block: std::ops::Range<usize>,
        delimiter: bool,
        content: &str,
        number: SharedString,
        current: bool,
        p: &Palette,
    ) -> AnyElement {
        if delimiter {
            return div().h(px(0.)).into_any_element();
        }
        let layout = self.table_layout(block.clone());
        // Each column's share of the width: its longest cell, within limits
        // so a short column stays readable and a long one doesn't take all.
        let weights: Vec<f32> = layout.0.iter().map(|&w| w.clamp(6, 60) as f32).collect();
        let total = weights.iter().sum::<f32>().max(1.);
        let cells = mnote_core::styled::table_cells(content);
        let columns = weights.len().max(cells.len()).max(1);
        let header = ix == block.start;
        let last = ix + 1 >= block.end;
        let body = text::NOTE_BODY;
        let line = Theme::color(p.line_strong);
        let row = div()
            .flex()
            .w_full()
            .border_l_1()
            .border_r_1()
            .border_b_1()
            .border_color(line)
            .when(header, |row| row.border_t_1().rounded_t(px(radius::MD)))
            .when(last, |row| row.rounded_b(px(radius::MD)))
            .children((0..columns).map(|c| {
                let cell = cells.get(c).cloned().unwrap_or_default();
                let shown = mnote_core::styled::style_inline(&cell, false, 0);
                let mut styling = styled::inline_styling(&shown, p);
                if header {
                    styling.insert(
                        0,
                        line::Styling {
                            bytes: 0..shown.text.len(),
                            weight: Some(gpui::FontWeight::SEMIBOLD),
                            ..line::Styling::default()
                        },
                    );
                }
                let share = weights.get(c).copied().unwrap_or(6.) / total;
                div()
                    .w(gpui::relative(share))
                    .min_w_0()
                    .px(px(space::S3))
                    .py(px(6.))
                    .when(c + 1 < columns, |cell| cell.border_r_1().border_color(line))
                    .child(
                        LineText::new(
                            shown.text.clone().into(),
                            Font {
                                weight: body.font_weight(),
                                ..font(body.font_family())
                            },
                            body.size(),
                            body.line_height(),
                            Theme::color(p.ink),
                        )
                        .styling(styling),
                    )
            }));
        div()
            .flex()
            .w_full()
            .when(current, |r| r.bg(Theme::color(p.well)))
            .child(
                div()
                    .flex_none()
                    .w(px(size::GUTTER_W))
                    .pr(px(space::S8))
                    .pt(px(6.))
                    .text_right()
                    .text_style(body)
                    .text_color(Theme::color(if current { p.ink } else { p.ink_3 }))
                    .child(number),
            )
            .child(div().flex_1().min_w_0().max_w(px(size::MEASURE)).child(row))
            .into_any_element()
    }

    /// The rows of the table around line `ix`: from the tree when it has
    /// them, else the `|` lines next to it.
    pub(super) fn table_block(
        &self,
        block: std::ops::Range<usize>,
        ix: usize,
    ) -> std::ops::Range<usize> {
        if block.contains(&ix) {
            return block;
        }
        let rope = self.buffer.text();
        let is_row =
            |line: usize| rope.line(line).chars().find(|c| !c.is_whitespace()) == Some('|');
        let mut start = ix;
        while start > 0 && is_row(start - 1) {
            start -= 1;
        }
        let mut end = ix + 1;
        while end < line_count(rope) && is_row(end) {
            end += 1;
        }
        start..end
    }

    /// Column widths and alignments of the table on rows `block`, once per
    /// table and text revision.
    pub(super) fn table_layout(&mut self, block: std::ops::Range<usize>) -> TableLayout {
        let revision = self.buffer.revision();
        if let Some((rev, layout)) = self.table_cache.get(&block.start)
            && *rev == revision
        {
            return layout.clone();
        }
        let rope = self.buffer.text();
        let first = block.start;
        let rows: Vec<String> = block
            .map(|row| {
                let start = line_start(rope, row);
                rope.slice(start..start + line_len(rope, row)).to_string()
            })
            .collect();
        let layout = Rc::new(mnote_core::styled::table_layout(&rows));
        if self.table_cache.len() > 64 {
            self.table_cache.retain(|_, (rev, _)| *rev == revision);
        }
        self.table_cache.insert(first, (revision, layout.clone()));
        layout
    }

    /// An image on its own line: the picture, or a quiet note when it
    /// can't be shown.
    pub(super) fn image_row(
        &self,
        number: SharedString,
        current: bool,
        src: &str,
        alt: &str,
        p: &Palette,
    ) -> AnyElement {
        let gutter = div()
            .flex_none()
            .w(px(size::GUTTER_W))
            .pr(px(space::S8))
            .text_right()
            .text_style(text::NOTE_BODY)
            .text_color(Theme::color(if current { p.ink } else { p.ink_3 }))
            .child(number);
        let note_text = |message: String| {
            div()
                .text_style(text::STATUS)
                .line_height(text::NOTE_BODY.line_height())
                .text_color(Theme::color(p.ink_3))
                .child(SharedString::from(message))
        };
        let shown: AnyElement = match self.image_path(src) {
            Some(path) => {
                let missing = format!("image not found: {src}");
                gpui::img(path)
                    .max_w(px(size::MEASURE))
                    .max_h(px(480.))
                    .rounded(px(radius::MD))
                    .object_fit(gpui::ObjectFit::Contain)
                    .with_fallback(move || {
                        div()
                            .text_style(text::STATUS)
                            .child(SharedString::from(missing.clone()))
                            .into_any_element()
                    })
                    .into_any_element()
            }
            None if src.starts_with("http://") || src.starts_with("https://") => {
                note_text(format!("web image, not loaded: {src}")).into_any_element()
            }
            None => note_text(format!("image not found: {src}")).into_any_element(),
        };
        div()
            .flex()
            .w_full()
            .py(px(space::S1))
            .when(current, |row| row.bg(Theme::color(p.well)))
            .child(gutter)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .max_w(px(size::MEASURE))
                    .child(shown)
                    .when(!alt.is_empty(), |col| {
                        col.child(
                            div()
                                .text_style(text::STATUS)
                                .text_color(Theme::color(p.ink_3))
                                .child(SharedString::from(alt.to_owned())),
                        )
                    }),
            )
            .into_any_element()
    }

    /// Where an image path points: next to the note first, then from the
    /// vault's top folder. Web images aren't loaded.
    pub(super) fn image_path(&self, src: &str) -> Option<std::path::PathBuf> {
        if src.contains("://") {
            return None;
        }
        if let Some(found) = self.image_paths.borrow().get(src) {
            return found.clone();
        }
        let found = self.find_image(src);
        self.image_paths
            .borrow_mut()
            .insert(src.to_owned(), found.clone());
        found
    }

    pub(super) fn find_image(&self, src: &str) -> Option<std::path::PathBuf> {
        let Source::Note { vault, note, .. } = &self.source else {
            return None;
        };
        let src = src.replace("%20", " ");
        let beside = vault.path_of(note).parent().map(|dir| dir.join(&src));
        let from_root = vault.root().join(src.trim_start_matches('/'));
        [beside, Some(from_root)]
            .into_iter()
            .flatten()
            .find(|p| p.starts_with(vault.root()) && p.is_file())
    }

    /// The frontmatter's first line, collapsed to its tags.
    pub(super) fn frontmatter_row(
        &self,
        number: SharedString,
        current: bool,
        tags: &[String],
        p: &Palette,
    ) -> AnyElement {
        div()
            .flex()
            .w_full()
            .when(current, |row| row.bg(Theme::color(p.well)))
            .child(
                div()
                    .flex_none()
                    .w(px(size::GUTTER_W))
                    .pr(px(space::S8))
                    .text_right()
                    .text_style(text::NOTE_BODY)
                    .text_color(Theme::color(p.ink_3))
                    .child(number),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .py(px(2.))
                    .max_w(px(size::MEASURE))
                    .children(tags.iter().map(|tag| {
                        div()
                            .h(px(22.))
                            .px(px(space::S2))
                            .flex()
                            .items_center()
                            .rounded(px(11.))
                            .bg(Theme::color(p.well))
                            .text_style(text::UI)
                            .text_size(px(13.))
                            .text_color(Theme::color(p.ink_2))
                            .child(SharedString::from(format!("#{tag}")))
                    }))
                    .when(tags.is_empty(), |row| {
                        row.text_style(text::STATUS)
                            .text_color(Theme::color(p.ink_3))
                            .child(SharedString::new_static("frontmatter"))
                    }),
            )
            .into_any_element()
    }

    /// The code colours of line `ix` in the block `block` (its content rows),
    /// computed once per block and text revision.
    pub(super) fn code_spans(
        &mut self,
        lang: &str,
        block: std::ops::Range<usize>,
        ix: usize,
        cx: &mut Context<Self>,
    ) -> Option<Vec<(std::ops::Range<usize>, CodeStyle)>> {
        if !mnote_core::code::supported(lang) {
            // A grammar may be loadable (Neovim's, say): asked after the frame.
            self.want_grammar(lang, cx);
            return None;
        }
        // Past the highlight limit, a huge note stays plain.
        if self.buffer.text().len_bytes() > mnote_core::lang::syntax::limits().highlight {
            return None;
        }
        let revision = self.buffer.revision();
        let cached = self
            .code_cache
            .get(&block.start)
            .filter(|(rev, lines)| *rev == revision && lines.len() == block.len())
            .map(|(_, lines)| lines.clone());
        let lines = match cached {
            Some(lines) => lines,
            None => {
                // The fence's layer in the syntax map: its tree is already
                // parsed (incrementally, in the background), so this only
                // queries the block's bytes.
                let language = mnote_core::lang::languages::pack(lang)
                    .map_or_else(|| lang.to_owned(), |p| p.name);
                let (first, last) = (block.start, block.end.saturating_sub(1).max(block.start));
                let (from, to) = {
                    let rope = self.buffer.text();
                    let from = rope.char_to_byte(line_start(rope, first));
                    let to = rope.char_to_byte(line_start(rope, last) + line_len(rope, last));
                    (from, to)
                };
                let tree = self
                    .buffer
                    .layer_at(from, &language)
                    .map(|layer| layer.tree.clone())?;
                let current = self.buffer.syntax_is_current();
                let rope = self.buffer.text();
                let spans = mnote_core::code::highlight_tree(lang, &tree, rope, from..to.max(from))
                    .unwrap_or_default();
                // Split the block's spans into lines, as bytes of each line.
                let mut per_line = Vec::with_capacity(block.len());
                for row in block.clone() {
                    let first_char = line_start(rope, row);
                    let start = rope.char_to_byte(first_char);
                    let end = rope.char_to_byte(first_char + line_len(rope, row));
                    per_line.push(
                        spans
                            .iter()
                            .filter(|(r, _)| r.start < end && r.end > start)
                            .map(|(r, s)| (r.start.max(start) - start..r.end.min(end) - start, *s))
                            .collect(),
                    );
                }
                let lines = Rc::new(per_line);
                // A tree that's waiting for its re-parse is drawn, not kept.
                if current {
                    if self.code_cache.len() > 64 {
                        self.code_cache.retain(|_, (rev, _)| *rev == revision);
                    }
                    self.code_cache
                        .insert(block.start, (revision, lines.clone()));
                }
                lines
            }
        };
        lines.get(ix.checked_sub(block.start)?).cloned()
    }

    /// Whether line `ix` is in the `'<,'>` range of the command being typed,
    /// so the selection stays tinted while the command line is open.
    pub(super) fn in_visual_range(&self, ix: usize) -> bool {
        if self.drawing_other {
            return false;
        }
        let Some(line) = self.vim.command_line() else {
            return false;
        };
        if !line.text.trim_start().starts_with("'<,'>") {
            return false;
        }
        let rope = self.buffer.text();
        match (self.vim.mark('<'), self.vim.mark('>')) {
            (Some(a), Some(b)) => {
                let (a, b) = (line_of(rope, a.min(b)), line_of(rope, a.max(b)));
                (a..=b).contains(&ix)
            }
            _ => false,
        }
    }

    /// The text to show for line `ix` and its highlights: `:s` replacements
    /// while one is typed, else matches of the pattern being typed, else
    /// matches of the last search until `:noh`.
    pub(super) fn decorate(
        &self,
        ix: usize,
        start: usize,
        len: usize,
        content: &SharedString,
        p: &Palette,
    ) -> (SharedString, Vec<Highlight>) {
        // A command being typed previews in the focused pane only.
        let preview = self.vim.preview().filter(|_| !self.drawing_other);
        if let Some(preview) = preview.filter(|pv| !pv.substitutions.is_empty()) {
            let subs = preview.substitutions_in(start..start + len);
            if subs.is_empty() {
                return (content.clone(), Vec::new());
            }
            let mut shown = String::with_capacity(content.len() + 16);
            let mut highlights = Vec::with_capacity(subs.len());
            let mut byte = 0;
            for (range, replacement) in subs {
                let from = char_to_byte(content, range.start - start);
                let to = char_to_byte(content, range.end.min(start + len) - start);
                shown.push_str(&content[byte.min(from)..from]);
                let at = shown.len();
                // A replacement that splits the line shows where it breaks.
                shown.push_str(&replacement.replace('\n', "⏎"));
                highlights.push(Highlight {
                    bytes: at..shown.len(),
                    background: Theme::color(p.insert_wash),
                    color: Theme::color(p.insert),
                    outline: (preview.current.as_ref() == Some(range))
                        .then(|| Theme::color(p.insert)),
                });
                byte = to.max(byte);
            }
            shown.push_str(&content[byte..]);
            return (shown.into(), highlights);
        }

        let (pattern, current) = match preview {
            Some(pv) if pv.covers(ix) => (pv.pattern.as_ref(), pv.current.clone()),
            Some(pv) if pv.pattern.is_some() => (None, None),
            _ => (self.vim.highlight_pattern(), None),
        };
        let Some(pattern) = pattern else {
            return (content.clone(), Vec::new());
        };
        let highlights = line_matches(pattern, content, start)
            .filter(|m| !m.is_empty())
            .map(|m| {
                let next = current.as_ref() == Some(&m);
                Highlight {
                    bytes: char_to_byte(content, m.start - start)
                        ..char_to_byte(content, m.end - start),
                    background: Theme::color(if next { p.cursor } else { p.match_ }),
                    color: Theme::color(if next { p.on_cursor } else { p.ink }),
                    outline: None,
                }
            })
            .collect();
        (content.clone(), highlights)
    }

    /// The selected characters on line `ix` (relative to the line start). An
    /// end past `len` means the line's newline is selected too.
    pub(super) fn selection_on_line(
        &self,
        ix: usize,
        start: usize,
        len: usize,
    ) -> Option<std::ops::Range<usize>> {
        if self.drawing_other {
            return None;
        }
        let rope = self.buffer.text();
        let (anchor, cursor) = (self.vim.anchor(), self.vim.cursor());
        let (a, b) = (anchor.min(cursor), anchor.max(cursor));
        match self.vim.mode() {
            Mode::VisualLine => (line_of(rope, a)..=line_of(rope, b))
                .contains(&ix)
                .then_some(0..len + 1),
            // One rectangle per line; lines shorter than its left edge have
            // none.
            Mode::VisualBlock => self.vim.block(rope)?.columns(rope, ix),
            Mode::Visual => {
                let end = start + len;
                if b < start || a > end {
                    return None;
                }
                let from = a.max(start) - start;
                // The selection includes the character under the cursor; past
                // the line's last character that's the newline.
                let to = (b + 1).min(end + 1) - start;
                Some(from..to)
            }
            _ => None,
        }
    }
}

/// Text drawn inside a line that isn't part of it (ghost text, text being
/// composed): where each piece goes in the shown text, and how long it is,
/// sorted. Maps shown bytes to drawn bytes and back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Inserts(Vec<(usize, usize)>);

impl Inserts {
    /// Where shown byte `b` is drawn, before anything inserted there.
    fn before(&self, b: usize) -> usize {
        b + self
            .0
            .iter()
            .filter(|(at, _)| *at < b)
            .map(|(_, l)| l)
            .sum::<usize>()
    }

    /// Where shown byte `b` is drawn, after anything inserted there.
    fn after(&self, b: usize) -> usize {
        b + self
            .0
            .iter()
            .filter(|(at, _)| *at <= b)
            .map(|(_, l)| l)
            .sum::<usize>()
    }

    /// A shown range as drawn: text inserted at its start comes before it,
    /// text inserted at its end after it.
    fn range(&self, r: std::ops::Range<usize>) -> std::ops::Range<usize> {
        if r.is_empty() {
            let at = self.after(r.start);
            return at..at;
        }
        self.after(r.start)..self.before(r.end)
    }

    /// A drawn byte back to the shown text; inside inserted text, the
    /// place it's inserted.
    fn back(&self, drawn: usize) -> usize {
        let mut offset = 0;
        for (at, len) in &self.0 {
            let start = at + offset;
            if drawn < start {
                break;
            }
            if drawn < start + len {
                return *at;
            }
            offset += len;
        }
        drawn - offset
    }
}

#[cfg(test)]
mod insert_tests {
    use super::Inserts;

    #[test]
    fn drawn_and_shown_bytes_map_both_ways() {
        // "abcdef" with 3 bytes drawn at 2 and 2 bytes at 4: "ab___cd__ef".
        let i = Inserts(vec![(2, 3), (4, 2)]);
        assert_eq!((i.before(2), i.after(2)), (2, 5));
        assert_eq!(i.before(5), 10);
        assert_eq!(
            i.range(2..4),
            5..7,
            "a range starting at ghost text starts after it"
        );
        assert_eq!(i.range(0..2), 0..2, "and one ending there ends before it");
        for (drawn, shown) in [
            (0, 0),
            (2, 2),
            (3, 2),
            (4, 2),
            (5, 2),
            (6, 3),
            (7, 4),
            (8, 4),
            (9, 4),
            (10, 5),
        ] {
            assert_eq!(i.back(drawn), shown, "drawn byte {drawn}");
        }
        let none = Inserts::default();
        assert_eq!(
            (none.before(3), none.range(1..2), none.back(4)),
            (3, 1..2, 4)
        );
    }
}
