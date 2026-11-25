//! Highlight scopes: capture names (`keyword.function`) as small numbers,
//! so spans are cheap to make and themes resolve a name once (ADR-0004).
//! The table is shared by every thread and only grows: there are a few
//! hundred capture names in all of nvim-treesitter.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

/// A capture name, interned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Scope(pub u32);

#[derive(Default)]
struct Table {
    names: Vec<Arc<str>>,
    ids: HashMap<Arc<str>, u32>,
}

fn table() -> &'static RwLock<Table> {
    static TABLE: OnceLock<RwLock<Table>> = OnceLock::new();
    TABLE.get_or_init(RwLock::default)
}

/// The scope for a capture name, with any leading `@` dropped.
pub fn intern(name: &str) -> Scope {
    let name = name.trim_start_matches('@');
    if let Ok(t) = table().read()
        && let Some(&id) = t.ids.get(name)
    {
        return Scope(id);
    }
    let mut t = match table().write() {
        Ok(t) => t,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(&id) = t.ids.get(name) {
        return Scope(id);
    }
    let id = t.names.len() as u32;
    let shared: Arc<str> = Arc::from(name);
    t.names.push(shared.clone());
    t.ids.insert(shared, id);
    Scope(id)
}

/// A scope's capture name (`keyword.function`).
pub fn name(scope: Scope) -> Arc<str> {
    table()
        .read()
        .ok()
        .and_then(|t| t.names.get(scope.0 as usize).cloned())
        .unwrap_or_else(|| Arc::from(""))
}

/// How many scopes there are (themes size their caches by it).
pub fn count() -> usize {
    table().read().map(|t| t.names.len()).unwrap_or(0)
}

/// A scope's name, then each shorter one: `keyword.function`, `keyword`.
/// Themes look a scope up in this order.
pub fn fallbacks(name: &str) -> impl Iterator<Item = &str> {
    let name = name.trim_start_matches('@');
    std::iter::successors(Some(name), |n| n.rfind('.').map(|i| &n[..i]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_are_stable_and_fall_back() {
        let a = intern("@keyword.function");
        assert_eq!(a, intern("keyword.function"));
        assert_ne!(a, intern("keyword"));
        assert_eq!(&*name(a), "keyword.function");
        assert!(count() >= 2);
        let chain: Vec<&str> = fallbacks("@markup.heading.1.marker").collect();
        assert_eq!(
            chain,
            [
                "markup.heading.1.marker",
                "markup.heading.1",
                "markup.heading",
                "markup"
            ]
        );
    }
}
