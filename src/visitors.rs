use anyhow::Result;
use serde::de::{MapAccess, Visitor};
use serde::Deserializer;
use serde_json::Value;
use std::collections::BTreeMap;
use std::{collections::BTreeSet, fmt};

#[derive(Debug, Clone)]
pub struct SelectiveVisitor {
    pub orig_keys: Vec<String>,
    pub root_keys: BTreeSet<String>,
}

impl SelectiveVisitor {
    pub fn new(keys: Vec<String>) -> Self {
        Self {
            orig_keys: keys.clone(),
            root_keys: keys.into_iter().map(|k| get_root_key(&k).to_owned()).collect(),
        }
    }

    pub fn get_values_by_keys(
        self,
        json: &str,
    ) -> Result<Vec<serde_json::Value>, serde_json::Error> {
        let deserializer = &mut serde_json::Deserializer::from_str(json);
        let result = deserializer.deserialize_map(self)?;

        // Consume any remaining whitespace or trailing characters
        deserializer.end()?;

        Ok(result)
    }
}

impl<'de> Visitor<'de> for SelectiveVisitor {
    type Value = Vec<serde_json::Value>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut values = BTreeMap::new();

        while let Some(key) = access.next_key::<&str>()? {
            if self.root_keys.contains(key) {
                // Deserialize the value for this root key.
                let value = access.next_value::<serde_json::Value>()?;

                // Process all requested keys with the same root.
                for cur_key in get_keys_by_root(key, self.orig_keys.to_vec()) {
                    if is_multi_level_key(&cur_key) {
                        // Directly traverse the nested JSON value.
                        let sub_keys = get_sub_keys(&cur_key);
                        let nested_value = traverse_value(&value, &sub_keys);
                        values.insert(cur_key, nested_value);
                    } else {
                        // Flat key: just insert the value.
                        values.insert(cur_key, value.clone());
                    }
                }

                // If we've found values for all root keys, we can exit early.
                if values.len() >= self.orig_keys.len() {
                    while access
                        .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                        .is_some()
                    {}
                    return Ok(self
                        .orig_keys
                        .iter()
                        .map(|key| values.remove(key).unwrap_or(Value::Null))
                        .collect());
                }
            } else {
                // Skip values for keys we don't care about
                access.next_value::<serde::de::IgnoredAny>()?;
            }
        }

        Ok(self
            .orig_keys
            .iter()
            .map(|key| values.remove(key).unwrap_or(Value::Null))
            .collect())
    }
}

/// Recursively traverses `value` using the provided sub_keys slice.
/// If any level is missing, returns Value::Null.
fn traverse_value(value: &Value, sub_keys: &[&str]) -> Value {
    let mut current = value;
    for key in sub_keys {
        current = match current {
            Value::Object(map) => map.get(*key).unwrap_or(&Value::Null),
            _ => return Value::Null,
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
        Self {
            expr
        }
    }

    pub fn get_keys_by_regex(
        self,
        json: &str,
    ) -> Result<Vec<(String, serde_json::Value)>, serde_json::Error> {
        let mut result = vec![];
        let doc : serde_json::Value = serde_json::from_str(json)?;
        for (key, value) in doc.as_object().unwrap().iter() {
            if self.expr.is_match(&value.to_string()) {
                result.push((key.clone(), value.clone()));
            }
        }
        Ok(result)
    }
}

#[inline]
fn get_keys_by_root(root: &str, keys: Vec<String>) -> Vec<String> {
    keys.into_iter().filter(|k| get_root_key(k) == root).collect()
}

#[inline]
fn get_root_key(key : &str) -> &str {
    key.split('.').next().unwrap()
}

#[inline]
fn get_sub_keys(key : &str) -> Vec<&str> {
    key.split('.').skip(1).collect()
}

#[inline]
fn is_multi_level_key(key : &str) -> bool {
    key.contains('.')
}


#[cfg(test)]
mod tests {
    // use super::*;

    #[test]
    fn root_key_basic() {
        let key = "a";
        let root_key = super::get_root_key(key);
        assert_eq!(root_key, "a");
    }

    #[test]
    fn root_key() {
        let key = "a.b.c";
        let root_key = super::get_root_key(key);
        assert_eq!(root_key, "a");
    }

    #[test]
    fn sub_keys() {
        let key = "a.b.c";
        let sub_keys = super::get_sub_keys(key);
        assert_eq!(sub_keys, vec!["b", "c"]);
    }

    #[test]
    fn sub_keys_empty() {
        let key = "a";
        let sub_keys = super::get_sub_keys(key);
        assert!(sub_keys.is_empty());
    }
}