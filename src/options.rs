use clap::Parser;
use colored::Colorize;
use std::path::PathBuf;

#[derive(Parser, Default, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Options {
    #[clap(short, long, help = "Specify the aggregation level")]
    pub level: Option<u64>,

    #[clap(short, long, help = "Enable colors")]
    pub colors: bool,

    #[clap(long, help = "Display counters to the right of bucket names")]
    pub counters_to_right: bool,

    #[clap(
        short,
        long,
        help = "Specify the JSON/CSV keys to aggregate (column numbers starting from 1 in tokenize mode)"
    )]
    pub keys: Vec<String>,

    #[clap(short, long, help = "Tokenize lines by whitespace (used for non JSON input)")]
    pub tokenize: bool,

    #[clap(
        short,
        long,
        help = "Tokenize lines using the given separator (implies --tokenize, \\t for tab)"
    )]
    pub separator: Option<String>,

    #[clap(short, long, help = "Filter buckets by regular expression")]
    pub filter: Option<regex::Regex>,

    #[clap(
        short,
        long,
        help = "Discovery keys matching regular expression on values"
    )]
    pub discovery: Option<regex::Regex>,

    #[clap(short = 'j', long, help = "Specify the number of threads")]
    pub num_threads: Option<usize>,

    #[clap(long, help = "Specify the file format (json, csv)")]
    pub file_format: Option<String>,

    #[clap(short, long, help = "Enable verbose mode")]
    pub verbose: bool,

    #[clap(long, help = "Print only values without counters (useful for piping to jq)")]
    pub raw: bool,

    pub file: Option<PathBuf>,
}

impl Options {
    /// True when plain-text tokenization is requested (`-t` or `-s`).
    #[inline]
    pub fn is_tokenize(&self) -> bool {
        self.tokenize || self.separator.is_some()
    }

    #[inline]
    pub fn colorize(&self, s: &str) -> String {
        if self.colors {
            s.blue().to_string()
        } else {
            s.to_string()
        }
    }
}
