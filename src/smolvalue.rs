use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use smol_str::SmolStr;
use std::fmt::{self, Display};
use std::io::Write;

const REPEAT_01: u64 = 0x0101_0101_0101_0101;
const REPEAT_80: u64 = 0x8080_8080_8080_8080;

#[derive(Serialize, Debug, Clone, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum SmolValue {
    Null,
    Bool(bool),
    Number(Number),
    String(SmolStr),
    Array(Vec<SmolValue>),
    Object(Vec<(SmolStr, SmolValue)>),
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

pub struct SmolValueVisitor;

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
        Ok(SmolValue::String(SmolStr::new(v)))
    }

    #[inline]
    fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
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
        let mut v = Vec::with_capacity(map.size_hint().unwrap_or(4));
        while let Some((key, value)) = map.next_entry::<BorrowedSmolStr, SmolValue>()? {
            v.push((key.0, value));
        }
        v.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        v.dedup_by(|a, b| {
            if a.0 == b.0 {
                b.1 = std::mem::replace(&mut a.1, SmolValue::Null);
                true
            } else {
                false
            }
        });
        Ok(SmolValue::Object(v))
    }
}

/// Zero-copy wrapper around `SmolStr` for deserializing borrowed keys/strings
/// from a `'static` leaked input buffer without copying strings > 23 bytes.
pub struct BorrowedSmolStr(pub SmolStr);

impl<'de> Deserialize<'de> for BorrowedSmolStr {
    #[inline]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BorrowedStrVisitor;

        impl<'de> Visitor<'de> for BorrowedStrVisitor {
            type Value = BorrowedSmolStr;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a string")
            }

            #[inline]
            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(BorrowedSmolStr(SmolStr::new(v)))
            }

            #[inline]
            fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let static_str: &'static str = unsafe { std::mem::transmute(v) };
                Ok(BorrowedSmolStr(SmolStr::new_static(static_str)))
            }

            #[inline]
            fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(BorrowedSmolStr(SmolStr::new(v)))
            }
        }

        deserializer.deserialize_str(BorrowedStrVisitor)
    }
}

/// A borrowed string reference that tolerates JSON escape sequences by
/// falling back to a temporary allocation only when necessary.
pub enum KeyCow<'de> {
    Borrowed(&'de str),
    Owned(SmolStr),
}

impl KeyCow<'_> {
    #[inline]
    pub fn as_str(&self) -> &str {
        match self {
            KeyCow::Borrowed(s) => s,
            KeyCow::Owned(s) => s.as_str(),
        }
    }
}

impl<'de> Deserialize<'de> for KeyCow<'de> {
    #[inline]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct KeyCowVisitor;

        impl<'de> Visitor<'de> for KeyCowVisitor {
            type Value = KeyCow<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a string key")
            }

            #[inline]
            fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(KeyCow::Borrowed(v))
            }

            #[inline]
            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(KeyCow::Owned(SmolStr::new(v)))
            }

            #[inline]
            fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(KeyCow::Owned(SmolStr::from(v)))
            }
        }

        deserializer.deserialize_str(KeyCowVisitor)
    }
}

/// Fast single-path streaming extractor that traverses a nested JSON path
/// without allocating intermediate `BTreeMap`s or deserializing sibling fields.
pub struct SinglePathSeed<'a>(pub &'a [String]);

impl<'de> serde::de::DeserializeSeed<'de> for SinglePathSeed<'_> {
    type Value = SmolValue;

    #[inline]
    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        if self.0.is_empty() {
            SmolValue::deserialize(deserializer)
        } else {
            deserializer.deserialize_any(self)
        }
    }
}

impl<'de> Visitor<'de> for SinglePathSeed<'_> {
    type Value = SmolValue;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    #[inline]
    fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
        Ok(SmolValue::Null)
    }
    #[inline]
    fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
        Ok(SmolValue::Null)
    }
    #[inline]
    fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
        Ok(SmolValue::Null)
    }
    #[inline]
    fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
        Ok(SmolValue::Null)
    }
    #[inline]
    fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
        Ok(SmolValue::Null)
    }
    #[inline]
    fn visit_borrowed_str<E>(self, _: &'de str) -> Result<Self::Value, E> {
        Ok(SmolValue::Null)
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
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(SmolValue::Null)
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let target = self.0[0].as_str();
        let rest = &self.0[1..];
        let mut found = SmolValue::Null;

        while let Some(key) = map.next_key::<KeyCow<'de>>()? {
            if key.as_str() == target {
                found = map.next_value_seed(SinglePathSeed(rest))?;
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                break;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(found)
    }
}

/// A wrapper around f64 that implements Eq and Hash for use as map keys.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Number(pub f64);

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
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl SmolValue {
    /// Fast direct write to any `io::Write` matching `Display` 100%.
    #[inline]
    pub fn write_to<W: std::io::Write>(&self, w: &mut W) -> std::io::Result<()> {
        match self {
            SmolValue::Null => w.write_all(b"null"),
            SmolValue::Bool(true) => w.write_all(b"true"),
            SmolValue::Bool(false) => w.write_all(b"false"),
            SmolValue::Number(n) => {
                let v = n.0;
                if v.fract() == 0.0
                    && v.abs() < 9_007_199_254_740_992.0
                    && (v != 0.0 || !v.is_sign_negative())
                {
                    let mut buf = itoa::Buffer::new();
                    w.write_all(buf.format(v as i64).as_bytes())
                } else {
                    write!(w, "{}", v)
                }
            }
            SmolValue::String(s) => w.write_all(s.as_bytes()),
            SmolValue::Array(arr) => {
                w.write_all(b"[")?;
                for (i, v) in arr.iter().enumerate() {
                    if i > 0 {
                        w.write_all(b",")?;
                    }
                    v.write_to(w)?;
                }
                w.write_all(b"]")
            }
            SmolValue::Object(map) => {
                w.write_all(b"{")?;
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        w.write_all(b",")?;
                    }
                    w.write_all(k.as_bytes())?;
                    w.write_all(b":")?;
                    v.write_to(w)?;
                }
                w.write_all(b"}")
            }
        }
    }

    /// Writes value in `--raw` mode: top-level strings are unquoted, while JSON arrays
    /// and objects are emitted as valid JSON (with quoted and escaped keys/strings).
    #[inline]
    pub fn write_raw_to<W: std::io::Write>(&self, w: &mut W) -> std::io::Result<()> {
        match self {
            SmolValue::Array(_) | SmolValue::Object(_) => self.write_json_to(w),
            _ => self.write_to(w),
        }
    }

    /// Writes value as valid RFC 8259 JSON.
    #[inline]
    pub fn write_json_to<W: std::io::Write>(&self, w: &mut W) -> std::io::Result<()> {
        match self {
            SmolValue::Null => w.write_all(b"null"),
            SmolValue::Bool(true) => w.write_all(b"true"),
            SmolValue::Bool(false) => w.write_all(b"false"),
            SmolValue::Number(n) => {
                let v = n.0;
                if v.fract() == 0.0
                    && v.abs() < 9_007_199_254_740_992.0
                    && (v != 0.0 || !v.is_sign_negative())
                {
                    let mut buf = itoa::Buffer::new();
                    w.write_all(buf.format(v as i64).as_bytes())
                } else {
                    write!(w, "{}", v)
                }
            }
            SmolValue::String(s) => write_json_str_to(w, s.as_str()),
            SmolValue::Array(arr) => {
                w.write_all(b"[")?;
                for (i, v) in arr.iter().enumerate() {
                    if i > 0 {
                        w.write_all(b",")?;
                    }
                    v.write_json_to(w)?;
                }
                w.write_all(b"]")
            }
            SmolValue::Object(map) => {
                w.write_all(b"{")?;
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        w.write_all(b",")?;
                    }
                    write_json_str_to(w, k.as_str())?;
                    w.write_all(b":")?;
                    v.write_json_to(w)?;
                }
                w.write_all(b"}")
            }
        }
    }
}

#[inline]
fn write_json_str_to<W: std::io::Write>(w: &mut W, s: &str) -> std::io::Result<()> {
    w.write_all(b"\"")?;
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'"' || b == b'\\' || b < 0x20 {
            if start < i {
                w.write_all(&bytes[start..i])?;
            }
            match b {
                b'"' => w.write_all(b"\\\"")?,
                b'\\' => w.write_all(b"\\\\")?,
                b'\x08' => w.write_all(b"\\b")?,
                b'\x0c' => w.write_all(b"\\f")?,
                b'\n' => w.write_all(b"\\n")?,
                b'\r' => w.write_all(b"\\r")?,
                b'\t' => w.write_all(b"\\t")?,
                _ => {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    let esc = [
                        b'\\',
                        b'u',
                        b'0',
                        b'0',
                        HEX[(b >> 4) as usize],
                        HEX[(b & 0x0f) as usize],
                    ];
                    w.write_all(&esc)?;
                }
            }
            start = i + 1;
        }
    }
    if start < bytes.len() {
        w.write_all(&bytes[start..])?;
    }
    w.write_all(b"\"")
}

#[inline]
fn write_json_str_vec(buf: &mut Vec<u8>, s: &str) {
    let _ = write_json_str_to(buf, s);
}

impl fmt::Display for SmolValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmolValue::Null => f.write_str("null"),
            SmolValue::Bool(true) => f.write_str("true"),
            SmolValue::Bool(false) => f.write_str("false"),
            SmolValue::Number(n) => Display::fmt(n, f),
            SmolValue::String(s) => f.write_str(s),
            SmolValue::Array(arr) => {
                f.write_str("[")?;
                for (i, v) in arr.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    Display::fmt(v, f)?;
                }
                f.write_str("]")
            }
            SmolValue::Object(map) => {
                f.write_str("{")?;
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    f.write_str(k)?;
                    f.write_str(":")?;
                    Display::fmt(v, f)?;
                }
                f.write_str("}")
            }
        }
    }
}

#[inline(always)]
fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && unsafe { *bytes.get_unchecked(i) } <= b' ' {
        i += 1;
    }
    i
}

/// Scans a JSON string starting at `i` (where `bytes[i] == b'"'`).
/// Returns `Some((index_after_closing_quote, has_backslash))`
#[inline(always)]
pub fn scan_json_string(bytes: &[u8], mut i: usize) -> Option<(usize, bool)> {
    let len = bytes.len();
    i += 1; // past opening '"'
    let mut has_escape = false;
    while i + 8 <= len {
        let word = unsafe { (bytes.as_ptr().add(i) as *const u64).read_unaligned() };
        let x_q = word ^ (REPEAT_01 * (b'"' as u64));
        let q_mask = x_q.wrapping_sub(REPEAT_01) & !x_q & REPEAT_80;
        let x_b = word ^ (REPEAT_01 * (b'\\' as u64));
        let b_mask = x_b.wrapping_sub(REPEAT_01) & !x_b & REPEAT_80;
        if (q_mask | b_mask) == 0 {
            i += 8;
            continue;
        }
        if b_mask == 0 || (q_mask != 0 && q_mask.trailing_zeros() < b_mask.trailing_zeros()) {
            return Some((i + (q_mask.trailing_zeros() >> 3) as usize + 1, has_escape));
        }
        has_escape = true;
        i += (b_mask.trailing_zeros() >> 3) as usize + 2;
    }
    while i < len {
        let b = unsafe { *bytes.get_unchecked(i) };
        i += 1;
        if b == b'"' {
            return Some((i, has_escape));
        } else if b == b'\\' {
            has_escape = true;
            i += 1;
        }
    }
    None
}

/// If `raw` can be represented as a zero-copy slice of `raw` without allocation, returns `Some(slice)`.
/// Otherwise appends the canonical `SmolValue` representation into `buf` (using `tmp` as scratch)
/// and returns `None`. When `raw_mode` is true, JSON arrays and objects are formatted as valid JSON.
#[inline]
pub fn format_or_borrow_canonical<'a>(
    raw: &'a [u8],
    buf: &mut Vec<u8>,
    tmp: &mut Vec<u8>,
    raw_mode: bool,
) -> Option<&'a [u8]> {
    if raw.is_empty() || raw == b"null" {
        return Some(b"null");
    }
    let first = raw[0];
    match first {
        b't' if raw == b"true" => return Some(b"true"),
        b'f' if raw == b"false" => return Some(b"false"),
        b'"' => {
            if let Some((end, has_escape)) = scan_json_string(raw, 0) {
                if end == raw.len() {
                    if !has_escape {
                        return Some(&raw[1..raw.len() - 1]);
                    }
                    if let Ok(s) = serde_json::from_slice::<SmolStr>(raw) {
                        buf.extend_from_slice(s.as_bytes());
                    } else {
                        buf.extend_from_slice(&raw[1..raw.len() - 1]);
                    }
                    return None;
                }
            }
        }
        b'0'..=b'9' => {
            if raw.len() <= 15 && raw.iter().all(|&b| b.is_ascii_digit()) {
                return Some(raw);
            }
        }
        _ => {}
    }
    if format_value_at(raw, 0, buf, tmp, raw_mode).is_none() {
        buf.extend_from_slice(b"null");
    }
    None
}

fn format_value_at(
    bytes: &[u8],
    pos: usize,
    buf: &mut Vec<u8>,
    tmp: &mut Vec<u8>,
    raw_mode: bool,
) -> Option<usize> {
    let i = skip_ws(bytes, pos);
    if i >= bytes.len() {
        return None;
    }
    match unsafe { *bytes.get_unchecked(i) } {
        b'n' => {
            buf.extend_from_slice(b"null");
            Some(i + 4)
        }
        b't' => {
            buf.extend_from_slice(b"true");
            Some(i + 4)
        }
        b'f' => {
            buf.extend_from_slice(b"false");
            Some(i + 5)
        }
        b'"' => {
            let (end, has_escape) = scan_json_string(bytes, i)?;
            if raw_mode {
                if !has_escape {
                    buf.extend_from_slice(unsafe { bytes.get_unchecked(i..end) });
                } else if let Ok(s) = serde_json::from_slice::<SmolStr>(&bytes[i..end]) {
                    write_json_str_vec(buf, s.as_str());
                } else {
                    buf.extend_from_slice(unsafe { bytes.get_unchecked(i..end) });
                }
            } else if !has_escape {
                buf.extend_from_slice(unsafe { bytes.get_unchecked(i + 1..end - 1) });
            } else if let Ok(s) = serde_json::from_slice::<SmolStr>(&bytes[i..end]) {
                buf.extend_from_slice(s.as_bytes());
            } else {
                buf.extend_from_slice(unsafe { bytes.get_unchecked(i + 1..end - 1) });
            }
            Some(end)
        }
        b'[' => {
            buf.push(b'[');
            let mut cur = skip_ws(bytes, i + 1);
            if cur < bytes.len() && unsafe { *bytes.get_unchecked(cur) } == b']' {
                buf.push(b']');
                return Some(cur + 1);
            }
            let mut first = true;
            loop {
                if !first {
                    buf.push(b',');
                }
                first = false;
                cur = format_value_at(bytes, cur, buf, tmp, raw_mode)?;
                cur = skip_ws(bytes, cur);
                if cur >= bytes.len() {
                    return None;
                }
                let b = unsafe { *bytes.get_unchecked(cur) };
                if b == b',' {
                    cur += 1;
                } else if b == b']' {
                    buf.push(b']');
                    return Some(cur + 1);
                } else {
                    return None;
                }
            }
        }
        b'{' => {
            let mut cur = skip_ws(bytes, i + 1);
            if cur < bytes.len() && unsafe { *bytes.get_unchecked(cur) } == b'}' {
                buf.extend_from_slice(b"{}");
                return Some(cur + 1);
            }

            let obj_out_start = buf.len();
            let mut stack_fields: [(&[u8], u32, u32, bool); 32] = [(&[], 0, 0, false); 32];
            let mut overflow_fields: Option<Vec<(&[u8], u32, u32, bool)>> = None;
            let mut count = 0usize;

            loop {
                cur = skip_ws(bytes, cur);
                if cur >= bytes.len() || unsafe { *bytes.get_unchecked(cur) } != b'"' {
                    return None;
                }
                let (key_end, key_esc) = scan_json_string(bytes, cur)?;
                let key_raw = unsafe { bytes.get_unchecked(cur + 1..key_end - 1) };
                cur = skip_ws(bytes, key_end);
                if cur >= bytes.len() || unsafe { *bytes.get_unchecked(cur) } != b':' {
                    return None;
                }
                cur += 1;

                let v_start = (buf.len() - obj_out_start) as u32;
                cur = format_value_at(bytes, cur, buf, tmp, raw_mode)?;
                let v_end = (buf.len() - obj_out_start) as u32;

                let entry = (key_raw, v_start, v_end, key_esc);
                if count < 32 {
                    stack_fields[count] = entry;
                } else {
                    let vec = overflow_fields.get_or_insert_with(|| {
                        let mut v = Vec::with_capacity(64);
                        v.extend_from_slice(&stack_fields);
                        v
                    });
                    vec.push(entry);
                }
                count += 1;

                cur = skip_ws(bytes, cur);
                if cur >= bytes.len() {
                    return None;
                }
                let b = unsafe { *bytes.get_unchecked(cur) };
                if b == b',' {
                    cur += 1;
                } else if b == b'}' {
                    cur += 1;
                    break;
                } else {
                    return None;
                }
            }

            let fields: &mut [(&[u8], u32, u32, bool)] = if let Some(ref mut vec) = overflow_fields
            {
                vec.as_mut_slice()
            } else {
                &mut stack_fields[..count]
            };

            if fields.len() == 2 {
                if fields[1].0 < fields[0].0 {
                    fields.swap(0, 1);
                }
            } else if fields.len() > 2 {
                fields.sort_unstable_by(|a, b| a.0.cmp(b.0));
            }

            tmp.clear();
            tmp.extend_from_slice(&buf[obj_out_start..]);
            buf.truncate(obj_out_start);

            buf.push(b'{');
            for (idx, &(key_raw, v_start, v_end, key_esc)) in fields.iter().enumerate() {
                if idx > 0 {
                    buf.push(b',');
                }
                if raw_mode {
                    if !key_esc {
                        buf.push(b'"');
                        buf.extend_from_slice(key_raw);
                        buf.push(b'"');
                    } else if let Ok(s) = serde_json::from_slice::<SmolStr>(
                        &bytes[key_raw.as_ptr() as usize - bytes.as_ptr() as usize - 1
                            ..key_raw.as_ptr() as usize - bytes.as_ptr() as usize + key_raw.len() + 1],
                    ) {
                        write_json_str_vec(buf, s.as_str());
                    } else {
                        buf.push(b'"');
                        buf.extend_from_slice(key_raw);
                        buf.push(b'"');
                    }
                } else if !key_esc {
                    buf.extend_from_slice(key_raw);
                } else if let Ok(s) = serde_json::from_slice::<SmolStr>(
                    &bytes[key_raw.as_ptr() as usize - bytes.as_ptr() as usize - 1
                        ..key_raw.as_ptr() as usize - bytes.as_ptr() as usize + key_raw.len() + 1],
                ) {
                    buf.extend_from_slice(s.as_bytes());
                } else {
                    buf.extend_from_slice(key_raw);
                }
                buf.push(b':');
                buf.extend_from_slice(&tmp[v_start as usize..v_end as usize]);
            }
            buf.push(b'}');
            Some(cur)
        }
        _ => {
            // Number
            let mut end = i;
            let mut all_digits = true;
            while end < bytes.len() {
                let b = unsafe { *bytes.get_unchecked(end) };
                if b.is_ascii_digit() {
                    end += 1;
                } else if matches!(b, b'-' | b'+' | b'.' | b'e' | b'E') {
                    all_digits = false;
                    end += 1;
                } else {
                    break;
                }
            }
            let num_slice = unsafe { bytes.get_unchecked(i..end) };
            if all_digits && !num_slice.is_empty() {
                if num_slice.len() <= 15 {
                    buf.extend_from_slice(num_slice);
                } else if num_slice.len() <= 19 {
                    let mut v: u64 = 0;
                    for &b in num_slice {
                        v = v.wrapping_mul(10).wrapping_add((b - b'0') as u64);
                    }
                    let _ = write!(buf, "{}", v as f64);
                } else if let Ok(n) = serde_json::from_slice::<Number>(num_slice) {
                    let _ = write!(buf, "{}", n.0);
                } else {
                    buf.extend_from_slice(num_slice);
                }
            } else if let Ok(n) = serde_json::from_slice::<Number>(num_slice) {
                let f = n.0;
                if f.fract() == 0.0
                    && f.abs() < 9_007_199_254_740_992.0
                    && (f != 0.0 || !f.is_sign_negative())
                {
                    let mut itoa_buf = itoa::Buffer::new();
                    buf.extend_from_slice(itoa_buf.format(f as i64).as_bytes());
                } else {
                    let _ = write!(buf, "{}", f);
                }
            } else {
                buf.extend_from_slice(num_slice);
            }
            Some(end)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon_str(raw: &[u8], raw_mode: bool) -> String {
        let mut buf = Vec::new();
        let mut tmp = Vec::new();
        if let Some(s) = format_or_borrow_canonical(raw, &mut buf, &mut tmp, raw_mode) {
            String::from_utf8(s.to_vec()).unwrap()
        } else {
            String::from_utf8(buf).unwrap()
        }
    }

    #[test]
    fn raw_mode_json_object_and_array() {
        let input: &'static str = r#"{"b":"hello","a":["x","y\"z",1,true,null],"empty":""}"#;
        let val: SmolValue = serde_json::from_str(input).unwrap();

        let mut normal_buf = Vec::new();
        val.write_to(&mut normal_buf).unwrap();
        assert_eq!(
            String::from_utf8(normal_buf).unwrap(),
            r#"{a:[x,y"z,1,true,null],b:hello,empty:}"#
        );
        assert_eq!(
            canon_str(input.as_bytes(), false),
            r#"{a:[x,y"z,1,true,null],b:hello,empty:}"#
        );

        let mut raw_buf = Vec::new();
        val.write_raw_to(&mut raw_buf).unwrap();
        let raw_out = String::from_utf8(raw_buf).unwrap();
        assert_eq!(
            raw_out,
            r#"{"a":["x","y\"z",1,true,null],"b":"hello","empty":""}"#
        );
        assert_eq!(canon_str(input.as_bytes(), true), raw_out);

        // Verify valid JSON parseable by serde_json
        let reparsed: serde_json::Value = serde_json::from_str(&raw_out).unwrap();
        assert_eq!(reparsed["b"], "hello");
        assert_eq!(reparsed["empty"], "");
    }

    #[test]
    fn raw_mode_top_level_primitives() {
        let s_in: &'static str = r#""192.168.1.1""#;
        let val: SmolValue = serde_json::from_str(s_in).unwrap();
        let mut raw_buf = Vec::new();
        val.write_raw_to(&mut raw_buf).unwrap();
        assert_eq!(String::from_utf8(raw_buf).unwrap(), "192.168.1.1");
        assert_eq!(canon_str(s_in.as_bytes(), true), "192.168.1.1");
    }
}
