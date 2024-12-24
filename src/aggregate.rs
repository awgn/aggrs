use std::{collections::HashMap, io::BufRead};
use anyhow::Result;

use crate::options::{Options, AggrKeys};
use serde_json::Value;
use std::io::Write;
use colored::Colorize;

#[derive(Debug)]
struct AggregateMap(HashMap<Value, AggregateData>);

#[derive(Debug)]
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
            let Some(m) = map else { unreachable!("impossible")};
            let tmp = m.0.entry(value).or_insert(AggregateData {
                count: 0,
                buckets: Some(Box::new(AggregateMap::new())),
            });
            tmp.count += 1;
            map = tmp.buckets.as_deref_mut();
        }
    }

    fn print(&self, level : i32, opt: &Options)  {
        let mut pairs = Vec::with_capacity(self.0.len());

        let mut total_count : u64 = 0;

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
                    writeln!(stdout, " ({:.2}%)", (v.count as f64 / total_count as f64) * 100.0).unwrap();
                }

            } else {
                write!(stdout, "{}", v.count).unwrap();
                if opt.verbose {
                    write!(stdout, " ({:.2}%)", (v.count as f64 / total_count as f64) * 100.0).unwrap();
                }
                writeln!(stdout, ": {}", k).unwrap();
            }

            if let Some(buckets) = &v.buckets {
                buckets.print(level + 4, opt);
            }
        }
    }
}

#[inline]
fn colorize(s: &str, opt: &Options) -> String {
    if opt.colors {
        s.blue().to_string()
    } else {
        s.to_string()
    }
}

pub fn aggregate(opt: Options) -> Result<()> {
    let keys = AggrKeys::new(&opt)?;

    let mut aggr_map = AggregateMap::new();
    let mut entries = 0;

    let iter_line: Box<dyn Iterator<Item = _>> =
        if let Some(ref file) = opt.file {
            // iterate over the file, line by line...
            let f = std::fs::File::open(file)?;
            Box::new(std::io::BufReader::new(f).lines())
        } else {
            Box::new(std::io::stdin().lock().lines())
        };

    for line in iter_line {
        let line = line?;
        if line.starts_with('#') {
            continue;
        }

        entries += 1;
        let values = parse_line(line, &keys)?;
        aggr_map.insert(values);
    }

    aggr_map.print(0, &opt);

    println!("buckets: {}", aggr_map.0.len());
    println!("total entries: {}", entries);

    Ok(())
}


#[inline]
fn parse_line(line: String, keys: &AggrKeys) -> Result<Vec<Value>> {
    match &keys {
        AggrKeys::Keys(keys) => {
            let v : Value = serde_json::from_str(&line)?;
            Ok(keys.iter().map(|k| v[k].clone()).collect())
        },
        AggrKeys::Text(tok) => {
            if *tok {
                Ok(line.split_whitespace().map(|s| Value::String(s.to_string())).collect())
            } else {
                Ok(vec![Value::String(line)])
            }
        }
    }
}