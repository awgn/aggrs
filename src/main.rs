mod options;
mod aggregate;
mod discovery;
mod visitors;
mod merge;

use anyhow::Result;
use clap::Parser;
use options::Options;
extern crate scopeguard;

fn main() -> Result<()> {
    let opt = Options::parse();
    match opt.discovery {
        Some(_) => {
            discovery::run(opt)
        }
        None => {
            aggregate::run(opt)
        }
    }
}
