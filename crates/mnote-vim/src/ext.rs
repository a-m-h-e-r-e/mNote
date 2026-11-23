//! Extensions: motions, text objects, operators and ex commands added at
//! run time (by the app, and later by plugins), each with one call. They're
//! checked before the built-ins, so an extension can also replace one.
//!
//! Like the app's ex commands ([`crate::ex::set_app_commands`]), they're
//! kept per thread and shared by every [`Vim`] on it: the editor, the
//! folder editor and the command-line window all see the same ones.
//!
//! What extensions do counts like the built-ins: an operator's edits are
//! one undo step and `.` repeats it, a motion works after any operator and
//! in VISUAL, and a text object after `i` / `a`.

use std::cell::RefCell;
use std::ops::{Range, RangeInclusive};
use std::rc::Rc;

use mnote_core::Buffer;
use ropey::Rope;

use crate::Vim;
use crate::ex::{CommandSpec, ExError};
use crate::motion::Target;
use crate::textobj::Selection;

/// Where a motion goes from `pos`, `count` times (at least 1). `None` when
/// it can't move, as `f` with no match.
pub type MotionFn = Rc<dyn Fn(&Rope, usize, usize) -> Option<Target>>;

/// The text object at `pos`: `around` for `a…`, `count` for `2i(`.
pub type TextObjectFn = Rc<dyn Fn(&Rope, usize, bool, usize) -> Option<Selection>>;

/// An operator on `range` (whole lines when `linewise`). It edits through
/// [`Vim::replace`] or [`Vim::edit_text`]; the whole run is one undo step.
pub type OperatorFn = Rc<dyn Fn(&mut Vim, &mut Buffer, Range<usize>, bool)>;

/// An ex command: what to show (a message) or an error.
pub type ExFn = Rc<dyn Fn(&mut Vim, &mut Buffer, &ExCall) -> Result<Option<String>, ExError>>;

/// How an extension ex command was called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExCall {
    /// The lines it covers (0-based): its range, or the cursor line.
    pub lines: RangeInclusive<usize>,
    /// What followed the name and `!`, trimmed.
    pub args: String,
    pub bang: bool,
}

/// Where a key for a motion or operator sits: on its own (`x`) or after
/// `g` (`gx`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keys {
    Plain(char),
    G(char),
}

impl Keys {
    /// `"x"` or `"gx"`.
    pub fn parse(keys: &str) -> Option<Keys> {
        let mut chars = keys.chars();
        match (chars.next(), chars.next(), chars.next()) {
            (Some(c), None, None) if !c.is_ascii_digit() => Some(Keys::Plain(c)),
            (Some('g'), Some(c), None) => Some(Keys::G(c)),
            _ => None,
        }
    }

    /// The key that, typed after the operator, applies it to whole lines
    /// (`gcc`: `c`).
    fn last(self) -> char {
        match self {
            Keys::Plain(c) | Keys::G(c) => c,
        }
    }
}

struct MotionExt {
    keys: Keys,
    jump: bool,
    run: MotionFn,
}

struct OperatorExt {
    name: String,
    keys: Option<Keys>,
    run: OperatorFn,
}

pub(crate) struct ExExt {
    pub spec: CommandSpec,
    pub name: &'static str,
    pub run: ExFn,
}

#[derive(Default)]
struct Registry {
    motions: Vec<MotionExt>,
    objects: Vec<(char, TextObjectFn)>,
    operators: Vec<OperatorExt>,
    commands: Vec<Rc<ExExt>>,
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

/// Adds a motion on `keys` (`"x"` or `"gx"`); `jump` makes it a jump (`''`
/// comes back). Returns `false` for keys it can't take.
pub fn register_motion(keys: &str, jump: bool, run: MotionFn) -> bool {
    let Some(keys) = Keys::parse(keys) else {
        return false;
    };
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        r.motions.retain(|m| m.keys != keys);
        r.motions.push(MotionExt { keys, jump, run });
    });
    true
}

/// Adds a text object, used after `i` and `a` (`dix`, `vax`).
pub fn register_text_object(key: char, run: TextObjectFn) {
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        r.objects.retain(|(k, _)| *k != key);
        r.objects.push((key, run));
    });
}

/// Adds an operator called `name`, on `keys` if given (`"gc"`: then `gcc`
/// does whole lines). Any operator can also be `g@`'s, with
/// `:set operatorfunc=name`.
pub fn register_operator(name: &str, keys: Option<&str>, run: OperatorFn) -> bool {
    let keys = match keys {
        Some(k) => match Keys::parse(k) {
            Some(k) => Some(k),
            None => return false,
        },
        None => None,
    };
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        r.operators
            .retain(|o| o.name != name && (keys.is_none() || o.keys != keys));
        r.operators.push(OperatorExt {
            name: name.to_owned(),
            keys,
            run,
        });
    });
    true
}

/// Adds an ex command that runs here, in Vim (`:Name args`), with its
/// abbreviation, completion and description from `spec`.
pub fn register_ex(spec: CommandSpec, run: ExFn) {
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        let name = r
            .commands
            .iter()
            .find(|c| c.name == spec.name)
            .map(|c| c.name)
            .unwrap_or_else(|| Box::leak(spec.name.clone().into_boxed_str()));
        r.commands.retain(|c| c.name != name);
        r.commands.push(Rc::new(ExExt { spec, name, run }));
    });
}

/// Forgets every extension (for tests, and before plugins reload).
pub fn clear() {
    REGISTRY.with(|r| *r.borrow_mut() = Registry::default());
}

// ---- lookups, for the engine ------------------------------------------------

/// The motion on `keys`, by id.
pub(crate) fn motion_for(keys: Keys) -> Option<u16> {
    REGISTRY.with(|r| {
        r.borrow()
            .motions
            .iter()
            .position(|m| m.keys == keys)
            .map(|i| i as u16)
    })
}

pub(crate) fn motion(id: u16) -> Option<(MotionFn, bool)> {
    REGISTRY.with(|r| {
        r.borrow()
            .motions
            .get(id as usize)
            .map(|m| (m.run.clone(), m.jump))
    })
}

pub(crate) fn text_object(key: char) -> Option<TextObjectFn> {
    REGISTRY.with(|r| {
        r.borrow()
            .objects
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, f)| f.clone())
    })
}

/// The operator on `keys`, by id.
pub(crate) fn operator_for(keys: Keys) -> Option<u16> {
    REGISTRY.with(|r| {
        r.borrow()
            .operators
            .iter()
            .position(|o| o.keys == Some(keys))
            .map(|i| i as u16)
    })
}

/// The operator called `name`, by id.
pub(crate) fn operator_named(name: &str) -> Option<u16> {
    REGISTRY.with(|r| {
        r.borrow()
            .operators
            .iter()
            .position(|o| o.name == name)
            .map(|i| i as u16)
    })
}

pub(crate) fn operator(id: u16) -> Option<OperatorFn> {
    REGISTRY.with(|r| r.borrow().operators.get(id as usize).map(|o| o.run.clone()))
}

/// The key that doubles operator `id` onto whole lines (`gcc`); `@` for
/// one used only through `g@`.
pub(crate) fn operator_line_key(id: u16) -> char {
    REGISTRY.with(|r| {
        r.borrow()
            .operators
            .get(id as usize)
            .and_then(|o| o.keys)
            .map_or('@', Keys::last)
    })
}

/// The extension ex command whose full name starts with `word` (abbreviated
/// to at least its `abbrev`).
pub(crate) fn command_lookup(word: &str) -> Option<&'static str> {
    REGISTRY.with(|r| {
        r.borrow()
            .commands
            .iter()
            .find(|c| word.len() >= c.spec.abbrev.max(1) && c.name.starts_with(word))
            .map(|c| c.name)
    })
}

pub(crate) fn command(name: &str) -> Option<Rc<ExExt>> {
    REGISTRY.with(|r| r.borrow().commands.iter().find(|c| c.name == name).cloned())
}

/// Every extension ex command, for completion.
pub(crate) fn commands() -> Vec<Rc<ExExt>> {
    REGISTRY.with(|r| r.borrow().commands.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::check;
    use crate::motion::MotionKind;
    use crate::text::{line_end, line_of, line_start};

    /// `M`: the middle of the line (exclusive); `gl`: its last character.
    fn motions() {
        clear();
        register_motion(
            "M",
            false,
            Rc::new(|text, pos, _| {
                let line = line_of(text, pos);
                let (start, end) = (line_start(text, line), line_end(text, line));
                Some(Target {
                    pos: start + (end - start) / 2,
                    kind: MotionKind::Exclusive,
                    keep_column: false,
                })
            }),
        );
        register_motion(
            "gl",
            true,
            Rc::new(|text, pos, _| {
                let line = line_of(text, pos);
                Some(Target {
                    pos: line_end(text, line).saturating_sub(1),
                    kind: MotionKind::Inclusive,
                    keep_column: false,
                })
            }),
        );
    }

    /// `gc`: comments lines in and out with `# `.
    fn comment_operator() {
        register_operator(
            "comment",
            Some("gc"),
            Rc::new(|vim, buf, range, _| {
                let text = buf.text();
                let (first, last) = (
                    line_of(text, range.start),
                    line_of(text, range.end.saturating_sub(1).max(range.start)),
                );
                for line in (first..=last).rev() {
                    let start = line_start(buf.text(), line);
                    if buf.text().slice(start..).to_string().starts_with("# ") {
                        vim.edit_text(buf, start..start + 2, "");
                    } else {
                        vim.edit_text(buf, start..start, "# ");
                    }
                }
            }),
        );
    }

    #[test]
    fn motions_work_alone_after_operators_and_in_visual() {
        motions();
        check("|abcdef\n", "M", "abc|def\n");
        check("|abcdef\n", "dM", "|def\n");
        check("|abcdef\n", "vMd", "|ef\n");
        check("|abcdef\n", "gl", "abcde|f\n");
        check("|abcdef\n", "dgl", "|\n");
        check("|abcdef\n", "dglu", "|abcdef\n");
        let (_, vim, _) = crate::harness::run("|abcdef\n", "gl");
        assert_eq!(vim.mark('\''), Some(0), "gl is a jump");
        assert!(!register_motion("3", false, Rc::new(|_, _, _| None)));
        assert!(!register_motion("gxy", false, Rc::new(|_, _, _| None)));
    }

    #[test]
    fn text_objects_work_after_i_and_a() {
        clear();
        // `il` / `al`: the line without / with its indent.
        register_text_object(
            'l',
            Rc::new(|text, pos, around, _| {
                let line = line_of(text, pos);
                let (start, end) = (line_start(text, line), line_end(text, line));
                let indent = text
                    .slice(start..end)
                    .chars()
                    .take_while(|c| *c == ' ')
                    .count();
                Some(Selection {
                    range: if around {
                        start..end
                    } else {
                        start + indent..end
                    },
                    linewise: false,
                })
            }),
        );
        check("  a|b\n", "dil", " | \n");
        check("  a|b\n", "dal", "|\n");
        check("  a|b\n", "cilx<esc>", "  |x\n");
        check("  a|b\n", "vily", "  |ab\n");
        // A built-in object still works.
        check("a(b|c)\n", "di(", "a(|)\n");
    }

    #[test]
    fn operators_repeat_undo_and_double_onto_lines() {
        clear();
        comment_operator();
        check("|a\nb\nc\n", "gcc", "|# a\nb\nc\n");
        check("|a\nb\nc\n", "gcj", "|# a\n# b\nc\n");
        check("|a\nb\nc\n", "gccj.", "# a\n|# b\nc\n");
        check("|a\nb\nc\n", "gcjgcju", "|# a\n# b\nc\n");
        check("|a\nb\nc\n", "Vjgc", "|# a\n# b\nc\n");
        check("|# a\nb\n", "gcc", "|a\nb\n");
        // `g@` runs the operatorfunc.
        check("|a\nb\n", ":set opfunc=comment<CR>g@j", "|# a\n# b\n");
        check("|a\n", "g@j", "|a\n");
        let (_, vim, _) = crate::harness::run("|a\n", "g@");
        assert_eq!(
            vim.message().map(|m| m.text.as_str()),
            Some("E774: 'operatorfunc' is empty")
        );
        let (_, mut vim, mut buf) = crate::harness::run("|a\n", "");
        assert!(vim.execute("set opfunc=nope", &mut buf).is_err());
    }

    #[test]
    fn ex_commands_run_on_their_lines() {
        clear();
        register_ex(
            CommandSpec {
                name: "Upcase".into(),
                abbrev: 2,
                detail: "upper-case lines".into(),
                args: crate::ex::ArgKind::None,
            },
            Rc::new(|vim, buf, call| {
                let text = buf.text();
                let start = line_start(text, *call.lines.start());
                let end = line_end(text, *call.lines.end());
                let upper = text.slice(start..end).to_string().to_uppercase();
                vim.edit_text(buf, start..end, &upper);
                Ok(Some(format!("{} lines", call.lines.clone().count())))
            }),
        );
        check("|a\nb\nc\n", ":Up<CR>", "|A\nb\nc\n");
        check("|a\nb\nc\n", ":%Upcase<CR>", "|A\nB\nC\n");
        check("|a\nb\nc\n", ":%Up<CR>u", "|a\nb\nc\n");
        let (_, mut vim, mut buf) = crate::harness::run("|a\nb\n", "");
        let out = vim.execute("2Up", &mut buf).expect("runs");
        assert_eq!(out.message.as_deref(), Some("1 lines"));
        assert!(vim.execute("Up now", &mut buf).is_err(), "no arguments");
        // Completion knows it.
        let names = crate::ex::complete::candidates("Up", 2, &Default::default(), Vec::new)
            .expect("completes")
            .1;
        assert_eq!(names[0].text, "Upcase");
        // Mapped, `.` repeats it on the next line.
        let (_, mut vim, mut buf) = crate::harness::run("|a\nb\n", "");
        vim.execute("nnoremap Q <Cmd>Upcase<CR>", &mut buf)
            .expect("maps");
        for key in crate::parse_keys("Qj.") {
            vim.handle_key(key, &mut buf);
        }
        assert_eq!(buf.to_string(), "A\nB\n");
        // An extension can make itself repeatable too.
        vim.repeat_with("1Upcase");
        let mut buf = mnote_core::Buffer::new("x\ny\n");
        vim.set_cursor(0);
        vim.handle_key(crate::Key::Char('.'), &mut buf);
        assert_eq!(buf.to_string(), "X\ny\n");
    }

    #[test]
    fn dot_repeats_a_change_whose_text_has_a_v() {
        clear();
        check("|foo bar\n", "cwvar<esc>w.", "var va|r\n");
        check("|foo\nbar\n", "Avv<esc>j.", "foovv\nbarv|v\n");
        // A change made in VISUAL still isn't repeated.
        check("|abc\n", "vlxl.", "|c\n");
    }
}
