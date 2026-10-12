//! `loomd`: the daemon. Runs in the foreground until `SIGTERM` or `SIGINT`.

mod daemon;

use std::env;
use std::io::{self, IsTerminal as _};
use std::process::ExitCode;

use clap::Parser as _;
use loomd::cli::Cli;
use loomd::exit::FAILURE;
use loomd::logging::{level, use_color};

fn main() -> ExitCode {
    // Usage errors exit with code 2, `--help` and `--version` with 0 (clap).
    let cli = Cli::parse();
    init_logging(&cli);
    match daemon::run(&cli) {
        Ok(shutdown) => ExitCode::from(shutdown.exit_code()),
        Err(report) => {
            eprintln!("Error: {report}");
            for cause in report.chain().skip(1) {
                eprintln!("  caused by: {cause}");
            }
            ExitCode::from(FAILURE)
        }
    }
}

/// Logs to stderr, at the level and with the colors the global flags ask for.
fn init_logging(cli: &Cli) {
    let color = use_color(
        cli.color,
        env::var_os("NO_COLOR").as_deref(),
        io::stderr().is_terminal(),
    );
    let result = tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_ansi(color)
        .with_max_level(level(cli.quiet, cli.verbose))
        .try_init();
    if let Err(error) = result {
        eprintln!("Error: cannot set up logging: {error}");
    }
}
