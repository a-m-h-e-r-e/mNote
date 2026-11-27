//! The context API in the app (ADR-0011, AMI-241): the input the
//! providers read, taken from the focused note, and requests run on a
//! background thread, a newer one cancelling an older one. `:context`
//! shows what a request gathers at the cursor; plugins ask with
//! `mnote.context.get` and add providers with `mnote.context.register`.

use gpui::Context;
use mnote_core::context::{Answer, Edit, Input, Item, Limit, Other, Request, gather};
use mnote_core::text::Bias;

use super::Editor;

/// How many lines of another document a provider gives.
const OTHER_LINES: usize = 40;

impl Editor {
    /// What the providers read, at the focused pane's cursor.
    pub(super) fn context_input(&mut self) -> Input {
        let text = self.buffer.text().clone();
        let document = self
            .source
            .key()
            .map_or_else(|| self.source.doc_key().label(), |n| n.to_string());
        let cursor = self.vim.cursor();
        let selection = self.vim.mode().is_visual().then(|| {
            let anchor = self.vim.anchor();
            anchor.min(cursor)..(anchor.max(cursor) + 1).min(text.len_chars())
        });
        // The user's recent edits, where they are now, newest first.
        let edits: Vec<Edit> = self
            .buffer
            .recent_user_edits()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .filter_map(|c| {
                // Where the edit's new text begins, on the text as it is now.
                let start = self
                    .buffer
                    .rebase(c.range.start, c.revision, Bias::Before)?
                    .0;
                Some(Edit {
                    range: start..start + c.new_chars,
                    by_user: true,
                })
            })
            .collect();
        let (links, backlinks) = self.note_links();
        let open = self
            .parked
            .iter()
            .filter(|d| d.source.doc_key() != self.source.doc_key())
            .map(|d| Other {
                document: d
                    .source
                    .key()
                    .map_or_else(|| d.source.doc_key().label(), |n| n.to_string()),
                text: first_lines(d.buffer.text(), OTHER_LINES),
            })
            .collect();
        Input {
            document,
            revision: self.buffer.revision(),
            position: cursor,
            syntax: self.buffer.syntax_snapshot(),
            text,
            selection,
            edits,
            links,
            backlinks,
            open,
        }
    }

    /// The notes the focused note links to (their title and headings) and
    /// the lines linking to it, from the index.
    fn note_links(&self) -> (Vec<Other>, Vec<Other>) {
        let Some(note) = self.source.key() else {
            return (Vec::new(), Vec::new());
        };
        let index = self.index.borrow();
        let notes = index.notes();
        let Some(id) = notes.iter().position(|n| n.path == *note) else {
            return (Vec::new(), Vec::new());
        };
        let mut seen = Vec::new();
        let links = notes[id]
            .links
            .iter()
            .filter_map(|l| index.resolve(&l.key))
            .filter(|&to| {
                to != id && !seen.contains(&to) && {
                    seen.push(to);
                    true
                }
            })
            .map(|to| {
                let n = &notes[to];
                let mut text = format!("# {}\n", n.title);
                for h in &n.headings {
                    text.push_str(&format!("{} {}\n", "#".repeat(h.level as usize), h.text));
                }
                Other {
                    document: n.path_text.clone(),
                    text,
                }
            })
            .collect();
        let backlinks = index
            .backlinks(id)
            .into_iter()
            .map(|b| Other {
                document: notes[b.from].path_text.clone(),
                text: b.context,
            })
            .collect();
        (links, backlinks)
    }

    /// Gathers `request` on a background thread with `extra` items (a
    /// plugin's), then calls `done` with the answer, unless a newer request
    /// came meanwhile.
    pub(super) fn request_context(
        &mut self,
        request: Request,
        extra: Vec<Item>,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Answer, &mut Context<Self>) + 'static,
    ) {
        let input = self.context_input();
        let cancel = self.context_cancel.clone();
        let ticket = cancel.ticket();
        cx.spawn(async move |this, cx| {
            let answer = cx
                .background_executor()
                .spawn(async move { gather(&request, &input, extra, Some((&cancel, ticket))) })
                .await;
            if let Some(answer) = answer {
                let _ = this.update(cx, |this, cx| done(this, answer, cx));
            }
        })
        .detach();
    }

    /// `:context [provider …]`: what a request gathers at the cursor, as a
    /// reply (each item's kind, document and size).
    pub(super) fn show_context(&mut self, args: Option<&str>, cx: &mut Context<Self>) {
        let providers: Vec<String> = args
            .unwrap_or("")
            .split([' ', ','])
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect();
        if let Some(bad) = providers
            .iter()
            .find(|p| !mnote_core::context::PROVIDERS.contains(&p.as_str()))
        {
            self.reply = Some((
                format!(
                    "No context provider {bad} (they are {})",
                    mnote_core::context::PROVIDERS.join(", ")
                )
                .into(),
                true,
            ));
            cx.notify();
            return;
        }
        let request = Request {
            providers,
            limit: Limit::Tokens(8000),
        };
        self.request_context(request, Vec::new(), cx, |this, answer, cx| {
            let total: usize = answer.items.iter().map(|i| i.text.chars().count()).sum();
            let parts: Vec<String> = answer
                .items
                .iter()
                .map(|i| format!("{} {} ({})", i.kind, i.document, i.text.chars().count()))
                .collect();
            this.reply = Some((
                format!(
                    "Context: {} items, {total} characters: {}",
                    answer.items.len(),
                    if parts.is_empty() {
                        "none".to_owned()
                    } else {
                        parts.join(" · ")
                    }
                )
                .into(),
                false,
            ));
            cx.notify();
        });
    }
}

fn first_lines(text: &mnote_core::Rope, n: usize) -> String {
    let end = text.line_to_char(n.min(text.len_lines()));
    text.slice(..end).to_string()
}
