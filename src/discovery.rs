use hashbrown::hash_map::RawEntryMut;
use std::io::BufRead;

use crate::merge::AHashMap;
use ahash::RandomState;

use crate::smolvalue::SmolValue;
use crate::{options::Options, visitors::RegexVisitor};

use anyhow::Result;
use rayon::iter::ParallelBridge;
use rayon::prelude::*;
use std::io::{self, Write};

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

    fn merge_into(&mut self, other: &Self) {
        for (key, other_values) in &other.0 {
            match self.0.raw_entry_mut().from_key(key) {
                RawEntryMut::Occupied(mut entry) => {
                    let inner = entry.get_mut();
                    for (v, cnt) in other_values {
                        match inner.raw_entry_mut().from_key(v) {
                            RawEntryMut::Occupied(mut e) => {
                                *e.get_mut() += *cnt;
                            }
                            RawEntryMut::Vacant(e) => {
                                e.insert(v.clone(), *cnt);
                            }
                        }
                    }
                }
                RawEntryMut::Vacant(entry) => {
                    entry.insert(key.clone(), other_values.clone());
                }
            }
        }
    }

    pub fn print(&self, opt: &Options) {
        let mut pairs = Vec::with_capacity(self.0.len());

        for (key, values) in &self.0 {
            pairs.push((key, values));
        }

        pairs.sort_by(|a, b| a.1.len().cmp(&b.1.len()));

        let mut stdout = std::io::stdout();
        for (k, v) in pairs {
            let total = v.values().sum::<u64>();

            writeln!(stdout, "{}: {}", opt.colorize(k), total).unwrap();

            if opt.verbose {
                for (value, cnt) in v {
                    writeln!(stdout, "    {} -> {}", value, cnt).unwrap();
                }
            }
        }
    }

    pub fn discovery(self, opt: &Options) -> Result<DiscoveryMap> {
        let discovery = Discovery::new(opt)?;

        // Leak the entire input buffer so that SmolStr::new_static can
        // store zero-copy references in visit_borrowed_str.
        let v: &'static [String] = Box::leak(
            match &opt.file {
                Some(file) => io::BufReader::new(std::fs::File::open(file)?)
                    .lines()
                    .collect::<Result<Vec<_>, _>>()?,
                None => io::stdin().lock().lines().collect::<Result<Vec<_>, _>>()?,
            }
            .into_boxed_slice(),
        );

        let iter = v.iter();

        let map = iter
            .par_bridge()
            .fold(DiscoveryMap::default, |mut dmap, line| {
                if line.starts_with('#') {
                    return dmap;
                }

                let values = discovery.parse_key_value(line).unwrap();
                let filter = opt.discovery.as_ref().unwrap();

                let res = values
                    .into_iter()
                    .filter(|(_, value)| filter.is_match(&value.to_string()))
                    .collect::<Vec<(String, SmolValue)>>();

                for (key, value) in res {
                    let key_entry = dmap.0.entry(key).or_default();
                    let value_entry = key_entry.entry(value).or_default();
                    *value_entry += 1;
                }

                dmap
            })
            .reduce(DiscoveryMap::default, |mut dmap, b| {
                dmap.merge_into(&b);
                dmap
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

    dmap.print(&opt);

    println!("buckets      : {}", dmap.0.len());
    println!("time elapsed : {:.2?}", elapsed);

    Ok(())
}
