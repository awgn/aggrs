use anyhow::Result;
use serde::de::{MapAccess, Visitor};
use serde::Deserializer;
use std::{collections::BTreeSet, fmt};

#[derive(Debug, Clone)]
pub struct SelectiveVisitor {
    pub keys: BTreeSet<String>,
}

impl SelectiveVisitor {
    pub fn new(keys: Vec<String>) -> Self {
        Self {
            keys: keys.into_iter().collect(),
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
        let mut values = Vec::with_capacity(self.keys.len());

        while let Some(key) = access.next_key::<&str>()? {
            if self.keys.contains(key) {
                let value = access.next_value::<serde_json::Value>()?;
                values.push(value);

                if values.len() == self.keys.len() {
                    // Consume the rest of the input without parsing it
                    while access
                        .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                        .is_some()
                    {}
                    return Ok(values);
                }
            } else {
                // Skip values for keys we don't care about
                access.next_value::<serde::de::IgnoredAny>()?;
            }
        }
        Ok(values)
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
