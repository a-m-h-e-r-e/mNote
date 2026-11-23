//! Drawing the window: the panes and their layout, the top bar, the status
//! line, the command line and its completion, the command-line window, and
//! the overlays (pickers, the folder editor, toasts). Note lines are drawn
//! in `lines`.

use super::*;

impl Editor {
    /// What replaces the status line: the line being typed with its live
    /// count, or the last command and its result. Empty means the status
    /// line shows.
    pub(super) fn command_area(&self) -> Vec<AnyElement> {
        if let Some(line) = self.vim.command_line() {
            return vec![self.typed_line(line).into_any_element()];
        }
        let (text, error, line) = match (self.vim.message(), &self.reply) {
            (Some(m), _) => (m.text.clone(), m.error, m.line.clone()),
            (None, Some((text, error))) => (text.to_string(), *error, None),
            (None, None) => return Vec::new(),
        };
        if text.contains('\n') {
            let mut area = vec![MessagePanel::new(&text).into_any_element()];
            if let Some(line) = line {
                area.push(CommandLine::new(line.kind, line.text).into_any_element());
            }
            return area;
        }
        let bar = match line {
            Some(line) => CommandLine::new(line.kind, line.text)
                .info(text)
                .error(error),
            None => CommandLine::message(text, error),
        };
        vec![bar.into_any_element()]
    }
}

impl Editor {
    pub(super) fn typed_line(&self, line: &mnote_vim::ex::cmdline::CommandLine) -> CommandLine {
        let caret = self.caret_on.then_some(line.cursor);
        let mut bar = CommandLine::new(line.kind, line.text.clone()).caret(caret);
        if let Some(info) = self.vim.preview().and_then(|p| p.info.clone()) {
            bar = bar.info(info);
        }
        bar
    }

    /// The `⇥` popup, if open: placed by the caller just above the line.
    pub(super) fn completion_popup(&self) -> Option<CompletionPopup> {
        let completion = self.vim.command_line()?.completion.as_ref()?;
        let (first, rows) = completion.visible();
        let rows = rows
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let selected = completion.selected == Some(first + i);
                (c.text.clone().into(), c.detail.clone().into(), selected)
            })
            .collect();
        Some(CompletionPopup::new(rows))
    }

    /// The command-line window (`q:`): history lines in mono, with the
    /// window's own caret and status line, above the bottom bar.
    pub(super) fn cmd_window_area(&self, win: &CmdWindow, p: &Palette) -> Vec<AnyElement> {
        let rope = win.buffer.text();
        let (first, last) = win.visible();
        let cursor = win.vim.cursor();
        let cursor_line = line_of(rope, cursor);
        let code = text::CODE;
        let mono = Font {
            weight: code.font_weight(),
            ..font(crate::theme::mono())
        };
        let rows = (first..=last).map(|ix| {
            let start = line_start(rope, ix);
            let len = line_len(rope, ix);
            let content: SharedString = rope.slice(start..start + len).to_string().into();
            let current = ix == cursor_line;
            let mode = win.vim.mode();
            let mut line = LineText::new(
                content.clone(),
                mono.clone(),
                code.size(),
                code.line_height(),
                Theme::color(p.ink),
            );
            if current && mode != Mode::Command && (self.caret_on || mode != Mode::Insert) {
                line = line.caret(Some(Caret {
                    byte: char_to_byte(&content, cursor - start),
                    shape: mode.caret(),
                    color: Theme::color(p.cursor),
                    on_color: Theme::color(p.on_cursor),
                }));
            }
            div()
                .flex()
                .px(px(space::S3))
                .when(current, |row| row.bg(Theme::color(p.well)))
                .child(
                    div()
                        .flex_none()
                        .w(px(space::S6))
                        .text_style(text::STATUS)
                        .line_height(code.line_height())
                        .text_color(Theme::color(p.ink_3))
                        .child(SharedString::new_static(match win.kind.prefix() {
                            ':' => ":",
                            '/' => "/",
                            _ => "?",
                        })),
                )
                .child(div().flex_1().min_w_0().child(line))
        });
        let panel = div()
            .flex()
            .flex_col()
            .flex_none()
            .py(px(space::S1))
            .bg(Theme::color(p.paper))
            .border_t_1()
            .border_color(Theme::color(p.line))
            .children(rows)
            .into_any_element();
        // The window's own status line, or its command line while `:` is typed
        // in it.
        let bar = match win.vim.command_line() {
            Some(line) => self.typed_line(line).into_any_element(),
            None => StatusLine::new(win.vim.mode())
                .file("[Command Line]")
                .item("⏎ run · esc close")
                .into_any_element(),
        };
        vec![panel, bar]
    }
}

impl Editor {
    /// The panes as laid out, with a rule between parts.
    pub(super) fn render_layout(
        &self,
        layout: &Layout,
        view: &gpui::Entity<Editor>,
        p: &Palette,
    ) -> AnyElement {
        match layout {
            Layout::Pane(id) => self.render_pane(*id, view, p),
            Layout::Split(axis, parts) => {
                let row = *axis == Axis::Row;
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .when(!row, |d| d.flex_col())
                    .children(parts.iter().enumerate().map(|(i, part)| {
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .when(i > 0 && row, |d| d.border_l_1())
                            .when(i > 0 && !row, |d| d.border_t_1())
                            .border_color(Theme::color(p.line))
                            .child(self.render_layout(part, view, p))
                    }))
                    .into_any_element()
            }
        }
    }

    /// One pane: its note's visible lines. With several panes, the focused
    /// one has a `cursor` rule along its top and the others are quieter.
    pub(super) fn render_pane(
        &self,
        id: PaneId,
        view: &gpui::Entity<Editor>,
        p: &Palette,
    ) -> AnyElement {
        let focused = id == self.focused;
        let several = self.panes.len() > 1;
        let lines = if focused {
            Some(self.lines.clone())
        } else {
            self.panes
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.local.as_ref())
                .map(|l| l.lines.clone())
        };
        let Some(lines) = lines else {
            return div().flex_1().into_any_element();
        };
        let view = view.clone();
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .justify_center()
            .pt(px(space::S8))
            .when(several, |d| {
                d.px(px(space::S3)).border_t_2().border_color(if focused {
                    Theme::color(p.cursor)
                } else {
                    gpui::transparent_black()
                })
            })
            .when(several && !focused, |d| d.opacity(0.72))
            .child(
                list(lines, move |ix, _window, cx: &mut App| {
                    view.update(cx, |this, cx| {
                        if this.focused == id {
                            this.render_line(ix, cx)
                        } else {
                            this.render_other(id, ix, cx)
                        }
                    })
                })
                .size_full()
                .max_w(px(size::GUTTER_W + size::MEASURE + space::S8)),
            )
            .into_any_element()
    }
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The frame has started drawing: the key's wait for it ends here.
        self.latency.frame_started();
        if let Some(first_run) = self.render_first_run(cx) {
            return div()
                .size_full()
                .track_focus(&self.focus)
                .on_key_down(cx.listener(Self::on_key))
                .child(first_run)
                .into_any_element();
        }
        // The tree it replaces is freed off this thread (slow on big notes).
        let tree = self.buffer.syntax_tree().cloned();
        mnote_core::drop_tree_later(std::mem::replace(&mut self.tree, tree));
        self.refresh_parked_trees();
        // Events from this change go out just after the frame is drawn.
        if self.events.needs_flush() {
            cx.on_next_frame(window, |this, _, cx| this.flush_events(cx));
        }
        // A page for Page Up/Down and ⌃f ⌃b: the lines that fit the window.
        let text_h = window.viewport_size().height / px(1.) - 4. * crate::theme::bar_h();
        let page = (text_h / (text::NOTE_BODY.line_height().max(px(1.)) / px(1.))).floor();
        self.vim.set_page_lines(page.max(1.) as usize);
        let p = cx.theme().palette;
        let width = window.viewport_size().width / px(1.);
        let height = window.viewport_size().height / px(1.);
        let caret_on = self.caret_on;
        let mouse = {
            let rows = cx.entity().downgrade();
            let scrim = rows.clone();
            crate::picker::PickerMouse {
                on_row: Rc::new(move |ix, clicks, _window, cx| {
                    let _ = rows.update(cx, |this, cx| this.picker_click(ix, clicks, cx));
                }),
                on_scrim: Rc::new(move |_window, cx| {
                    let _ = scrim.update(cx, |this, cx| {
                        this.picker = None;
                        this.restart_blink(cx);
                        cx.notify();
                    });
                }),
            }
        };
        let picker = self
            .picker
            .as_mut()
            .map(|picker| picker.render(width, height, caret_on, &mouse, p));
        let folder_edit = self
            .folder_edit
            .as_ref()
            .map(|editor| editor.render(width, height, caret_on, p));
        let rope = self.buffer.text();
        let cursor = self.vim.cursor();
        let line = line_of(rope, cursor);
        let column = cursor - line_start(rope, line);
        let file = match &self.source {
            Source::Scratch => SharedString::new_static("scratch (not saved)"),
            Source::Config(_) => SharedString::new_static("config.toml"),
            Source::Note { note, .. } => note.file_name().to_owned().into(),
        };
        let view = cx.entity();

        let mut status = StatusLine::new(self.vim.mode()).file(file);
        if self.view != styled::View::Styled {
            status = status.item(format!("{} view", self.view.name()));
        }
        if let Some(notice) = &self.notice {
            status = status.item(notice.clone());
        }
        for file in self.other_pane_files() {
            status = status.item(format!("│ {file}"));
        }
        let words = self.words.count_for(&self.source.doc_key());
        let status = status
            .when_some(words, |status, words| {
                status.right(if words == 1 {
                    "1 word".to_owned()
                } else {
                    format!("{words} words")
                })
            })
            .right(format!("{} lines", line_count(rope)))
            .right(format!("{}:{}", line + 1, column + 1));

        div()
            .flex()
            .flex_col()
            .relative()
            .size_full()
            .bg(Theme::color(p.paper))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, _| this.drag_anchor = None),
            )
            .child(
                TopBar::new(self.path_segments())
                    .dirty(self.buffer.is_dirty())
                    .leading_inset(px(TRAFFIC_LIGHT_INSET))
                    .when(self.panes.len() > 1, |bar| {
                        bar.hint("⌃w h j k l move · ⌃w c close")
                    })
                    .when(self.vim.command_line().is_some(), |bar| {
                        bar.hint("⇥ complete · ↑ history · ⌃f edit")
                    })
                    .when_some(
                        crate::keymap::current().shown_for("view").filter(|_| {
                            self.vim.command_line().is_none() && self.view != styled::View::Styled
                        }),
                        |bar, keys| bar.hint(format!("{keys} view: {}", self.view.next().name())),
                    ),
            )
            .child({
                // Registers the editor with the system's text input each
                // frame (accents, IME); see `ime`.
                let focus = self.focus.clone();
                let input = view.clone();
                gpui::canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &focus,
                            gpui::ElementInputHandler::new(bounds, input),
                            cx,
                        );
                    },
                )
                .absolute()
                .size_full()
            })
            .child(self.render_layout(&self.layout.clone(), &view, p))
            // The command line takes the status line's place while it's
            // typed, and while it shows the result.
            .map(|editor| {
                if let Some(win) = self.vim.cmd_window() {
                    return editor.children(self.cmd_window_area(win, p));
                }
                let command = self.command_area();
                if command.is_empty() {
                    editor.child(status)
                } else {
                    editor.children(command)
                }
            })
            .children(self.completion_popup().map(|popup| {
                div()
                    .absolute()
                    .left(px(space::S3))
                    .bottom(px(crate::theme::bar_h() + space::S1))
                    .child(popup)
            }))
            .children(picker)
            .children(folder_edit)
            .children(self.toast.clone().map(|(text, undo)| {
                let toast = Toast::new(text);
                let toast = if undo {
                    toast.action("u", "undo")
                } else {
                    toast
                };
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(px(48.))
                    .flex()
                    .justify_center()
                    .child(toast)
            }))
            .children(self.latency.frame_probe())
            .into_any_element()
    }
}
