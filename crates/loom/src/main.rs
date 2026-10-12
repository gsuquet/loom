//! `loom`: the CLI, later the TUI.

mod client;
mod service_cmd;
mod start;

use std::env;
use std::fs;
use std::io::{self, IsTerminal as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser as _;
use loom::cli::{Cli, Command, GlobalArgs, ServiceCommand, VERSION_INFO};
use loom::conventions::{Exit, use_color, write_out};
use loom::logging::level;
use loom::loomd_path::loomd_candidates;
use loom::service::{
    ServicePaths, ServiceSpec, format_status, opt_out_message, render_unit, unit_path,
};
use loom::status::{render_human, version_mismatch};
use loom_api::{ServicePlatform, StatusResponse, VersionInfo};
use loom_config::config::{Config, parse_config};
use loom_config::home::{LOOM_HOME_ENV, resolve_home};
use loom_config::layout::{Layout, MAX_SOCKET_PATH_BYTES, check_socket_path};
use miette::{Context as _, IntoDiagnostic as _, Result, miette};
use service_cmd::{SystemRunner, Target};
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
        Command::Service { command } => run_service(command),
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

/// `loom status`: queries loomd, starting it first unless `no_start`.
fn status(json: bool, no_start: bool) -> Result<()> {
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
    let layout = Layout::new(home);
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

/// Platform of this build, `None` when it has no supported service manager.
const fn current_platform() -> Option<ServicePlatform> {
    if cfg!(target_os = "macos") {
        Some(ServicePlatform::Macos)
    } else if cfg!(target_os = "linux") {
        Some(ServicePlatform::Linux)
    } else {
        None
    }
}

fn run_service(command: &ServiceCommand) -> Result<()> {
    let platform = current_platform()
        .ok_or_else(|| miette!("loom service supports macOS (launchd) and Linux (systemd) only"))?;
    let user_home = env::home_dir()
        .filter(|home| !home.as_os_str().is_empty())
        .ok_or_else(|| miette!("cannot find the user's home directory: HOME is unset or empty"))?;
    let cwd = env::current_dir()
        .into_diagnostic()
        .wrap_err("cannot read the current directory")?;
    let home = resolve_home(
        None,
        env::var_os(LOOM_HOME_ENV).as_deref(),
        Some(&user_home),
        &cwd,
    )
    .into_diagnostic()?;
    let layout = Layout::new(home);
    let unit = unit_path(
        platform,
        &user_home,
        env::var_os("XDG_CONFIG_HOME").as_deref(),
    );
    if unit.to_str().is_none() {
        return Err(miette!(
            "{} is not valid UTF-8: it cannot be passed to the service manager",
            unit.display()
        ));
    }
    let target = Target {
        platform,
        paths: ServicePaths {
            unit,
            log_dir: layout.log_dir.clone(),
        },
    };

    let mut runner = SystemRunner;
    let text = match command {
        ServiceCommand::Install { dry_run } => {
            // Before anything else: an opted-out user gets no command run.
            let config = load_config(&layout.config)?;
            if !config.daemon.service {
                return Err(miette!("{}", opt_out_message(&layout.config)));
            }
            let spec = ServiceSpec {
                loomd: find_loomd()?,
                home: layout.home,
            };
            let desired = render_unit(platform, &spec).into_diagnostic()?;
            service_cmd::install(&target, &desired, &mut runner, *dry_run)?
        }
        ServiceCommand::Uninstall { dry_run } => {
            service_cmd::uninstall(&target, &mut runner, *dry_run)?
        }
        ServiceCommand::Status { json } => {
            let status = service_cmd::status(&target, &mut runner)?;
            if *json {
                serde_json::to_string_pretty(&status).into_diagnostic()? + "\n"
            } else {
                format_status(&status)
            }
        }
    };
    write_out(&mut io::stdout().lock(), &text).into_diagnostic()
}

/// Reads the configuration; a missing file gives the defaults.
fn load_config(path: &Path) -> Result<Config> {
    match fs::read_to_string(path) {
        Ok(text) => parse_config(&text, path).into_diagnostic(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(error) => Err(error)
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot read the configuration {}", path.display())),
    }
}

/// Absolute, canonical path of the first `loomd` found next to `loom`, then in `PATH`.
fn find_loomd() -> Result<PathBuf> {
    let exe = env::current_exe()
        .into_diagnostic()
        .wrap_err("cannot find the path of the loom executable")?;
    // Through a symbolic link to `loom`, look next to its target.
    let exe = fs::canonicalize(&exe).unwrap_or(exe);
    let candidates = loomd_candidates(&exe, env::var_os("PATH").as_deref());
    let found = candidates
        .iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            let searched: Vec<String> = candidates
                .iter()
                .map(|candidate| candidate.display().to_string())
                .collect();
            miette!(
                "cannot find loomd: install it next to loom or in a PATH directory (searched: {})",
                searched.join(", ")
            )
        })?;
    fs::canonicalize(found)
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot resolve {}", found.display()))
}
