//! `loomd`: the daemon. Placeholder until v0.1 commands land.

use clap::Parser as _;
use loomd::cli::Cli;

fn main() {
    // Usage errors exit with code 2, `--help` and `--version` with 0 (clap).
    let _cli = Cli::parse();
}
