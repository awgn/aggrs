use std::path::PathBuf;
use colored::Colorize;
use clap::Parser;

#[derive(Parser, Default, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Options {
    #[clap(short, long, help = "Specify the aggregation level")]
    pub level: Option<u64>,

    #[clap(short, long, help = "Enable colors")]
    pub colors: bool,

    #[clap(long, help = "Display counters to the right of bucket names")]
    pub counters_to_right: bool,

    #[clap(short, long, help = "Specify the JSON/CSV keys to aggregate")]
    pub keys : Vec<String>,

    #[clap(short, long, help = "Tokenise lines (used for non JSON input)")]
    pub tokenise:  bool,

    #[clap(short, long, help = "Filter buckets by regular expression")]
    pub filter: Option<regex::Regex>,

    #[clap(short, long, help = "Discovery keys matching regular expression on values")]
    pub discovery: Option<regex::Regex>,

    #[clap(short = 'j', long, help = "Specify the number of threads")]
    pub num_threads: Option<usize>,

    #[clap(long, help = "Specify the file format (json, csv)")]
    pub file_format: Option<String>,

    #[clap(short, long, help = "Enable verbose mode")]
    pub verbose: bool,

    pub file: Option<PathBuf>,
}

impl Options {
    #[inline]
    pub fn colorize(&self, s: &str) -> String {
        if self.colors {
            s.blue().to_string()
        } else {
            s.to_string()
        }
    }
}