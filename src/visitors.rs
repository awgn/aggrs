use crate::merge::AHashMap;
use anyhow::Result;
use serde::de::{MapAccess, Visitor};
use serde::Deserializer;

use crate::smolvalue::SmolValue;

/// Pre-built mapping: for each JSON root key encountered at runtime,
/// which output slots (indices into the result Vec) to fill and with
/// which sub-key path (empty = flat key, no traversal).
#[derive(Debug, Clone)]
pub struct SelectiveVisitor {
    /// root_json_key → [(output_index, sub_keys_to_traverse)]
    root_map: AHashMap<String, Vec<(usize, Vec<String>)>>,
    num_keys: usize,
}

/// Borrowed version for serde Visitor — avoids cloning HashMap.
struct BorrowedVisitor<'a> {
    root_map: &'a AHashMap<String, Vec<(usize, Vec<String>)>>,
    num_keys: usize,
}

impl SelectiveVisitor {
    pub fn new(keys: Vec<String>) -> Self {
        let mut root_map: AHashMap<String, Vec<(usize, Vec<String>)>> =
            AHashMap::with_hasher(ahash::RandomState::new());

        for (idx, key) in keys.iter().enumerate() {
            let root = key.split('.').next().unwrap().to_owned();
            let sub_keys: Vec<String> = key.split('.').skip(1).map(|s| s.to_owned()).collect();
            root_map.entry(root).or_default().push((idx, sub_keys));
        }

        Self {
            num_keys: keys.len(),
            root_map,
        }
    }

    pub fn get_values_by_keys(&self, json: &str) -> Result<Vec<SmolValue>, serde_json::Error> {
        let deserializer = &mut serde_json::Deserializer::from_str(json);
        let visitor = BorrowedVisitor {
            root_map: &self.root_map,
            num_keys: self.num_keys,
        };
        let result = deserializer.deserialize_map(visitor)?;
        deserializer.end()?;
        Ok(result)
    }
}

impl<'de> Visitor<'de> for BorrowedVisitor<'_> {
    type Value = Vec<SmolValue>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut result = vec![SmolValue::Null; self.num_keys];
        let mut filled = 0usize;

        while let Some(key) = access.next_key::<&str>()? {
            if let Some(slots) = self.root_map.get(key) {
                let mut value = Some(access.next_value::<SmolValue>()?);
                let last = slots.len() - 1;

                for (i, &(idx, ref sub_keys)) in slots.iter().enumerate() {
                    let v = if i == last {
                        // Last slot: move, no clone.
                        if sub_keys.is_empty() {
                            value.take().unwrap()
                        } else {
                            traverse_value(&value.take().unwrap(), sub_keys)
                        }
                    } else {
                        // Not last: clone only what we need.
                        if sub_keys.is_empty() {
                            value.as_ref().unwrap().clone()
                        } else {
                            traverse_value(value.as_ref().unwrap(), sub_keys)
                        }
                    };
                    result[idx] = v;
                    filled += 1;
                }

                if filled >= self.num_keys {
                    while access
                        .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                        .is_some()
                    {}
                    return Ok(result);
                }
            } else {
                access.next_value::<serde::de::IgnoredAny>()?;
            }
        }

        Ok(result)
    }
}

/// Recursively traverses `value` using the provided sub_keys slice.
/// If any level is missing, returns SmolValue::Null.
fn traverse_value(value: &SmolValue, sub_keys: &[String]) -> SmolValue {
    let mut current = value;
    for key in sub_keys {
        current = match current {
            SmolValue::Object(map) => map.get(key.as_str()).unwrap_or(&SmolValue::Null),
            _ => return SmolValue::Null,
        };
    }
    current.clone()
}

#[derive(Debug, Clone)]
pub struct RegexVisitor {
    pub expr: regex::Regex,
}

impl RegexVisitor {
    pub fn new(expr: regex::Regex) -> Self {
        Self { expr }
    }

    pub fn get_keys_by_regex(
        &self,
        json: &str,
    ) -> Result<Vec<(String, SmolValue)>, serde_json::Error> {
        let mut result = vec![];
        let doc: SmolValue = serde_json::from_str(json)?;
        if let SmolValue::Object(map) = &doc {
            for (key, value) in map.iter() {
                if self.expr.is_match(&value.to_string()) {
                    result.push((key.to_string(), value.clone()));
                }
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prebuilt_flat_keys() {
        let v = SelectiveVisitor::new(vec!["a".into(), "b".into(), "c".into()]);
        assert_eq!(v.num_keys, 3);
        assert_eq!(v.root_map.len(), 3);
        assert!(v.root_map["a"][0].1.is_empty());
    }

    #[test]
    fn prebuilt_nested_keys() {
        let v = SelectiveVisitor::new(vec!["a.x".into(), "a.y".into(), "b".into()]);
        assert_eq!(v.num_keys, 3);
        assert_eq!(v.root_map.len(), 2); // roots: a, b
        assert_eq!(v.root_map["a"].len(), 2);
        assert_eq!(v.root_map["a"][0].1, vec!["x"]);
        assert_eq!(v.root_map["a"][1].1, vec!["y"]);
    }
}
