use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use smol_str::SmolStr;
use std::collections::BTreeMap;
use std::fmt;

#[derive(Serialize, Debug, Clone, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum SmolValue {
    Null,
    Bool(bool),
    Number(Number),
    String(SmolStr),
    Array(Vec<SmolValue>),
    Object(BTreeMap<SmolStr, SmolValue>),
}

/// Custom Deserialize: uses `deserialize_any` to avoid the runtime
/// overhead of `#[serde(untagged)]` which tries variants sequentially.
impl<'de> Deserialize<'de> for SmolValue {
    fn deserialize<D>(deserializer: D) -> Result<SmolValue, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(SmolValueVisitor)
    }
}

struct SmolValueVisitor;

impl<'de> Visitor<'de> for SmolValueVisitor {
    type Value = SmolValue;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("any valid JSON value")
    }

    #[inline]
    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
        Ok(SmolValue::Bool(v))
    }

    #[inline]
    fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
        Ok(SmolValue::Number(Number(v as f64)))
    }

    #[inline]
    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
        Ok(SmolValue::Number(Number(v as f64)))
    }

    #[inline]
    fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E> {
        Ok(SmolValue::Number(Number(v)))
    }

    #[inline]
    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        // fallback: string had escapes, must copy
        Ok(SmolValue::String(SmolStr::new(v)))
    }

    #[inline]
    fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        // SAFETY: the caller (aggregate::aggregate) has leaked the input
        // buffer via Box::leak, making all &str references effectively 'static.
        // This enables SmolStr::new_static which stores a pointer without copying.
        let static_str: &'static str = unsafe { std::mem::transmute(v) };
        Ok(SmolValue::String(SmolStr::new_static(static_str)))
    }

    #[inline]
    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(SmolValue::String(SmolStr::new(v)))
    }

    #[inline]
    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(SmolValue::Null)
    }

    #[inline]
    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(SmolValue::Null)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut v = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(elem) = seq.next_element()? {
            v.push(elem);
        }
        Ok(SmolValue::Array(v))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut m = BTreeMap::new();
        while let Some((key, value)) = map.next_entry::<SmolStr, SmolValue>()? {
            m.insert(key, value);
        }
        Ok(SmolValue::Object(m))
    }
}

/// A wrapper around f64 that implements Eq and Hash for use as map keys.
/// NaN values are coerced to 0.0 for hashing/equality purposes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Number(f64);

impl PartialEq for Number {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for Number {}

impl std::hash::Hash for Number {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for SmolValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmolValue::Null => write!(f, "null"),
            SmolValue::Bool(b) => write!(f, "{}", b),
            SmolValue::Number(n) => write!(f, "{}", n),
            SmolValue::String(s) => write!(f, "{}", s),
            SmolValue::Array(arr) => {
                write!(f, "[")?;
                for (i, v) in arr.iter().enumerate() {
                    if i > 0 {
                        write!(f, ",")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            SmolValue::Object(map) => {
                write!(f, "{{")?;
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        write!(f, ",")?;
                    }
                    write!(f, "{}:{}", k, v)?;
                }
                write!(f, "}}")
            }
        }
    }
}
