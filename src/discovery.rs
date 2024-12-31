use std::{collections::HashMap, io::BufRead};

use crate::{options::Options, visitors::RegexVisitor};
use crate::merge::*;

use rayon::iter::ParallelBridge;
use serde_json::Value;
use anyhow::Result;
use rayon::prelude::*;
use std::io::Write;

#[derive(Debug, Clone)]
enum Discovery {
    Json(RegexVisitor),
}

impl Discovery {
    pub fn new(opt: &Options) -> Result<Self> {
        if let Some(expr)  = &opt.discovery {
            Ok(Discovery::Json(RegexVisitor::new(expr.clone())))
        } else {
            Err(anyhow::anyhow!("Invalid options (learn expression not specified)"))
        }
    }

    #[inline]
    pub fn parse_key_value(&self, line: &str) -> Result<Vec<(String, Value)>> {
        match self {
            Discovery::Json(learn) =>  Ok(learn.clone().get_keys_by_regex(line)?),
        }
    }

}

#[derive(Debug, Default)]
struct DiscoveryMap(HashMap<String, HashMap<Value, u64>>);

impl Merge for DiscoveryMap {
    fn merge(&mut self, other: Self) {
        for (key, other_values) in other.0 {
            let entry = self.0.entry(key).or_default();
            entry.merge(other_values);
        }
    }
}


impl DiscoveryMap {
    pub fn new() -> Self {
        Self(HashMap::new())
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

        let v = match &opt.file {
            Some(file) => {
                std::io::BufReader::new(std::fs::File::open(file)?)
                .lines()
                .collect::<Result<Vec<_>, _>>()?
            },
            None => {
                std::io::stdin().lock().lines().collect::<Result<Vec<_>, _>>()?
            }
        };

        let iter = v.iter();

        let map = iter
            .par_bridge()
            .fold(DiscoveryMap::default, |mut dmap, line| {
                if line.starts_with('#') {
                    return dmap;
                }

                let values = discovery.parse_key_value(line).unwrap();
                let filter = opt.discovery.as_ref().unwrap();

                let res = values.into_iter().filter(|(_, value)| {
                        filter.is_match(&value.to_string())
                }).collect::<Vec<(String,Value)>>();

                // insert the key values into the map, if the key is not present, create a new HashSet.

                for (key, value) in res {
                    let key_entry = dmap.0.entry(key).or_default();
                    let value_entry = key_entry.entry(value).or_default();
                    *value_entry += 1;
                }

                dmap
            })
            .reduce(DiscoveryMap::default, |mut dmap, b| {
                dmap.merge(b);
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