use anyhow::Result;
use csv::{ReaderBuilder, StringRecord};
use hashbrown::hash_map::RawEntryMut;
use hashbrown::HashTable;
use std::ffi::OsStr;
use std::io::{BufRead, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::merge::AHashMap;
use crate::options::Options;
use crate::smolvalue::{format_or_borrow_canonical, SmolValue};
use crate::visitors::SelectiveVisitor;
use ahash::RandomState;
use rayon::prelude::*;
use scopeguard::defer;
use smol_str::SmolStr;

const NUM_SHARDS: usize = 64;
const SHARD_SHIFT: u32 = 64 - 6; // top 6 bits -> 0..64

// Buffer reused across filter calls to avoid repeated allocations.
thread_local! {
    static FILTER_BUF: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[derive(Debug, Default, Clone)]
struct AggregateMap(AHashMap<SmolValue, AggregateData>);

#[derive(Debug, Default, Clone)]
struct AggregateData {
    count: u64,
    buckets: Option<Box<AggregateMap>>,
}

/// Memory-maps a file if possible (zero-copy, instant setup), falling back to reading into a leaked Vec<u8>.
fn load_input_bytes(file: &Option<PathBuf>) -> Result<&'static [u8]> {
    match file {
        Some(path) => {
            let f = std::fs::File::open(path)?;
            let len = f.metadata()?.len() as usize;
            if len == 0 {
                return Ok(&[]);
            }
            #[cfg(unix)]
            {
                use std::os::unix::io::AsRawFd;
                let ptr = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        len,
                        libc::PROT_READ,
                        libc::MAP_PRIVATE,
                        f.as_raw_fd(),
                        0,
                    )
                };
                if ptr != libc::MAP_FAILED {
                    unsafe {
                        libc::madvise(ptr, len, libc::MADV_SEQUENTIAL);
                        libc::madvise(ptr, len, libc::MADV_WILLNEED);
                        return Ok(std::slice::from_raw_parts(ptr as *const u8, len));
                    }
                }
            }
            let mut buf = Vec::with_capacity(len);
            let mut f = f;
            f.read_to_end(&mut buf)?;
            Ok(Box::leak(buf.into_boxed_slice()))
        }
        None => {
            let mut buf = Vec::new();
            std::io::stdin().lock().read_to_end(&mut buf)?;
            Ok(Box::leak(buf.into_boxed_slice()))
        }
    }
}

fn split_into_chunks(slice: &'static [u8], target_chunks: usize) -> Vec<&'static [u8]> {
    if slice.is_empty() {
        return Vec::new();
    }
    let chunk_size = (slice.len() / target_chunks.max(1)).max(64 * 1024);
    let mut chunks = Vec::with_capacity(target_chunks + 2);
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
        chunks.push(&slice[pos..end]);
        pos = end;
    }
    chunks
}

/// Fast sharded pipeline for single-key JSON aggregation (`-k key` or `-k a.b.c`).
fn run_single_key_json(
    bytes: &'static [u8],
    visitor: &SelectiveVisitor,
    opt: &Options,
) -> Result<()> {
    let num_threads = rayon::current_num_threads();
    let chunks = split_into_chunks(bytes, num_threads * 16);
    let hasher_state = RandomState::with_seeds(
        0x517cc1b727220a95,
        0x9e3779b97f4a7c15,
        0xbf58476d1ce4e5b9,
        0x94d049bb133111eb,
    );

    // Phase 1: Work-stealing parallel chunk scan into per-thread sharded raw-slice tables.
    let next_chunk = AtomicUsize::new(0);
    let worker_results: Vec<(Vec<HashTable<(u64, &'static [u8], u64)>>, u64)> = (0..num_threads)
        .into_par_iter()
        .map(|_| {
            let mut shards: Vec<HashTable<(u64, &'static [u8], u64)>> =
                (0..NUM_SHARDS).map(|_| HashTable::with_capacity(1024)).collect();
            let mut local_entries = 0u64;

            loop {
                let idx = next_chunk.fetch_add(1, Ordering::Relaxed);
                if idx >= chunks.len() {
                    break;
                }
                let chunk = chunks[idx];
                let mut line_start = 0usize;

                for nl_pos in memchr::memchr_iter(b'\n', chunk) {
                    let mut line = unsafe { chunk.get_unchecked(line_start..nl_pos) };
                    line_start = nl_pos + 1;
                    if !line.is_empty() && unsafe { *line.get_unchecked(line.len() - 1) } == b'\r' {
                        line = unsafe { line.get_unchecked(..line.len() - 1) };
                    }
                    if line.is_empty() || unsafe { *line.get_unchecked(0) } == b'#' {
                        continue;
                    }

                    let raw = visitor.extract_raw_single(line);
                    let hash = hasher_state.hash_one(raw);
                    let shard_idx = (hash >> SHARD_SHIFT) as usize;
                    let table = unsafe { shards.get_unchecked_mut(shard_idx) };

                    match table.entry(
                        hash,
                        |&(h, k, _)| h == hash && k == raw,
                        |&(h, _, _)| h,
                    ) {
                        hashbrown::hash_table::Entry::Occupied(mut e) => {
                            e.get_mut().2 += 1;
                        }
                        hashbrown::hash_table::Entry::Vacant(v) => {
                            v.insert((hash, raw, 1));
                        }
                    }
                    local_entries += 1;
                }

                // Handle trailing line without newline
                if line_start < chunk.len() {
                    let mut line = unsafe { chunk.get_unchecked(line_start..) };
                    if !line.is_empty() && unsafe { *line.get_unchecked(line.len() - 1) } == b'\r' {
                        line = unsafe { line.get_unchecked(..line.len() - 1) };
                    }
                    if !line.is_empty() && unsafe { *line.get_unchecked(0) } != b'#' {
                        let raw = visitor.extract_raw_single(line);
                        let hash = hasher_state.hash_one(raw);
                        let shard_idx = (hash >> SHARD_SHIFT) as usize;
                        let table = unsafe { shards.get_unchecked_mut(shard_idx) };

                        match table.entry(
                            hash,
                            |&(h, k, _)| h == hash && k == raw,
                            |&(h, _, _)| h,
                        ) {
                            hashbrown::hash_table::Entry::Occupied(mut e) => {
                                e.get_mut().2 += 1;
                            }
                            hashbrown::hash_table::Entry::Vacant(v) => {
                                v.insert((hash, raw, 1));
                            }
                        }
                        local_entries += 1;
                    }
                }
            }

            (shards, local_entries)
        })
        .collect();

    // Phase 2: Merge each of the 64 raw shards in parallel across all cores,
    // format unique raw JSON slices into canonical SmolValue display strings,
    // and partition into 64 canonical shards.
    let filter_ref = opt.filter.as_ref();
    let raw_shard_outputs: Vec<(Vec<Vec<(u64, &'static [u8], u64)>>, u64)> = (0..NUM_SHARDS)
        .into_par_iter()
        .map(|shard_id| {
            let total_cap: usize = worker_results
                .iter()
                .map(|(s, _)| s[shard_id].len())
                .sum();
            let mut merged_raw: HashTable<(u64, &'static [u8], u64)> =
                HashTable::with_capacity(total_cap);

            for (worker_shards, _) in &worker_results {
                for &(hash, raw, count) in &worker_shards[shard_id] {
                    match merged_raw.entry(
                        hash,
                        |&(h, k, _)| h == hash && k == raw,
                        |&(h, _, _)| h,
                    ) {
                        hashbrown::hash_table::Entry::Occupied(mut e) => {
                            e.get_mut().2 += count;
                        }
                        hashbrown::hash_table::Entry::Vacant(v) => {
                            v.insert((hash, raw, count));
                        }
                    }
                }
            }

            let mut arena_buf: Vec<u8> = Vec::with_capacity(merged_raw.len() * 96);
            let mut tmp_buf: Vec<u8> = Vec::with_capacity(4096);
            // Store (BorrowedSlice or ArenaRange, count)
            enum SliceOrRange {
                Borrowed(&'static [u8]),
                Range(usize, usize),
            }
            let mut formatted_items: Vec<(SliceOrRange, u64)> =
                Vec::with_capacity(merged_raw.len());

            for &(_raw_hash, raw, count) in &merged_raw {
                let start = arena_buf.len();
                if let Some(borrowed) =
                    format_or_borrow_canonical(raw, &mut arena_buf, &mut tmp_buf, opt.raw)
                {
                    formatted_items.push((SliceOrRange::Borrowed(borrowed), count));
                } else {
                    let end = arena_buf.len();
                    formatted_items.push((SliceOrRange::Range(start, end), count));
                }
            }

            let leaked_arena: &'static [u8] = Box::leak(arena_buf.into_boxed_slice());
            let mut to_canon: Vec<Vec<(u64, &'static [u8], u64)>> = (0..NUM_SHARDS)
                .map(|_| Vec::with_capacity((formatted_items.len() / NUM_SHARDS) + 8))
                .collect();
            let mut matched_entries = 0u64;

            for (item, count) in formatted_items {
                let canon_slice: &'static [u8] = match item {
                    SliceOrRange::Borrowed(s) => s,
                    SliceOrRange::Range(start, end) => unsafe {
                        leaked_arena.get_unchecked(start..end)
                    },
                };

                if let Some(re) = filter_ref {
                    let s = unsafe { std::str::from_utf8_unchecked(canon_slice) };
                    if !re.is_match(s) {
                        continue;
                    }
                }

                matched_entries += count;
                let canon_hash = hasher_state.hash_one(canon_slice);
                let c_shard = (canon_hash >> SHARD_SHIFT) as usize;
                unsafe { to_canon.get_unchecked_mut(c_shard) }.push((
                    canon_hash,
                    canon_slice,
                    count,
                ));
            }

            (to_canon, matched_entries)
        })
        .collect();

    let total_entries: u64 = if filter_ref.is_some() {
        raw_shard_outputs.iter().map(|(_, e)| *e).sum()
    } else {
        worker_results.iter().map(|(_, e)| *e).sum()
    };

    // Phase 3: Deduplicate across the 64 canonical shards in parallel.
    let canon_shards: Vec<Vec<(&'static [u8], u64)>> = (0..NUM_SHARDS)
        .into_par_iter()
        .map(|c_shard| {
            let cap: usize = raw_shard_outputs
                .iter()
                .map(|(to_c, _)| to_c[c_shard].len())
                .sum();
            let mut table: HashTable<(u64, &'static [u8], u64)> = HashTable::with_capacity(cap);

            for (to_c, _) in &raw_shard_outputs {
                for &(hash, canon, count) in &to_c[c_shard] {
                    match table.entry(
                        hash,
                        |&(h, k, _)| h == hash && k == canon,
                        |&(h, _, _)| h,
                    ) {
                        hashbrown::hash_table::Entry::Occupied(mut e) => {
                            e.get_mut().2 += count;
                        }
                        hashbrown::hash_table::Entry::Vacant(v) => {
                            v.insert((hash, canon, count));
                        }
                    }
                }
            }

            table.into_iter().map(|(_, canon, count)| (canon, count)).collect()
        })
        .collect();

    let total_buckets: usize = canon_shards.iter().map(|s| s.len()).sum();
    let mut pairs: Vec<(&'static [u8], u64)> = Vec::with_capacity(total_buckets);
    for shard in canon_shards {
        pairs.extend(shard);
    }

    pairs.sort_unstable_by_key(|p| p.1);

    // Phase 4: Format output buffers in parallel and write to stdout.
    let chunk_len = (pairs.len() / (num_threads * 2).max(1)).max(4096);
    let formatted_chunks: Vec<Vec<u8>> = pairs
        .par_chunks(chunk_len)
        .map(|slice| {
            let mut out = Vec::with_capacity(slice.len() * 128);
            let mut itoa_buf = itoa::Buffer::new();
            for &(k, count) in slice {
                if opt.raw {
                    out.extend_from_slice(k);
                    out.push(b'\n');
                } else if opt.counters_to_right {
                    if opt.colors {
                        out.extend_from_slice(b"\x1b[34m");
                        out.extend_from_slice(k);
                        out.extend_from_slice(b"\x1b[0m -> ");
                    } else {
                        out.extend_from_slice(k);
                        out.extend_from_slice(b" -> ");
                    }
                    out.extend_from_slice(itoa_buf.format(count).as_bytes());
                    if opt.verbose {
                        let _ = writeln!(
                            out,
                            " ({:.2}%)",
                            (count as f64 / total_entries as f64) * 100.0
                        );
                    } else {
                        out.push(b'\n');
                    }
                } else {
                    out.extend_from_slice(itoa_buf.format(count).as_bytes());
                    if opt.verbose {
                        let _ = write!(
                            out,
                            " ({:.2}%)",
                            (count as f64 / total_entries as f64) * 100.0
                        );
                    }
                    if opt.colors {
                        out.extend_from_slice(b": \x1b[34m");
                        out.extend_from_slice(k);
                        out.extend_from_slice(b"\x1b[0m\n");
                    } else {
                        out.extend_from_slice(b": ");
                        out.extend_from_slice(k);
                        out.push(b'\n');
                    }
                }
            }
            out
        })
        .collect();

    {
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        for buf in &formatted_chunks {
            lock.write_all(buf)?;
        }
        lock.flush()?;
    }

    if !opt.raw {
        println!("buckets      : {}", total_buckets);
        println!("total entries: {}", total_entries);
    }

    std::mem::forget(worker_results);
    std::mem::forget(raw_shard_outputs);
    std::mem::forget(pairs);
    std::mem::forget(formatted_chunks);

    Ok(())
}

impl AggregateMap {
    fn new() -> Self {
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
        for (key, other_data) in other.0 {
            match self.0.raw_entry_mut().from_key(&key) {
                RawEntryMut::Occupied(mut entry) => {
                    let entry = entry.get_mut();
                    entry.count += other_data.count;
                    if let Some(other_buckets) = other_data.buckets {
                        if let Some(entry_buckets) = &mut entry.buckets {
                            entry_buckets.merge_owned(*other_buckets);
                        } else {
                            entry.buckets = Some(other_buckets);
                        }
                    }
                }
                RawEntryMut::Vacant(entry) => {
                    entry.insert(key, other_data);
                }
            }
        }
    }

    #[inline]
    fn insert(&mut self, values: Vec<SmolValue>) {
        let len = values.len();
        let mut map = Some(self);
        for (i, value) in values.into_iter().enumerate() {
            let is_last = i + 1 == len;
            let Some(m) = map else {
                unreachable!("impossible")
            };
            let tmp = m.0.entry(value).or_insert_with(|| AggregateData {
                count: 0,
                buckets: if is_last {
                    None
                } else {
                    Some(Box::new(AggregateMap::new()))
                },
            });
            tmp.count += 1;
            map = tmp.buckets.as_deref_mut();
        }
    }

    fn print_into<W: Write>(&self, level: usize, opt: &Options, out: &mut W) -> std::io::Result<()> {
        let mut pairs = Vec::with_capacity(self.0.len());
        let mut total_count: u64 = 0;

        for (k, v) in &self.0 {
            pairs.push((k, v));
            total_count += v.count;
        }

        pairs.sort_unstable_by_key(|a| a.1.count);

        const SPACES: &[u8] = b"                                                                ";
        let mut itoa_buf = itoa::Buffer::new();

        for (k, v) in pairs {
            if opt.raw {
                k.write_raw_to(out)?;
                out.write_all(b"\n")?;
            } else {
                let mut rem = level;
                while rem > 0 {
                    let n = rem.min(SPACES.len());
                    out.write_all(&SPACES[..n])?;
                    rem -= n;
                }

                if opt.counters_to_right {
                    if opt.colors {
                        out.write_all(b"\x1b[34m")?;
                        k.write_to(out)?;
                        out.write_all(b"\x1b[0m -> ")?;
                    } else {
                        k.write_to(out)?;
                        out.write_all(b" -> ")?;
                    }
                    out.write_all(itoa_buf.format(v.count).as_bytes())?;
                    if opt.verbose {
                        writeln!(
                            out,
                            " ({:.2}%)",
                            (v.count as f64 / total_count as f64) * 100.0
                        )?;
                    } else {
                        out.write_all(b"\n")?;
                    }
                } else {
                    out.write_all(itoa_buf.format(v.count).as_bytes())?;
                    if opt.verbose {
                        write!(
                            out,
                            " ({:.2}%)",
                            (v.count as f64 / total_count as f64) * 100.0
                        )?;
                    }
                    if opt.colors {
                        out.write_all(b": \x1b[34m")?;
                        k.write_to(out)?;
                        out.write_all(b"\x1b[0m\n")?;
                    } else {
                        out.write_all(b": ")?;
                        k.write_to(out)?;
                        out.write_all(b"\n")?;
                    }
                }
            }

            if let Some(buckets) = &v.buckets {
                buckets.print_into(level + 4, opt, out)?;
            }
        }
        Ok(())
    }

    fn aggregate(
        self,
        bytes: &'static [u8],
        visitor: &Aggregate,
        opt: &Options,
    ) -> Result<(AggregateMap, u64)> {
        let start_offset = if matches!(visitor, Aggregate::Csv(_)) {
            match memchr::memchr(b'\n', bytes) {
                Some(pos) => pos + 1,
                None => bytes.len(),
            }
        } else {
            0
        };

        let slice = &bytes[start_offset..];
        let num_threads = rayon::current_num_threads();
        let chunks = split_into_chunks(slice, num_threads * 4);

        let (map, total_entries) = chunks
            .into_par_iter()
            .map(|chunk| {
                let mut amap = AggregateMap::new();
                let mut local_entries = 0u64;
                let chunk_str: &'static str = unsafe { std::str::from_utf8_unchecked(chunk) };

                for mut line in chunk_str.split('\n') {
                    if line.ends_with('\r') {
                        line = &line[..line.len() - 1];
                    }
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }

                    let values = visitor.parse_values(line).unwrap();

                    if let Some(filter) = &opt.filter {
                        let matched = FILTER_BUF.with(|buf| {
                            let mut buf = buf.borrow_mut();
                            buf.clear();
                            for (i, v) in values.iter().enumerate() {
                                if i > 0 {
                                    buf.push(b' ');
                                }
                                v.write_to(&mut *buf).unwrap();
                            }
                            let s = unsafe { std::str::from_utf8_unchecked(&buf) };
                            filter.is_match(s)
                        });
                        if !matched {
                            continue;
                        }
                    }

                    local_entries += 1;
                    amap.insert(values);
                }

                (amap, local_entries)
            })
            .reduce(
                || (AggregateMap::new(), 0u64),
                |(mut amap_a, entries_a), (amap_b, entries_b)| {
                    amap_a.merge_owned(amap_b);
                    (amap_a, entries_a + entries_b)
                },
            );

        Ok((map, total_entries))
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
    /// Plain text: `None` aggregates whole lines, `Some` tokenizes them.
    Text(Option<Tokenizer>),
}

/// Plain-text tokenization settings (`-t`, `-s <SEP>`, `-k <COLUMN>`).
#[derive(Debug, Clone, Default)]
struct Tokenizer {
    /// Field separator; `None` splits on runs of whitespace.
    separator: Option<String>,
    /// 0-based columns to extract, in the requested order; empty means all tokens.
    columns: Vec<usize>,
    /// Highest requested column (only meaningful when `columns` is non-empty).
    max_column: usize,
}

impl Tokenizer {
    fn new(opt: &Options) -> Result<Self> {
        let separator = match opt.separator.as_deref() {
            None => None,
            Some("") => return Err(anyhow::anyhow!("separator must not be empty")),
            Some(s) => Some(s.replace("\\t", "\t")),
        };

        let columns = opt
            .keys
            .iter()
            .map(|k| match k.parse::<usize>() {
                Ok(n) if n >= 1 => Ok(n - 1),
                _ => Err(anyhow::anyhow!(
                    "invalid column '{}': in tokenize mode -k expects a column number starting from 1",
                    k
                )),
            })
            .collect::<Result<Vec<usize>>>()?;

        let max_column = columns.iter().copied().max().unwrap_or(0);

        Ok(Self {
            separator,
            columns,
            max_column,
        })
    }

    #[inline]
    fn tokenize(&self, line: &'static str) -> Vec<SmolValue> {
        match &self.separator {
            Some(sep) => self.select(line.split(sep.as_str())),
            None => self.select(line.split_whitespace()),
        }
    }

    /// Keeps all tokens, or only the requested columns (missing columns become `null`).
    #[inline]
    fn select<I: Iterator<Item = &'static str>>(&self, tokens: I) -> Vec<SmolValue> {
        let to_value = |s: &'static str| SmolValue::String(SmolStr::new_static(s));
        if self.columns.is_empty() {
            return tokens.map(to_value).collect();
        }
        let fields: Vec<&'static str> = tokens.take(self.max_column + 1).collect();
        self.columns
            .iter()
            .map(|&c| fields.get(c).map_or(SmolValue::Null, |&s| to_value(s)))
            .collect()
    }
}

impl Aggregate {
    fn new(opt: &Options) -> Result<Self> {
        match Self::get_parse_type(opt)? {
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
            ParserType::Text => {
                if opt.is_tokenize() {
                    Ok(Aggregate::Text(Some(Tokenizer::new(opt)?)))
                } else {
                    Ok(Aggregate::Text(None))
                }
            }
        }
    }

    fn get_parse_type(opt: &Options) -> Result<ParserType> {
        if opt.is_tokenize() {
            if opt.file_format.is_some() {
                return Err(anyhow::anyhow!(
                    "--file-format cannot be combined with --tokenize/--separator"
                ));
            }
            return Ok(ParserType::Text);
        }

        if opt.keys.is_empty() {
            return Ok(ParserType::Text);
        }

        if let Some(format) = &opt.file_format {
            match format.as_str() {
                "json" => Ok(ParserType::Json),
                "csv" => Ok(ParserType::Csv),
                _ => Err(anyhow::anyhow!("Invalid file format")),
            }
        } else if let Some(file) = &opt.file {
            let ext = file.extension();
            match ext.and_then(OsStr::to_str) {
                Some("csv") => Ok(ParserType::Csv),
                _ => Ok(ParserType::Json),
            }
        } else {
            Ok(ParserType::Json)
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
    fn parse_values(&self, line: &'static str) -> Result<Vec<SmolValue>> {
        match self {
            Aggregate::Json(visitor) => Ok(visitor.get_values_by_keys(line)?),
            Aggregate::Csv(pos) => {
                let mut res = Vec::with_capacity(pos.len());
                if memchr::memchr(b'"', line.as_bytes()).is_none() {
                    for &p in pos {
                        let s = line
                            .split(',')
                            .nth(p)
                            .ok_or_else(|| anyhow::anyhow!("Invalid CSV record"))?;
                        res.push(SmolValue::String(SmolStr::new_static(s)));
                    }
                } else {
                    let mut rdr = ReaderBuilder::new()
                        .has_headers(false)
                        .from_reader(line.as_bytes());

                    let record = rdr
                        .records()
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("Invalid CSV record"))??;

                    for &p in pos {
                        let s = record
                            .get(p)
                            .ok_or_else(|| anyhow::anyhow!("Invalid CSV record"))?;
                        res.push(SmolValue::String(SmolStr::new(s)));
                    }
                }

                Ok(res)
            }
            Aggregate::Text(None) => Ok(vec![SmolValue::String(SmolStr::new_static(line))]),
            Aggregate::Text(Some(tok)) => Ok(tok.tokenize(line)),
        }
    }
}

pub fn run(opt: Options) -> Result<()> {
    if let Some(nt) = opt.num_threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(nt)
            .build_global()?;
    }

    let raw = opt.raw;
    let start = std::time::Instant::now();
    defer! {
        if !raw {
            let elapsed = start.elapsed();
            println!("time elapsed : {:.2?}", elapsed);
        }
    }

    let visitor = Aggregate::new(&opt)?;
    let bytes = load_input_bytes(&opt.file)?;

    if let Aggregate::Json(ref jv) = visitor {
        if jv.is_single_key() && std::env::var("AGGRS_SLOW").is_err() {
            return run_single_key_json(bytes, jv, &opt);
        }
    }

    let (amap, entries) = AggregateMap::new().aggregate(bytes, &visitor, &opt)?;

    {
        let stdout = std::io::stdout();
        let mut out = BufWriter::with_capacity(256 * 1024, stdout.lock());
        amap.print_into(0, &opt, &mut out)?;
        out.flush()?;
    }

    if !raw {
        println!("buckets      : {}", amap.0.len());
        println!("total entries: {}", entries);
    }

    std::mem::forget(amap);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_parse_values_unquoted_and_quoted() {
        let visitor = Aggregate::Csv(vec![7, 0]);
        let line1: &'static str =
            "tcp,http,google,192.168.1.100,172.217.14.110,54321,80,15000,120,US,search";
        let vals1 = visitor.parse_values(line1).unwrap();
        assert_eq!(vals1[0].to_string(), "15000");
        assert_eq!(vals1[1].to_string(), "tcp");

        let line2: &'static str =
            r#"tcp,"http,2",google,192.168.1.100,172.217.14.110,54321,80,"25000",120,US,search"#;
        let vals2 = visitor.parse_values(line2).unwrap();
        assert_eq!(vals2[0].to_string(), "25000");
        assert_eq!(vals2[1].to_string(), "tcp");
    }

    fn tokenizer(separator: Option<&str>, keys: &[&str]) -> Result<Tokenizer> {
        let opt = Options {
            tokenize: separator.is_none(),
            separator: separator.map(str::to_string),
            keys: keys.iter().map(|k| k.to_string()).collect(),
            ..Default::default()
        };
        Tokenizer::new(&opt)
    }

    fn strings(values: Vec<SmolValue>) -> Vec<String> {
        values.into_iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn tokenize_whitespace_all_and_columns() {
        let line: &'static str = "GET  /index\t200";
        let all = tokenizer(None, &[]).unwrap();
        assert_eq!(strings(all.tokenize(line)), ["GET", "/index", "200"]);

        let cols = tokenizer(None, &["3", "1"]).unwrap();
        assert_eq!(strings(cols.tokenize(line)), ["200", "GET"]);
    }

    #[test]
    fn tokenize_custom_separator_keeps_empty_fields() {
        let line: &'static str = "a;;c";
        let tok = tokenizer(Some(";"), &["2", "3", "5"]).unwrap();
        assert_eq!(strings(tok.tokenize(line)), ["", "c", "null"]);

        let tab = tokenizer(Some("\\t"), &["2"]).unwrap();
        assert_eq!(strings(tab.tokenize("x y\tz")), ["z"]);

        let multi = tokenizer(Some("::"), &[]).unwrap();
        assert_eq!(strings(multi.tokenize("a::b:c")), ["a", "b:c"]);
    }

    #[test]
    fn tokenize_rejects_invalid_columns_and_empty_separator() {
        assert!(tokenizer(None, &["0"]).is_err());
        assert!(tokenizer(None, &["name"]).is_err());
        assert!(tokenizer(Some(""), &[]).is_err());
    }
}
