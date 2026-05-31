mod aggregate;
mod discovery;
mod merge;
mod options;
mod smolvalue;
mod visitors;

use anyhow::Result;
use clap::Parser;
use options::Options;
extern crate scopeguard;

fn main() -> Result<()> {
    let opt = Options::parse();
    match opt.discovery {
        Some(_) => discovery::run(opt),
        None => aggregate::run(opt),
    }
}
