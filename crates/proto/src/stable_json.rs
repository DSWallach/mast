//! Canonical JSON for settings files users keep under version control.

use serde::Serialize;

/// Pretty-print with every object's keys sorted, so unchanged data always
/// serializes to the same bytes. HashMap iteration order and the workspace's
/// `preserve_order` feature would otherwise reshuffle keys between saves.
pub fn to_vec_pretty_sorted<T: Serialize>(value: &T) -> serde_json::Result<Vec<u8>> {
    let mut value = serde_json::to_value(value)?;
    value.sort_all_objects();
    serde_json::to_vec_pretty(&value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn nested_keys_are_sorted_and_output_is_repeatable() {
        let mut map = HashMap::new();
        for key in ["m", "b", "z", "a", "q"] {
            map.insert(key.to_owned(), HashMap::from([("y", 1), ("x", 2)]));
        }
        let first = to_vec_pretty_sorted(&map).unwrap();
        let text = String::from_utf8(first.clone()).unwrap();
        let order: Vec<usize> = ["\"a\"", "\"b\"", "\"m\"", "\"q\"", "\"z\""]
            .iter()
            .map(|key| text.find(key).unwrap())
            .collect();
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(text.find("\"x\"").unwrap() < text.find("\"y\"").unwrap());
        assert_eq!(first, to_vec_pretty_sorted(&map).unwrap());
    }
}
