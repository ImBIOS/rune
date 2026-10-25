//! Key checks run before virtualization, so offscreen siblings are checked too.
use std::collections::{HashMap, HashSet};

use crate::El;

/// A sibling key used twice under one parent.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Duplicate {
    pub key: String,
    pub message: String,
}

fn find(root: &El, path: &str) -> Vec<Duplicate> {
    fn walk(el: &El, path: &str, out: &mut Vec<Duplicate>) {
        if el.all_children().filter(|c| c.key.is_some()).nth(1).is_some() {
            let mut seen: HashMap<&str, &El> = HashMap::new();
            for child in el.all_children() {
                let Some(key) = &child.key else { continue };
                if let Some(first) = seen.get(key.as_str()) {
                    out.push(Duplicate {
                        key: key.clone(),
                        message: format!(
                            "duplicate sibling key {key:?} under {path:?}: first at {}, again at {}; give siblings unique keys",
                            first.key_location(), child.key_location(),
                        ),
                    });
                } else {
                    seen.insert(key, child);
                }
            }
        }
        for (index, child) in el.all_children().enumerate() {
            if child.all_children().next().is_none() { continue; }
            let segment = child.key.clone().unwrap_or_else(|| format!("#{index}"));
            walk(child, &format!("{path}/{segment}"), out);
        }
    }
    let mut out = Vec::new();
    walk(root, path, &mut out);
    out
}

fn root_path(root: &El) -> &str {
    root.key.as_deref().unwrap_or("#0")
}

#[cfg(test)]
pub(crate) fn warnings(root: &El) -> Vec<String> {
    find(root, root_path(root)).into_iter().map(|d| d.message).collect()
}

/// Makes sibling keys unique so the layout tree never sees two siblings share a node. The first
/// sibling keeps its key; each later one is renamed `key#dup1`, `key#dup2`, ... in tree order, so
/// it still draws and keeps a stable identity across builds. Returns what it renamed.
pub(crate) fn repair(root: &mut El) -> Vec<Duplicate> {
    let path = root_path(root).to_owned();
    repair_at(root, &path)
}

/// [`repair`] for a subtree whose root sits at `path`.
pub(crate) fn repair_at(root: &mut El, path: &str) -> Vec<Duplicate> {
    let found = find(root, path);
    if found.is_empty() { return found; }
    fn walk(el: &mut El) {
        el.expand_memo();
        let mut seen: HashSet<String> = el.children.iter().filter_map(|c| c.key.clone()).collect();
        let mut kept: HashSet<String> = HashSet::new();
        for child in &mut el.children {
            let Some(key) = child.key.clone() else { continue };
            if kept.insert(key.clone()) { continue; }
            let renamed = (1..).map(|n| format!("{key}#dup{n}")).find(|k| !seen.contains(k)).unwrap();
            seen.insert(renamed.clone());
            kept.insert(renamed.clone());
            child.key = Some(renamed);
        }
        for child in &mut el.children { walk(child); }
    }
    walk(root);
    found
}

/// Prints each duplicate once per key for the life of the process.
pub(crate) fn warn_once(found: &[Duplicate]) {
    thread_local! { static WARNED: std::cell::RefCell<HashSet<String>> = Default::default(); }
    if found.is_empty() { return; }
    WARNED.with_borrow_mut(|warned| {
        for d in found {
            if warned.insert(d.key.clone()) { eprintln!("rune: {}; the later sibling is renamed {:?}", d.message, format!("{}#dupN", d.key)); }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{col, el};

    #[test]
    fn duplicate_keys_report_both_sources_and_allow_other_parents() {
        let first = el().key("card");
        let second = el().key("card");
        let first_source = first.key_location();
        let second_source = second.key_location();
        let tree = col().key("cards").children([first, second]);
        let found = warnings(&tree);
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("\"card\""));
        assert!(found[0].contains(&first_source));
        assert!(found[0].contains(&second_source));
        #[cfg(debug_assertions)]
        assert_ne!(first_source, second_source);
        assert!(warnings(&col().children([
            col().key("a").child(el().key("card")),
            col().key("b").child(el().key("card")),
            el(), el(),
        ])).is_empty());
    }

    #[test]
    fn repair_keeps_the_first_and_renames_later_duplicates_deterministically() {
        let keys = |el: &El| el.children.iter().map(|c| c.key.clone().unwrap_or_default()).collect::<Vec<_>>();
        let build = || col().children([
            el().key("row:R-94"), el().key("row:R-94#dup1"), el(), el().key("row:R-94"), el().key("row:R-94"),
            col().key("inner").children([el().key("x"), el().key("x")]),
        ]);
        let mut tree = build();
        let found = repair(&mut tree);
        assert_eq!(found.iter().map(|d| d.key.as_str()).collect::<Vec<_>>(), ["row:R-94", "row:R-94", "x"]);
        assert_eq!(keys(&tree), ["row:R-94", "row:R-94#dup1", "", "row:R-94#dup2", "row:R-94#dup3", "inner"]);
        assert_eq!(keys(&tree.children[5]), ["x", "x#dup1"]);
        assert!(warnings(&tree).is_empty());
        let mut again = build();
        repair(&mut again);
        assert_eq!(keys(&again), keys(&tree), "the same tree repairs the same way every build");
        let mut clean = col().children([el().key("a"), el().key("b")]);
        assert!(repair(&mut clean).is_empty());
    }
}
