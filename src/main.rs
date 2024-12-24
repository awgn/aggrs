mod options;
mod aggregate;

use anyhow::Result;
use clap::Parser;
use options::Options;

fn main() -> Result<()> {
    let opt = Options::parse();
    aggregate::aggregate(opt)
}
