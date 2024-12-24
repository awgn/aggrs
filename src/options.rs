use clap::Parser;

use anyhow::Result;

#[derive(Parser, Default, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Options {
    #[clap(short, long, help = "Specify the aggregation level")]
    pub level: Option<u64>,

    #[clap(short, long, help = "Enable colors")]
    pub colors: bool,

    #[clap(long, help = "Display counters to the right of bucket names")]
    pub counters_to_right: bool,

    #[clap(short, long, help = "Specify the JSON/Csv keys to aggregate")]
    pub keys : Vec<String>,

    #[clap(short, long, help = "Tokenise lines (used for non JSON input)")]
    pub tokenise:  bool,

    #[clap(short, long, help = "Filter buckets by regular expression")]
    pub filter: Option<String>,

    #[clap(short, long, help = "Enable verbose mode")]
    pub verbose: bool,

    pub file: Option<String>,
}

pub enum AggrKeys {
    Keys(Vec<String>),
    Text(bool) // tokenise
}

impl AggrKeys {
    pub fn new(opt: &Options) -> Result<AggrKeys> {
        if opt.keys.is_empty() {
            Ok(AggrKeys::Text(opt.tokenise))
        } else {
            Ok(AggrKeys::Keys(opt.keys.clone()))
        }
    }
}