//! Grammars loaded at run time (ADR-0002): when a code fence is in a
//! language whose grammar isn't built in, but a parser library for it is in
//! mNote's grammar folder or Neovim's, mNote asks (once per library, through
//! the permission store), loads it off the UI thread, and colours the fence.

use std::path::{Path, PathBuf};

use mnote_core::lang::grammar;
use mnote_core::permissions::{Kind, Permissions};

use super::*;

/// Whether `grammar`'s library at `path` may load, and its fingerprint (for
/// recording the user's answer).
pub(super) fn grammar_allowed(
    permissions: &Permissions,
    grammar: &str,
    path: &Path,
) -> (bool, u64) {
    let print = mnote_core::permissions::fingerprint_files(&[path]);
    (permissions.allowed(Kind::Grammar, grammar, print), print)
}

impl Editor {
    /// A fence in `lang` has no grammar: asked about once the frame is done.
    pub(super) fn want_grammar(&mut self, lang: &str, cx: &mut Context<Self>) {
        if self.grammars_seen.contains(lang) || self.grammars_wanted.iter().any(|l| l == lang) {
            return;
        }
        self.grammars_wanted.push(lang.to_owned());
        let view = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = view.update(cx, |this, cx| this.request_grammars(cx));
        });
    }

    /// Loads (or asks about) the grammars fences wanted. One question at a
    /// time; a language whose grammar can't be found isn't looked for again
    /// this session.
    fn request_grammars(&mut self, cx: &mut Context<Self>) {
        while let Some(lang) = self.grammars_wanted.pop() {
            if self.grammars_seen.contains(&lang) {
                continue;
            }
            let Some((name, path)) = grammar::available(&lang) else {
                self.grammars_seen.insert(lang);
                continue;
            };
            let (allowed, print) = grammar_allowed(&self.permissions, &name, &path);
            if allowed {
                self.grammars_seen.insert(lang);
                self.start_grammar_load(name, path, cx);
                continue;
            }
            if self.picker.is_some() {
                // Asked when the fence draws again with no picker open.
                return;
            }
            self.grammars_seen.insert(lang);
            let title = format!("Load the {name} grammar?");
            let what = format!(
                "the tree-sitter parser {}: it runs inside mNote",
                path.display()
            );
            let answer: plugin_list::PermissionAnswer = Rc::new(move |e, scope, cx| {
                if let Some(scope) = scope {
                    e.permissions.allow(Kind::Grammar, &name, print, scope);
                    e.start_grammar_load(name.clone(), path.clone(), cx);
                }
            });
            self.ask_permission(title, what, answer, cx);
            return;
        }
    }

    /// Opens `path`'s grammar off the UI thread; its fences colour when it's
    /// in.
    fn start_grammar_load(&mut self, name: String, path: PathBuf, cx: &mut Context<Self>) {
        let folders = grammar::current_folders();
        let job = {
            let name = name.clone();
            move || grammar::load(&grammar::native::NativeEngine, &name, &path, &folders)
        };
        let task = cx.background_executor().spawn(async move { job() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => {
                        // Fences coloured plain before re-colour now.
                        this.code_cache.clear();
                        for doc in &mut this.parked {
                            doc.code_cache.clear();
                        }
                    }
                    Err(err) => this.show_toast(
                        format!("Couldn't load the {name} grammar: {err}"),
                        false,
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnote_core::permissions::Scope;

    #[test]
    fn a_grammar_loads_only_once_allowed_and_again_after_it_changes() {
        let dir = std::env::temp_dir().join(format!("mnote-grammar-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let lib = dir.join("go.so");
        std::fs::write(&lib, b"one").expect("write");
        let mut permissions = Permissions::in_memory();
        let (allowed, print) = grammar_allowed(&permissions, "go", &lib);
        assert!(!allowed, "not trusted yet");
        permissions.allow(Kind::Grammar, "go", print, Scope::Session);
        assert!(grammar_allowed(&permissions, "go", &lib).0);
        assert!(
            !grammar_allowed(&permissions, "lua", &lib).0,
            "another grammar"
        );
        std::fs::write(&lib, b"two").expect("write");
        assert!(
            !grammar_allowed(&permissions, "go", &lib).0,
            "the library changed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
