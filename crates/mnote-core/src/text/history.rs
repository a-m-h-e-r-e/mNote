//! The undo tree.
//!
//! Every committed change is a node; undo walks to the parent, redo to the
//! most recent child. Undoing and then editing starts a new branch instead of
//! throwing the undone work away, as in Vim.

/// One replacement, in character offsets on the text as it was when the
/// replacement was applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Replace {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// A group of replacements applied in order, and the ones that reverse them.
#[derive(Debug, Clone, Default)]
pub(crate) struct Step {
    pub forward: Vec<Replace>,
    pub inverse: Vec<Replace>,
    pub cursor_before: usize,
    pub cursor_after: usize,
}

#[derive(Debug)]
struct Node {
    parent: usize,
    step: Step,
    /// The most recently created or redone child, which redo follows.
    last_child: Option<usize>,
}

#[derive(Debug)]
pub(crate) struct History {
    nodes: Vec<Node>,
    current: usize,
    saved: Option<usize>,
}

const ROOT: usize = 0;

impl History {
    pub fn new() -> Self {
        History {
            nodes: vec![Node {
                parent: ROOT,
                step: Step::default(),
                last_child: None,
            }],
            current: ROOT,
            saved: Some(ROOT),
        }
    }

    /// Adds a finished step as a child of the current state.
    pub fn commit(&mut self, step: Step) {
        let id = self.nodes.len();
        self.nodes.push(Node {
            parent: self.current,
            step,
            last_child: None,
        });
        self.nodes[self.current].last_child = Some(id);
        self.current = id;
    }

    /// The step to reverse for undo, moving to the parent. `None` at the root.
    pub fn undo(&mut self) -> Option<&Step> {
        if self.current == ROOT {
            return None;
        }
        let node = self.current;
        self.current = self.nodes[node].parent;
        self.nodes[self.current].last_child = Some(node);
        Some(&self.nodes[node].step)
    }

    /// The step to re-apply for redo, moving to the newest child.
    pub fn redo(&mut self) -> Option<&Step> {
        let child = self.nodes[self.current].last_child?;
        self.current = child;
        Some(&self.nodes[child].step)
    }

    pub fn mark_saved(&mut self) {
        self.saved = Some(self.current);
    }

    pub fn is_saved(&self) -> bool {
        self.saved == Some(self.current)
    }

    /// Number of committed steps, branches included.
    pub fn len(&self) -> usize {
        self.nodes.len() - 1
    }
}
