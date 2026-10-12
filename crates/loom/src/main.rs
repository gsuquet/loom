//! `loom`: the CLI, later the TUI.

mod client;
mod start;
mod upgrade;

use std::env;
use std::io::{self, IsTerminal as _};
use std::process::ExitCode;

use clap::Parser as _;
use loom::cli::{Cli, Command, GlobalArgs, VERSION_INFO};
use loom::conventions::{Exit, use_color, write_out};
use loom::logging::level;
use loom::status::{render_human, version_mismatch};
use loom_api::{StatusResponse, VersionInfo};
use loom_config::home::{LOOM_HOME_ENV, resolve_home};
use loom_config::layout::{Layout, MAX_SOCKET_PATH_BYTES, check_socket_path};
use miette::{Context as _, IntoDiagnostic as _, Result, miette};
use tracing::warn;

use crate::client::{Client, ClientError};

fn main() -> ExitCode {
    // Usage errors exit with code 2, `--help` and `--version` with 0 (clap).
    let cli = Cli::parse();
    init_logging(&cli.global);
    match run(&cli) {
        Ok(()) => Exit::Success.into(),
        Err(report) => {
            // Same layout as `loomd`: miette's default `Debug` output is a struct dump.
            eprintln!("Error: {report}");
            for cause in report.chain().skip(1) {
                eprintln!("  caused by: {cause}");
            }
            Exit::Failure.into()
        }
    }
}

/// Logs to stderr, at the level and with the colors the global flags ask for.
fn init_logging(global: &GlobalArgs) {
    let color = use_color(
        global.color,
        env::var_os("NO_COLOR").as_deref(),
        io::stderr().is_terminal(),
    );
    let result = tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_ansi(color)
        .with_max_level(level(global.quiet, global.verbose))
        .without_time()
        .with_target(false)
        .try_init();
    if let Err(error) = result {
        eprintln!("Error: cannot set up logging: {error}");
    }
}

fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Version { json } => print_version(&VERSION_INFO, *json),
        Command::Status { json, no_start } => status(*json, *no_start),
        Command::Upgrade(args) => print(&upgrade::upgrade(&VERSION_INFO, args, &layout()?)?),
        Command::Rollback(args) => print(&upgrade::rollback(&VERSION_INFO, args, &layout()?)?),
    }
}

fn print(text: &str) -> Result<()> {
    write_out(&mut io::stdout().lock(), text).into_diagnostic()
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

/// Paths under the loom home: `LOOM_HOME`, otherwise `~/.loom`.
fn layout() -> Result<Layout> {
    let cwd = env::current_dir()
        .into_diagnostic()
        .wrap_err("cannot read the current directory")?;
    // `loom` has no `--loom-home` flag.
    let home = resolve_home(
        None,
        env::var_os(LOOM_HOME_ENV).as_deref(),
        env::home_dir().as_deref(),
        &cwd,
    )
    .into_diagnostic()?;
    Ok(Layout::new(home))
}

/// `loom status`: queries loomd, starting it first unless `no_start`.
fn status(json: bool, no_start: bool) -> Result<()> {
    let layout = layout()?;
    check_socket_path(&layout.socket, MAX_SOCKET_PATH_BYTES).into_diagnostic()?;

    // The async runtime only drives the API client.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()
        .wrap_err("cannot start the async runtime")?;
    let status = runtime.block_on(query_or_start(&layout, no_start))?;

    if let Some(warning) = version_mismatch(&VERSION_INFO, &status.daemon, status.pid) {
        warn!("{warning}");
    }
    let text = if json {
        let mut text = serde_json::to_string_pretty(&status).into_diagnostic()?;
        text.push('\n');
        text
    } else {
        render_human(&status)
    };
    write_out(&mut io::stdout().lock(), &text).into_diagnostic()
}

async fn query_or_start(layout: &Layout, no_start: bool) -> Result<StatusResponse> {
    let client = Client::new(layout.socket.clone());
    match client.status().await {
        Ok(status) => Ok(status),
        Err(ClientError::Unreachable { .. }) if !no_start => start::start(layout, &client).await,
        Err(ClientError::Unreachable { socket, source }) => Err(miette!(
            "loomd is not running: cannot connect to {}: {source}; run `loom status` without --no-start to start it",
            socket.display()
        )),
        Err(error @ ClientError::Api { .. }) => Err(miette!("{error}")),
        Err(error @ ClientError::Protocol(_)) => Err(miette!(
            "{error}; check that the process listening on {} is a loomd of the same version as loom",
            layout.socket.display()
        )),
    }
}
