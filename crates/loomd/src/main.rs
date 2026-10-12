//! `loomd`: the daemon. Placeholder until v0.1 commands land.

use clap::Parser;

/// Loom daemon: API server, scheduler, updater.
#[derive(Debug, Parser)]
#[command(name = "loomd", version, about)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
