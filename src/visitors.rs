use std::{collections::BTreeSet, fmt};
use serde::de::{MapAccess, Visitor};
use anyhow::Result;
use serde::Deserializer;


#[derive(Debug, Clone)]
pub struct SelectiveVisitor {
    pub keys: BTreeSet<String>,
    pub values: Vec<serde_json::Value>,
}

impl SelectiveVisitor {
    pub fn new(keys: Vec<String>) -> Self {
        Self {
            values: Vec::with_capacity(keys.len()),
            keys: keys.into_iter().collect(),
        }
    }
}

impl<'de> Visitor<'de> for SelectiveVisitor {
    type Value = Vec<serde_json::Value>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<M>(mut self, mut access: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        while let Some(key) = access.next_key::<String>()? {
            if self.keys.contains(&key) {
                let value = access.next_value::<serde_json::Value>()?;
                self.values.push(value);

                if self.values.len() == self.keys.len() {
                    // Consume the rest of the input without parsing it
                    while access
                        .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                        .is_some()
                    {}
                    return Ok(self.values);
                }
            } else {
                // Skip values for keys we don't care about
                access.next_value::<serde::de::IgnoredAny>()?;
            }
        }
        Ok(self.values)
    }
}

pub fn parse_selected_keys(
    json: &str,
    visitor: SelectiveVisitor,
) -> Result<Vec<serde_json::Value>, serde_json::Error> {
    let deserializer = &mut serde_json::Deserializer::from_str(json);
    let result = deserializer.deserialize_map(visitor)?;

    // Consume any remaining whitespace or trailing characters
    deserializer.end()?;

    Ok(result)
}