use anyhow::Result;
use serde::de::{MapAccess, Visitor};
use std::collections::HashSet;
use std::fmt;
use std::{collections::HashMap, io::BufRead};

use crate::options::{AggrKeys, Options};
use colored::Colorize;
use rayon::prelude::*;
use serde_json::Value;

use serde::Deserializer;

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
struct AggregateMap(HashMap<Value, AggregateData>);

#[derive(Debug, Default)]
struct AggregateData {
    count: u64,
    buckets: Option<Box<AggregateMap>>,
}

impl AggregateMap {
    fn new() -> Self {
        Self(HashMap::new())
    }

    fn insert(&mut self, values: Vec<Value>) {
        let mut map = Some(self);
        for value in values {
            let Some(m) = map else {
                unreachable!("impossible")
            };
            let tmp = m.0.entry(value).or_insert(AggregateData {
                count: 0,
                buckets: Some(Box::new(AggregateMap::new())),
            });
            tmp.count += 1;
            map = tmp.buckets.as_deref_mut();
        }
    }

    fn merge(&mut self, other: AggregateMap) {
        for (key, other_data) in other.0 {
            let entry = self.0.entry(key).or_insert_with(|| AggregateData {
                count: 0,
                buckets: Some(Box::new(AggregateMap::new())),
            });

            entry.count += other_data.count;

            if let Some(other_buckets) = other_data.buckets {
                if let Some(entry_buckets) = &mut entry.buckets {
                    entry_buckets.merge(*other_buckets);
                } else {
                    entry.buckets = Some(other_buckets);
                }
            }
        }
    }

    fn print(&self, level: i32, opt: &Options) {
        let mut pairs = Vec::with_capacity(self.0.len());
        let mut total_count: u64 = 0;

        for (k, v) in &self.0 {
            pairs.push((k, v));
            total_count += v.count;
        }

        pairs.sort_by(|a, b| a.1.count.cmp(&b.1.count));

        let mut stdout = std::io::stdout();
        for (k, v) in pairs {
            write!(stdout, "{}", " ".repeat(level as usize)).unwrap();
            if opt.counters_to_right {
                write!(stdout, "{} -> {}", colorize(&k.to_string(), opt), v.count).unwrap();
                if opt.verbose {
                    writeln!(
                        stdout,
                        " ({:.2}%)",
                        (v.count as f64 / total_count as f64) * 100.0
                    )
                    .unwrap();
                }
            } else {
                write!(stdout, "{}", v.count).unwrap();
                if opt.verbose {
                    write!(
                        stdout,
                        " ({:.2}%)",
                        (v.count as f64 / total_count as f64) * 100.0
                    )
                    .unwrap();
                }
                writeln!(stdout, ": {}", k).unwrap();
            }

            if let Some(buckets) = &v.buckets {
                buckets.print(level + 4, opt);
            }
        }
    }

    fn aggregate_file(self, file: String, opt: &Options) -> Result<(AggregateMap, u64)> {
        let keys = AggrKeys::new(opt)?;

        let entries = AtomicU64::new(0);
        let v = std::io::BufReader::new(std::fs::File::open(file)?)
            .lines()
            .collect::<Result<Vec<_>, _>>()?;

        let visitor = match &keys {
            AggrKeys::Keys(vec) => Some(SelectiveVisitor::new(vec.clone())),
            AggrKeys::Text(_) => None,
        };

        let map = v
            .par_iter()
            .fold(AggregateMap::default, |mut amap, line| {
                if line.starts_with('#') {
                    return amap;
                }
                entries.fetch_add(1, Ordering::Relaxed);

                let values = parse_line(line, visitor.clone(), &keys).unwrap();
                amap.insert(values);
                amap
            })
            .reduce(AggregateMap::default, |mut amap, b| {
                amap.merge(b);
                amap
            });

        Ok((map, entries.load(Ordering::Relaxed)))
    }

    fn aggregate_stdin(self, opt: &Options) -> Result<(AggregateMap, u64)> {
        let keys = AggrKeys::new(opt)?;

        let v = std::io::stdin().lock()
            .lines();

        let entries = AtomicU64::new(0);

        let visitor = match opt.keys.is_empty() {
            true => None,
            false => Some(SelectiveVisitor::new(opt.keys.clone())),
        };

        let map = v
            .fold(AggregateMap::default(), |mut amap, line| {
                let line = line.unwrap();
                if line.starts_with('#') {
                    return amap;
                }

                entries.fetch_add(1, Ordering::Relaxed);

                let values = parse_line(&line, visitor.clone(), &keys).unwrap();
                amap.insert(values);
                amap
            });

        Ok((map, entries.load(Ordering::Relaxed)))
    }
}


pub fn run(opt: Options) -> Result<()> {
    let (amap, entries) = match &opt.file {
        Some(file) => AggregateMap::new()
            .aggregate_file(file.clone(), &opt)?,
        None => AggregateMap::new()
            .aggregate_stdin(&opt)?
    };

    amap.print(0, &opt);

    println!("buckets      : {}", amap.0.len());
    println!("total entries: {}", entries);
    Ok(())
}


#[inline]
fn colorize(s: &str, opt: &Options) -> String {
    if opt.colors {
        s.blue().to_string()
    } else {
        s.to_string()
    }
}

#[inline]
fn parse_line(line: &str, visitor: Option<SelectiveVisitor>, keys: &AggrKeys) -> Result<Vec<Value>> {
    match &keys {
        AggrKeys::Keys(_) => {
            Ok(parse_selected_keys(line, visitor.unwrap())?)
        }
        AggrKeys::Text(tok) => {
            if *tok {
                Ok(line
                    .split_whitespace()
                    .map(|s| Value::String(s.to_string()))
                    .collect())
            } else {
                Ok(vec![Value::String(line.to_string().clone())])
            }
        }
    }
}

#[derive(Debug, Clone)]
struct SelectiveVisitor {
    keys: HashSet<String>,
    values: Vec<serde_json::Value>,
}

impl SelectiveVisitor {
    fn new(keys: Vec<String>) -> Self {
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
                    while access.next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?.is_some() {}
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

fn parse_selected_keys(
    json: &str,
    visitor: SelectiveVisitor,
) -> Result<Vec<serde_json::Value>, serde_json::Error> {
    let deserializer = &mut serde_json::Deserializer::from_str(json);
    let result = deserializer.deserialize_map(visitor)?;

    // Consume any remaining whitespace or trailing characters
    deserializer.end()?;

    Ok(result)
}
