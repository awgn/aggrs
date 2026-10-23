# aggrs

A fast, multi-threaded command-line tool for building hierarchical aggregation trees from JSON and CSV data. Perfect for analyzing log files, network traffic data, and any structured data where you need to understand the distribution of values across multiple dimensions.

## Features

- 🚀 **Fast & Parallel**: Built with Rust, Rayon, zero-copy string slicing, and streaming selective JSON extraction for multi-gigabyte/s throughput
- 📊 **Hierarchical Aggregation**: Build nested aggregation trees with multiple flat or dotted keys (e.g. `-k info.dns.server`)
- 🔍 **Key Discovery**: Find which keys contain specific values in your data
- 📁 **Multiple Formats**: Supports JSON (newline-delimited), CSV, and plain text
- 🎨 **Colored Output**: Optional colored output for better readability
- 🔎 **Filtering**: Filter results using regular expressions
- 📈 **Statistics**: Optional percentage display for each bucket
- 🧩 **Raw Output**: Print only aggregated values without counters (`--raw`) for easy piping to tools like `jq`

## Installation

### From source

```bash
git clone https://github.com/awgn/aggrs.git
cd aggrs
cargo build --release
```

The binary will be available at `./target/release/aggrs`.

### From crates.io

```bash
cargo install aggrs
```

## Usage

```bash
aggrs [OPTIONS] [FILE]
```

If no file is specified, `aggrs` reads from stdin.

### Options

| Option | Description |
|--------|-------------|
| `-k, --keys <KEYS>` | Specify the JSON/CSV keys to aggregate (can be repeated, supports nested paths like `a.b.c`) |
| `-l, --level <LEVEL>` | Specify the aggregation level depth |
| `-c, --colors` | Enable colored output |
| `-v, --verbose` | Enable verbose mode (shows percentages) |
| `--counters-to-right` | Display counters to the right of bucket names |
| `--raw` | Print only values without counters (useful for piping to `jq`) |
| `-t, --tokenize` | Tokenize lines by whitespace (for plain text input) |
| `-f, --filter <FILTER>` | Filter buckets by regular expression |
| `-d, --discovery <DISCOVERY>` | Discover keys matching regex on values |
| `-j, --num-threads <NUM_THREADS>` | Specify the number of threads |
| `--file-format <FILE_FORMAT>` | Specify file format (`json` or `csv`) |
| `-h, --help` | Print help information |
| `-V, --version` | Print version |

## Examples

### JSON Aggregation

Given a JSON file with network flow data (`flows.json`):

```json
{"transport":"tcp","application":"http","service":"google","src_ip":"192.168.1.100"}
{"transport":"tcp","application":"https","service":"facebook","src_ip":"192.168.1.101"}
{"transport":"udp","application":"dns","service":"cloudflare","src_ip":"192.168.1.100"}
```

**Build a hierarchical aggregation tree:**

```bash
aggrs -k transport -k application -k service flows.json
```

Output:
```
1: udp
    1: dns
        1: cloudflare
2: tcp
    1: http
        1: google
    1: https
        1: facebook
buckets      : 2
total entries: 3
time elapsed : 0.42ms
```

**Aggregate nested JSON objects and sub-keys:**

```bash
aggrs -k info.dns.server -k info.dns.client flows.json
```

**With colored output and percentages:**

```bash
aggrs -k transport -k application flows.json -c -v
```

Output:
```
1 (33.33%): udp
    1 (100.00%): dns
2 (66.67%): tcp
    1 (50.00%): http
    1 (50.00%): https
```

**Display counters to the right:**

```bash
aggrs -k transport -k application flows.json --counters-to-right -v
```

Output:
```
udp -> 1 (33.33%)
    dns -> 1 (100.00%)
tcp -> 2 (66.67%)
    http -> 1 (50.00%)
    https -> 1 (50.00%)
```

### CSV Aggregation

Given a CSV file (`traffic.csv`):

```csv
transport,application,service,country,category
tcp,http,google,US,search
tcp,https,facebook,US,social
udp,dns,cloudflare,US,infrastructure
tcp,ssh,github,US,development
```

**Aggregate by category and application:**

```bash
aggrs -k category -k application traffic.csv
```

Output:
```
1: development
    1: ssh
1: infrastructure
    1: dns
1: search
    1: http
1: social
    1: https
```

### Key Discovery

Discover which keys contain a specific value pattern:

```bash
aggrs -d 'google' flows.json
```

Output:
```
service: 2
sni: 3
http_host: 1
buckets      : 3
time elapsed : 0.85ms
```

This is useful when you need to find which fields in your data contain a certain value.

### Filtering

Filter results to only show entries matching a pattern:

```bash
aggrs -k transport -k application -k service flows.json -f "https"
```

This will only aggregate entries where the combined key values match "https".

### Plain Text Mode

For non-JSON/CSV files, use tokenize mode:

```bash
cat access.log | aggrs -t
```

This splits each line by whitespace and treats each token as a separate level.

### Reading from stdin

```bash
cat data.json | aggrs -k field1 -k field2
```

Or pipe from other commands:

```bash
zcat compressed.json.gz | aggrs -k type -k status
```

### Multi-threaded Processing

For large files, specify the number of threads:

```bash
aggrs -k transport -k application large_file.json -j 8
```

## Output Format

The default output format shows:
- **Count**: Number of entries for each bucket
- **Bucket name**: The value of the key
- **Indentation**: Hierarchical level (4 spaces per level)

Results are sorted by count in ascending order within each level.

At the end, `aggrs` displays:
- `buckets`: Number of top-level buckets
- `total entries`: Total number of processed entries
- `time elapsed`: Processing time

## File Format Detection

`aggrs` automatically detects the file format:
- Files ending in `.csv` are treated as CSV
- All other files are treated as JSON (newline-delimited)
- Use `--file-format` to override automatic detection

## Performance Architecture

- **Contiguous Zero-Copy Input**: Reads input into a single contiguous buffer and partitions lines across Rayon worker chunks using SIMD `memchr`, avoiding per-line heap allocations and channel contention.
- **Streaming Multi-Branch JSON Extraction**: Navigates nested JSON key paths (`a.b.c`) directly in the `serde` visitor, skipping unrelated sibling objects and fields via `IgnoredAny` without allocating intermediate maps.
- **Compact Value Representation**: Uses zero-copy static `SmolStr` slices and flat sorted `Vec<(SmolStr, SmolValue)>` objects with `ahash` and `hashbrown` for low-overhead hashing and move-based parallel tree reduction.
- **Buffered Direct Output**: Formats numbers and values directly into a 256 KB `BufWriter` using `itoa` and `zmij`.

## Use Cases

- **Network Traffic Analysis**: Aggregate flows by protocol, application, and service
- **Log Analysis**: Group log entries by severity, source, and type
- **Data Exploration**: Discover patterns and distributions in structured data
- **Security Analysis**: Identify anomalies by aggregating event types and sources
- **Business Intelligence**: Summarize transactions by category, region, and time

## License

MIT License

## Author

Nicola Bonelli <nicola.bonelli@larthia.com>
