use crate::merge::AHashMap;
use anyhow::Result;
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::smolvalue::{scan_json_string, KeyCow, SinglePathSeed, SmolValue};

/// A compiled recursive extraction tree for a single JSON root key.
#[derive(Debug, Clone, Default)]
struct PathNode {
    slots: Vec<usize>,
    children: Vec<(String, PathNode)>,
}

impl PathNode {
    fn insert(&mut self, path: &[String], slot: usize) {
        if path.is_empty() {
            self.slots.push(slot);
        } else {
            let first = &path[0];
            if let Some((_, child)) = self.children.iter_mut().find(|(k, _)| k == first) {
                child.insert(&path[1..], slot);
            } else {
                let mut child = PathNode::default();
                child.insert(&path[1..], slot);
                self.children.push((first.clone(), child));
            }
        }
    }

    #[inline]
    fn extract<'de, D>(&self, deserializer: D, result: &mut [SmolValue]) -> Result<(), D::Error>
    where
        D: Deserializer<'de>,
    {
        if !self.slots.is_empty() {
            let val = SmolValue::deserialize(deserializer)?;
            self.fill_from_value(Some(val), result);
            Ok(())
        } else {
            deserializer.deserialize_any(PathNodeVisitor { node: self, result })
        }
    }

    fn fill_from_value(&self, mut val: Option<SmolValue>, result: &mut [SmolValue]) {
        for (k, child) in &self.children {
            let sub_val = match val.as_ref() {
                Some(SmolValue::Object(map)) => map
                    .binary_search_by(|(mk, _)| mk.as_str().cmp(k.as_str()))
                    .ok()
                    .map(|idx| map[idx].1.clone()),
                _ => None,
            };
            child.fill_from_value(sub_val, result);
        }
        if !self.slots.is_empty() {
            let v = val.take().unwrap_or(SmolValue::Null);
            let last = self.slots.len() - 1;
            for (i, &slot) in self.slots.iter().enumerate() {
                if i == last {
                    result[slot] = v;
                    break;
                } else {
                    result[slot] = v.clone();
                }
            }
        }
    }
}

struct PathNodeSeed<'a, 'b> {
    node: &'a PathNode,
    result: &'b mut [SmolValue],
}

impl<'de> DeserializeSeed<'de> for PathNodeSeed<'_, '_> {
    type Value = ();

    #[inline]
    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.node.extract(deserializer, self.result)
    }
}

struct PathNodeVisitor<'a, 'b> {
    node: &'a PathNode,
    result: &'b mut [SmolValue],
}

impl<'de> Visitor<'de> for PathNodeVisitor<'_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("any JSON value")
    }

    #[inline]
    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    #[inline]
    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    #[inline]
    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    #[inline]
    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    #[inline]
    fn visit_str<E>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    #[inline]
    fn visit_borrowed_str<E>(self, _: &'de str) -> Result<(), E> {
        Ok(())
    }
    #[inline]
    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }
    #[inline]
    fn visit_none<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<(), A::Error>
    where
        A: SeqAccess<'de>,
    {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(())
    }

    fn visit_map<A>(self, mut map: A) -> Result<(), A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut remaining = self.node.children.len();
        while let Some(key) = map.next_key::<KeyCow<'de>>()? {
            let k_str = key.as_str();
            if let Some((_, child)) = self.node.children.iter().find(|(ck, _)| ck == k_str) {
                map.next_value_seed(PathNodeSeed {
                    node: child,
                    result: self.result,
                })?;
                remaining -= 1;
                if remaining == 0 {
                    while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                    break;
                }
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

/// Pre-built mapping: for each JSON root key encountered at runtime,
/// which output slots (indices into the result Vec) to fill and with
/// which sub-key path (empty = flat key, no traversal).
#[derive(Debug, Clone)]
pub struct SelectiveVisitor {
    /// root_json_key → [(output_index, sub_keys_to_traverse)]
    #[allow(dead_code)]
    root_map: AHashMap<String, Vec<(usize, Vec<String>)>>,
    /// Compiled multi-branch extraction tree per root key
    root_nodes: AHashMap<String, PathNode>,
    /// Optimization for the common single-root-key case: avoids hash lookup per JSON field
    single_root: Option<(String, Vec<(usize, Vec<String>)>, PathNode)>,
    /// Pre-split byte path when exactly 1 key is requested (`-k key` or `-k a.b.c`)
    single_path: Option<Vec<Vec<u8>>>,
    num_keys: usize,
    num_roots: usize,
}

/// Borrowed version for serde Visitor — avoids cloning HashMap.
struct BorrowedVisitor<'a> {
    root_nodes: &'a AHashMap<String, PathNode>,
    single_root: Option<(&'a str, &'a [(usize, Vec<String>)], &'a PathNode)>,
    num_keys: usize,
    num_roots: usize,
}

impl SelectiveVisitor {
    pub fn new(keys: Vec<String>) -> Self {
        let mut root_map: AHashMap<String, Vec<(usize, Vec<String>)>> =
            AHashMap::with_hasher(ahash::RandomState::new());
        let mut root_nodes: AHashMap<String, PathNode> =
            AHashMap::with_hasher(ahash::RandomState::new());

        for (idx, key) in keys.iter().enumerate() {
            let root = key.split('.').next().unwrap().to_owned();
            let sub_keys: Vec<String> = key.split('.').skip(1).map(|s| s.to_owned()).collect();
            root_nodes
                .entry(root.clone())
                .or_default()
                .insert(&sub_keys, idx);
            root_map.entry(root).or_default().push((idx, sub_keys));
        }

        let num_roots = root_map.len();
        let single_root = if num_roots == 1 {
            let (k, v) = root_map.iter().next().unwrap();
            let node = root_nodes.get(k).unwrap().clone();
            Some((k.clone(), v.clone(), node))
        } else {
            None
        };

        let single_path = if keys.len() == 1 {
            Some(keys[0].split('.').map(|s| s.as_bytes().to_vec()).collect())
        } else {
            None
        };

        Self {
            num_keys: keys.len(),
            num_roots,
            root_map,
            root_nodes,
            single_root,
            single_path,
        }
    }

    #[inline]
    pub fn is_single_key(&self) -> bool {
        self.single_path.is_some()
    }

    /// Ultra-fast zero-allocation byte-level JSON path extractor for the single-key case.
    /// Returns the raw JSON value slice (or `b"null"` if missing).
    #[inline]
    pub fn extract_raw_single<'a>(&self, json: &'a [u8]) -> &'a [u8] {
        let path = unsafe { self.single_path.as_ref().unwrap_unchecked() };
        extract_raw_path(json, path).unwrap_or(b"null")
    }

    #[inline]
    pub fn get_values_by_keys(&self, json: &str) -> Result<Vec<SmolValue>, serde_json::Error> {
        let deserializer = &mut serde_json::Deserializer::from_str(json);
        let visitor = BorrowedVisitor {
            root_nodes: &self.root_nodes,
            single_root: self
                .single_root
                .as_ref()
                .map(|(k, v, n)| (k.as_str(), v.as_slice(), n)),
            num_keys: self.num_keys,
            num_roots: self.num_roots,
        };
        let result = deserializer.deserialize_map(visitor)?;
        Ok(result)
    }
}

#[inline(always)]
fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && unsafe { *bytes.get_unchecked(i) } <= b' ' {
        i += 1;
    }
    i
}

#[inline(always)]
fn skip_json_container(bytes: &[u8], mut i: usize) -> Option<usize> {
    let len = bytes.len();
    let mut depth = 1u32;
    i += 1; // past opening '{' or '['
    while i < len {
        let b = unsafe { *bytes.get_unchecked(i) };
        if b == b'"' {
            let (end, _) = scan_json_string(bytes, i)?;
            i = end;
        } else if b == b'{' || b == b'[' {
            depth += 1;
            i += 1;
        } else if b == b'}' || b == b']' {
            depth -= 1;
            i += 1;
            if depth == 0 {
                return Some(i);
            }
        } else {
            i += 1;
        }
    }
    None
}

#[inline(always)]
pub fn skip_json_value(bytes: &[u8], pos: usize) -> Option<usize> {
    if pos >= bytes.len() {
        return None;
    }
    match unsafe { *bytes.get_unchecked(pos) } {
        b'"' => scan_json_string(bytes, pos).map(|(end, _)| end),
        b'{' | b'[' => skip_json_container(bytes, pos),
        b't' | b'n' => Some((pos + 4).min(bytes.len())),
        b'f' => Some((pos + 5).min(bytes.len())),
        _ => {
            let mut i = pos + 1;
            while i < bytes.len() {
                let c = unsafe { *bytes.get_unchecked(i) };
                if matches!(c, b',' | b'}' | b']') || c <= b' ' {
                    break;
                }
                i += 1;
            }
            Some(i)
        }
    }
}

#[inline]
fn extract_raw_path<'a>(json: &'a [u8], path: &[Vec<u8>]) -> Option<&'a [u8]> {
    let len = json.len();
    let mut pos = 0usize;

    for (depth, target_vec) in path.iter().enumerate() {
        let target = target_vec.as_slice();
        pos = skip_ws(json, pos);
        if pos >= len || unsafe { *json.get_unchecked(pos) } != b'{' {
            return None;
        }
        pos += 1; // past '{'

        loop {
            if pos < len && unsafe { *json.get_unchecked(pos) } <= b' ' {
                pos = skip_ws(json, pos);
            }
            if pos >= len {
                return None;
            }
            let b = unsafe { *json.get_unchecked(pos) };
            if b == b'}' {
                return None;
            }
            if b != b'"' {
                return None;
            }

            let key_quote_pos = pos;
            let (key_end, key_has_esc) = scan_json_string(json, key_quote_pos)?;
            let key_slice = unsafe { json.get_unchecked(key_quote_pos + 1..key_end - 1) };

            // Advance past ':'
            if key_end < len && unsafe { *json.get_unchecked(key_end) } == b':' {
                pos = key_end + 1;
            } else {
                pos = skip_ws(json, key_end);
                if pos >= len || unsafe { *json.get_unchecked(pos) } != b':' {
                    return None;
                }
                pos += 1;
            }
            if pos < len && unsafe { *json.get_unchecked(pos) } <= b' ' {
                pos = skip_ws(json, pos);
            }

            let matched = if !key_has_esc {
                key_slice == target
            } else if let Ok(unesc) =
                serde_json::from_slice::<smol_str::SmolStr>(&json[key_quote_pos..key_end])
            {
                unesc.as_bytes() == target
            } else {
                false
            };

            if matched {
                if depth + 1 == path.len() {
                    let val_end = skip_json_value(json, pos)?;
                    return Some(unsafe { json.get_unchecked(pos..val_end) });
                } else {
                    break;
                }
            } else {
                pos = skip_json_value(json, pos)?;
                if pos < len && unsafe { *json.get_unchecked(pos) } == b',' {
                    pos += 1;
                } else {
                    pos = skip_ws(json, pos);
                    if pos < len && unsafe { *json.get_unchecked(pos) } == b',' {
                        pos += 1;
                    } else {
                        return None;
                    }
                }
            }
        }
    }
    None
}

impl<'de> Visitor<'de> for BorrowedVisitor<'_> {
    type Value = Vec<SmolValue>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a JSON object")
    }

    #[inline]
    fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut result = vec![SmolValue::Null; self.num_keys];

        // Fast path when all requested keys share a single root key (e.g. `-k info` or `-k info.dns`)
        if let Some((target_root, slots, node)) = self.single_root {
            while let Some(key) = access.next_key::<KeyCow<'de>>()? {
                if key.as_str() == target_root {
                    if slots.len() == 1 {
                        let (idx, ref sub_keys) = slots[0];
                        result[idx] = access.next_value_seed(SinglePathSeed(sub_keys))?;
                    } else {
                        access.next_value_seed(PathNodeSeed {
                            node,
                            result: &mut result,
                        })?;
                    }
                    while access
                        .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                        .is_some()
                    {}
                    return Ok(result);
                } else {
                    access.next_value::<serde::de::IgnoredAny>()?;
                }
            }
            return Ok(result);
        }

        let mut remaining_roots = self.num_roots;

        while let Some(key) = access.next_key::<KeyCow<'de>>()? {
            if let Some(node) = self.root_nodes.get(key.as_str()) {
                access.next_value_seed(PathNodeSeed {
                    node,
                    result: &mut result,
                })?;
                remaining_roots -= 1;

                if remaining_roots == 0 {
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

    #[test]
    fn multi_nested_extraction() {
        let v = SelectiveVisitor::new(vec![
            "info.dns.server".into(),
            "info.dns.client".into(),
            "info.anomaly.unique_ttl".into(),
        ]);
        let json = r#"{"id":"1","info":{"dns":{"server":"8.8.8.8","client":"10.0.0.1","latency":5},"anomaly":{"unique_ttl":2}}}"#;
        let vals = v.get_values_by_keys(json).unwrap();
        assert_eq!(vals.len(), 3);
        assert_eq!(vals[0].to_string(), "8.8.8.8");
        assert_eq!(vals[1].to_string(), "10.0.0.1");
        assert_eq!(vals[2].to_string(), "2");
    }
}
