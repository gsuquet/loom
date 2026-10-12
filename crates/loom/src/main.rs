//! `loom`: the CLI, later the TUI.

use std::io;
use std::process::ExitCode;

use clap::Parser as _;
use loom::cli::{Cli, Command, VERSION_INFO};
use loom::conventions::{Exit, write_out};
use loom_api::VersionInfo;
use miette::{IntoDiagnostic as _, Result};

fn main() -> ExitCode {
    // Usage errors exit with code 2, `--help` and `--version` with 0 (clap).
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => Exit::Success.into(),
        Err(report) => {
            eprintln!("{report:?}");
            Exit::Failure.into()
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    match cli.command {
        Command::Version { json } => print_version(&VERSION_INFO, json),
    }
}

fn print_version(info: &VersionInfo, json: bool) -> Result<()> {
    let text = if json {
        let mut text = serde_json::to_string_pretty(info).into_diagnostic()?;
        text.push('\n');
        text
    } else {
        format!("loom {info}\n")
    };
    write_out(&mut io::stdout().lock(), &text).into_diagnostic()
}
