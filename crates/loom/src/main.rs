//! `loom`: the CLI, later the TUI.

use std::io::{self, Write as _};
use std::sync::LazyLock;

use clap::{Parser, Subcommand};
use loom_api::VersionInfo;
use miette::{IntoDiagnostic as _, Result};

/// Build information, computed once for `--version` and `loom version`.
static VERSION_INFO: LazyLock<VersionInfo> = LazyLock::new(VersionInfo::current);
/// `--version` output: every build field, identical to `loomd --version`.
static LONG_VERSION: LazyLock<String> = LazyLock::new(|| VERSION_INFO.to_string());

/// Personal work engine: CLI and TUI.
#[derive(Debug, Parser)]
#[command(
    name = "loom",
    version = VERSION_INFO.version.as_str(),
    long_version = LONG_VERSION.as_str(),
    about
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print version, commit, target, channel and profile of this build.
    Version {
        /// Print the build information as JSON (`VersionInfo` schema).
        #[arg(long)]
        json: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Version { json } => print_version(&VERSION_INFO, json),
    }
}

fn print_version(info: &VersionInfo, json: bool) -> Result<()> {
    let mut stdout = io::stdout().lock();
    if json {
        serde_json::to_writer_pretty(&mut stdout, info).into_diagnostic()?;
        writeln!(stdout).into_diagnostic()
    } else {
        writeln!(stdout, "loom {info}").into_diagnostic()
    }
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
