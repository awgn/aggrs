mod options;
mod aggregate;
mod visitors;

use anyhow::Result;
use clap::Parser;
use options::Options;

fn main() -> Result<()> {
    let opt = Options::parse();
    aggregate::run(opt)
}
