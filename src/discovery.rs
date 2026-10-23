use hashbrown::hash_map::RawEntryMut;
use std::io::{BufWriter, Read, Write};

use crate::merge::AHashMap;
use ahash::RandomState;

use crate::smolvalue::SmolValue;
use crate::{options::Options, visitors::RegexVisitor};

use anyhow::Result;
use rayon::prelude::*;

#[derive(Debug, Clone)]
enum Discovery {
    Json(RegexVisitor),
}

impl Discovery {
    pub fn new(opt: &Options) -> Result<Self> {
        if let Some(expr) = &opt.discovery {
            Ok(Discovery::Json(RegexVisitor::new(expr.clone())))
        } else {
            Err(anyhow::anyhow!(
                "Invalid options (learn expression not specified)"
            ))
        }
    }

    #[inline]
    pub fn parse_key_value(&self, line: &str) -> Result<Vec<(String, SmolValue)>> {
        match self {
            Discovery::Json(learn) => Ok(learn.get_keys_by_regex(line)?),
        }
    }
}

#[derive(Debug, Default)]
struct DiscoveryMap(AHashMap<String, AHashMap<SmolValue, u64>>);

impl DiscoveryMap {
    pub fn new() -> Self {
        Self(AHashMap::with_hasher(RandomState::new()))
    }

    fn merge_owned(&mut self, mut other: Self) {
        if self.0.is_empty() {
            *self = other;
            return;
        }
        if other.0.len() > self.0.len() {
            std::mem::swap(self, &mut other);
        }
        self.0.reserve(other.0.len());
        for (key, other_values) in other.0 {
            match self.0.raw_entry_mut().from_key(&key) {
                RawEntryMut::Occupied(mut entry) => {
                    let inner = entry.get_mut();
                    inner.reserve(other_values.len());
                    for (v, cnt) in other_values {
                        match inner.raw_entry_mut().from_key(&v) {
                            RawEntryMut::Occupied(mut e) => {
                                *e.get_mut() += cnt;
                            }
                            RawEntryMut::Vacant(e) => {
                                e.insert(v, cnt);
                            }
                        }
                    }
                }
                RawEntryMut::Vacant(entry) => {
                    entry.insert(key, other_values);
                }
            }
        }
    }

    pub fn print(&self, opt: &Options) -> std::io::Result<()> {
        let mut pairs = Vec::with_capacity(self.0.len());

        for (key, values) in &self.0 {
            pairs.push((key, values));
        }

        pairs.sort_unstable_by_key(|a| a.1.len());

        let stdout = std::io::stdout();
        let mut out = BufWriter::with_capacity(256 * 1024, stdout.lock());
        let mut itoa_buf = itoa::Buffer::new();

        for (k, v) in pairs {
            let total = v.values().sum::<u64>();

            writeln!(out, "{}: {}", opt.colorize(k), itoa_buf.format(total))?;

            if opt.verbose {
                for (value, cnt) in v {
                    out.write_all(b"    ")?;
                    value.write_to(&mut out)?;
                    out.write_all(b" -> ")?;
                    out.write_all(itoa_buf.format(*cnt).as_bytes())?;
                    out.write_all(b"\n")?;
                }
            }
        }
        out.flush()
    }

    pub fn discovery(self, opt: &Options) -> Result<DiscoveryMap> {
        let discovery = Discovery::new(opt)?;

        // Leak a single contiguous input buffer so that SmolStr::new_static can
        // store zero-copy references in visit_borrowed_str.
        let content: &'static str = Box::leak(
            match &opt.file {
                Some(file) => std::fs::read_to_string(file)?,
                None => {
                    let mut s = String::new();
                    std::io::stdin().lock().read_to_string(&mut s)?;
                    s
                }
            }
            .into_boxed_str(),
        );

        let slice = content.as_bytes();
        let num_threads = rayon::current_num_threads();
        let target_chunks = (num_threads * 4).max(1);
        let chunk_size = (slice.len() / target_chunks).max(64 * 1024);

        let mut chunks: Vec<&'static str> = Vec::with_capacity(target_chunks + 1);
        let mut pos = 0;
        while pos < slice.len() {
            let end = if pos + chunk_size >= slice.len() {
                slice.len()
            } else {
                match memchr::memchr(b'\n', &slice[pos + chunk_size..]) {
                    Some(nl) => pos + chunk_size + nl + 1,
                    None => slice.len(),
                }
            };
            let chunk_str = unsafe { std::str::from_utf8_unchecked(&slice[pos..end]) };
            chunks.push(chunk_str);
            pos = end;
        }

        let map = chunks
            .into_par_iter()
            .map(|chunk| {
                let mut dmap = DiscoveryMap::new();
                for mut line in chunk.split('\n') {
                    if line.ends_with('\r') {
                        line = &line[..line.len() - 1];
                    }
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }

                    let values = discovery.parse_key_value(line).unwrap();
                    for (key, value) in values {
                        let key_entry = dmap.0.entry(key).or_default();
                        let value_entry = key_entry.entry(value).or_default();
                        *value_entry += 1;
                    }
                }
                dmap
            })
            .reduce(DiscoveryMap::new, |mut dmap_a, dmap_b| {
                dmap_a.merge_owned(dmap_b);
                dmap_a
            });

        Ok(map)
    }
}

pub fn run(opt: Options) -> Result<()> {
    if let Some(nt) = opt.num_threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(nt)
            .build_global()?;
    }

    let start = std::time::Instant::now();
    let dmap = DiscoveryMap::new().discovery(&opt)?;
    let elapsed = start.elapsed();

    dmap.print(&opt)?;

    println!("buckets      : {}", dmap.0.len());
    println!("time elapsed : {:.2?}", elapsed);

    Ok(())
}
