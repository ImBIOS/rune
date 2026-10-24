//! Key checks run before virtualization, so offscreen siblings are checked too.
use std::collections::HashMap;

use crate::El;

pub(crate) fn warnings(root: &El) -> Vec<String> {
    fn walk(el: &El, path: &str, out: &mut Vec<String>) {
        let mut seen: HashMap<&str, &El> = HashMap::new();
        for (index, child) in el.all_children().enumerate() {
            if let Some(key) = &child.key {
                if let Some(first) = seen.get(key.as_str()) {
                    out.push(format!(
                        "duplicate sibling key {key:?} under {path:?}: first at {}, again at {}; give siblings unique keys",
                        first.key_location(), child.key_location(),
                    ));
                } else {
                    seen.insert(key, child);
                }
            }
            let segment = child.key.clone().unwrap_or_else(|| format!("#{index}"));
            walk(child, &format!("{path}/{segment}"), out);
        }
    }
    let mut out = Vec::new();
    walk(root, root.key.as_deref().unwrap_or("#0"), &mut out);
    out
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
}
