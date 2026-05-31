use anyhow::Result;
use csv::{ReaderBuilder, StringRecord};
use hashbrown::hash_map::RawEntryMut;
use std::ffi::OsStr;
use std::io::BufRead;
use std::path::PathBuf;

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::merge::{AHashMap, Merge};
use crate::options::Options;
use crate::smolvalue::SmolValue;
use crate::visitors::SelectiveVisitor;
use ahash::RandomState;
use rayon::prelude::*;
use scopeguard::defer;
use smol_str::SmolStr;

// Buffer reused across filter calls to avoid repeated allocations.
thread_local! {
    static FILTER_BUF: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

#[derive(Debug, Default, Clone)]
struct AggregateMap(AHashMap<SmolValue, AggregateData>);

#[derive(Debug, Default, Clone)]
struct AggregateData {
    count: u64,
    buckets: Option<Box<AggregateMap>>,
}

impl Merge for AggregateMap {
    fn merge(&mut self, other: &Self) {
        for (key, other_data) in &other.0 {
            match self.0.raw_entry_mut().from_key(key) {
                RawEntryMut::Occupied(mut entry) => {
                    let entry = entry.get_mut();
                    entry.count += other_data.count;
                    if let Some(other_buckets) = &other_data.buckets {
                        if let Some(entry_buckets) = &mut entry.buckets {
                            entry_buckets.merge(other_buckets.as_ref());
                        } else {
                            entry.buckets = Some(other_buckets.clone());
                        }
                    }
                }
                RawEntryMut::Vacant(entry) => {
                    entry.insert(key.clone(), other_data.clone());
                }
            }
        }
    }
}

impl AggregateMap {
    fn new() -> Self {
        Self(AHashMap::with_hasher(RandomState::new()))
    }

    fn insert(&mut self, values: Vec<SmolValue>) {
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
                write!(stdout, "{} -> {}", opt.colorize(&k.to_string()), v.count).unwrap();
                if opt.verbose {
                    writeln!(
                        stdout,
                        " ({:.2}%)",
                        (v.count as f64 / total_count as f64) * 100.0
                    )
                    .unwrap();
                } else {
                    writeln!(stdout).unwrap();
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
                writeln!(stdout, ": {}", opt.colorize(&k.to_string())).unwrap();
            }

            if let Some(buckets) = &v.buckets {
                buckets.print(level + 4, opt);
            }
        }
    }

    fn aggregate(self, opt: &Options) -> Result<(AggregateMap, u64)> {
        let entries = AtomicU64::new(0);

        let visitor = Aggregate::new(opt)?;

        // Leak the entire input buffer so that SmolStr::new_static can
        // store zero-copy references in visit_borrowed_str.
        let v: &'static [String] = Box::leak(
            match &opt.file {
                Some(file) => std::io::BufReader::new(std::fs::File::open(file)?)
                    .lines()
                    .collect::<Result<Vec<_>, _>>()?,
                None => std::io::stdin()
                    .lock()
                    .lines()
                    .collect::<Result<Vec<_>, _>>()?,
            }
            .into_boxed_slice(),
        );

        let mut iter = v.iter();

        // skip the first line if it's a CSV header
        if matches!(visitor, Aggregate::Csv(_)) {
            iter.next();
        }

        let map = iter
            .par_bridge()
            .fold(AggregateMap::default, |mut amap, line| {
                if line.starts_with('#') {
                    return amap;
                }

                let values = visitor.parse_values(line).unwrap();

                if let Some(filter) = &opt.filter {
                    let matched = FILTER_BUF.with(|buf| {
                        let mut buf = buf.borrow_mut();
                        buf.clear();
                        for (i, v) in values.iter().enumerate() {
                            if i > 0 {
                                buf.push(' ');
                            }
                            use std::fmt::Write;
                            write!(buf, "{}", v).unwrap();
                        }
                        filter.is_match(&buf)
                    });
                    if !matched {
                        return amap;
                    }
                }

                entries.fetch_add(1, Ordering::Relaxed);
                amap.insert(values);
                amap
            })
            .reduce(AggregateMap::default, |mut amap, b| {
                amap.merge(&b);
                amap
            });

        Ok((map, entries.load(Ordering::Relaxed)))
    }
}

#[derive(Debug, Clone)]
enum ParserType {
    Json,
    Csv,
    Text,
}

#[derive(Debug, Clone)]
enum Aggregate {
    Json(SelectiveVisitor),
    Csv(Vec<usize>),
    Text(bool),
}

impl Aggregate {
    fn new(opt: &Options) -> Result<Self> {
        let par_type = if opt.keys.is_empty() {
            ParserType::Text
        } else if let Some(format) = &opt.file_format {
            match format.as_str() {
                "json" => ParserType::Json,
                "csv" => ParserType::Csv,
                _ => return Err(anyhow::anyhow!("Invalid file format")),
            }
        } else {
            // try deduce format from file extension
            if let Some(file) = &opt.file {
                let ext = file.extension();
                match ext.and_then(OsStr::to_str) {
                    Some("csv") => ParserType::Csv,
                    _ => ParserType::Json, // default to json
                }
            } else {
                ParserType::Json
            }
        };

        match par_type {
            ParserType::Json => Ok(Aggregate::Json(SelectiveVisitor::new(opt.keys.clone()))),
            ParserType::Csv => {
                let record = Self::get_string_record(
                    opt.file
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("file name not specified"))?,
                )?;

                let pos = opt
                    .keys
                    .iter()
                    .map(|k| {
                        record
                            .iter()
                            .position(|r| r == k)
                            .ok_or_else(|| anyhow::anyhow!("key not found"))
                    })
                    .collect::<Result<Vec<usize>, _>>()?;

                Ok(Aggregate::Csv(pos))
            }
            ParserType::Text => Ok(Aggregate::Text(opt.tokenize)),
        }
    }

    fn get_string_record(file: &PathBuf) -> Result<StringRecord> {
        let file = std::fs::File::open(file)?;
        let mut reader = std::io::BufReader::new(file);
        let mut line = String::new();
        reader.read_line(&mut line)?;

        let mut rdr = ReaderBuilder::new()
            .has_headers(true)
            .from_reader(line.as_bytes());
        Ok(rdr.headers()?.iter().map(|h| h.to_string()).collect())
    }

    #[inline]
    fn parse_values(&self, line: &str) -> Result<Vec<SmolValue>> {
        match self {
            Aggregate::Json(visitor) => Ok(visitor.get_values_by_keys(line)?),
            Aggregate::Csv(pos) => {
                let mut rdr = ReaderBuilder::new()
                    .has_headers(false)
                    .from_reader(line.as_bytes());

                let mut res = Vec::with_capacity(pos.len());

                let record = rdr
                    .records()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("Invalid CSV record"))??;

                for p in pos {
                    // SAFETY: the input buffer is leaked as &'static, so
                    // all &str slices are effectively 'static.
                    let s: &'static str = unsafe { std::mem::transmute(record.get(*p).unwrap()) };
                    res.push(SmolValue::String(SmolStr::new_static(s)));
                }

                Ok(res)
            }
            Aggregate::Text(tok) => {
                if *tok {
                    Ok(line
                        .split_whitespace()
                        .map(|s| {
                            // SAFETY: input buffer is &'static
                            let s: &'static str = unsafe { std::mem::transmute(s) };
                            SmolValue::String(SmolStr::new_static(s))
                        })
                        .collect())
                } else {
                    // SAFETY: input buffer is &'static
                    let s: &'static str = unsafe { std::mem::transmute(line) };
                    Ok(vec![SmolValue::String(SmolStr::new_static(s))])
                }
            }
        }
    }
}

pub fn run(opt: Options) -> Result<()> {
    if let Some(nt) = opt.num_threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(nt)
            .build_global()?;
    }

    let start = std::time::Instant::now();
    defer! {
        let elapsed = start.elapsed();
        println!("time elapsed : {:.2?}", elapsed);
    }

    let (amap, entries) = AggregateMap::new().aggregate(&opt)?;

    amap.print(0, &opt);

    println!("buckets      : {}", amap.0.len());
    println!("total entries: {}", entries);

    Ok(())
}
