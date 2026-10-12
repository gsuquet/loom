//! `loom`: the CLI, later the TUI. Placeholder until v0.1 commands land.

use clap::Parser;

/// Personal work engine: CLI and TUI.
#[derive(Debug, Parser)]
#[command(name = "loom", version, about)]
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
