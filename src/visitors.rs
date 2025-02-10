use anyhow::Result;
use serde::de::{MapAccess, Visitor, Error};
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
                let value = access.next_value::<serde_json::Value>()?;
                let mut value_str : Option<String> = None;

                for cur_key in get_keys_by_root(key, self.orig_keys.clone()).iter() {
                    if is_multi_level_key(cur_key)   {
                        let json = value_str.get_or_insert_with(|| {
                            serde_json::to_string(&value).unwrap()
                        });

                        let value = parse_multi_level_key(get_sub_keys(cur_key), json).map_err(M::Error::custom)?;
                        values.insert(cur_key.to_owned(), value.clone());

                    } else {
                        values.insert(cur_key.to_owned(), value.clone());
                    }
                }

                if values.len() == self.root_keys.len() {
                    // Consume the rest of the input without parsing it
                    while access
                        .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                        .is_some()
                    {}

                    // return the values in the order of the original keys...
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

#[inline]
fn parse_multi_level_key(keys : Vec<&str>, json : &str) -> Result<serde_json::Value> {
    let mut doc : serde_json::Value = serde_json::from_str(json)?;
    for k in keys {
        doc = doc.get(k).ok_or(anyhow::anyhow!("Key not found"))?.clone();
    }

    Ok(doc)
}


#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn parse_multi_level_key() -> Result<()> {
        let key = "root.b.c";
        let json = r#"{"b": {"c": 1}}"#;
        let value = super::parse_multi_level_key(get_sub_keys(key), json)?;
        assert_eq!(value, serde_json::Value::Number(serde_json::Number::from(1)));
        Ok(())
    }
}