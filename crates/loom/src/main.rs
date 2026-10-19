//! `loom`: the CLI and the TUI.

mod client;
mod on_use;
mod reindex_cmd;
mod repo_cmd;
mod restart_cmd;
mod search_cmd;
mod service_cmd;
mod start;
mod sync_cmd;
mod task_cmd;
mod tui_cmd;
mod upgrade;
mod watchdog;

use std::env;
use std::fs;
use std::io::{self, IsTerminal as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{CommandFactory as _, FromArgMatches as _};
use loom::cli::{
    Cli, Command, GlobalArgs, RepoCommand, ServiceCommand, VERSION_INFO, command_path,
};
use loom::conventions::{Exit, use_color, write_out};
use loom::loomd_path::loomd_candidates;
use loom::service::{
    ServicePaths, ServiceSpec, format_status, opt_out_message, render_unit, unit_path,
};
use loom::shell::init_script;
use loom::status::{render_human, version_mismatch};
use loom_api::{ServicePlatform, StatusResponse, VersionInfo};
use loom_config::config::{Config, OtlpEndpoint, TelemetryConfig, parse_config};
use loom_config::home::{LOOM_HOME_ENV, resolve_home};
use loom_config::layout::{Layout, MAX_SOCKET_PATH_BYTES, check_socket_path};
use loom_telemetry::logging::level;
use loom_telemetry::settings::resolve;
use loom_telemetry::{LOOM_SHUTDOWN_TIMEOUT, Options, TelemetryGuard};
use miette::{Context as _, IntoDiagnostic as _, Result, miette};
use service_cmd::{Supervisor, SystemRunner, Target};
use tracing::field::Empty;
use tracing::level_filters::LevelFilter;
use tracing::{info_span, warn};

use crate::client::{Client, ClientError};

fn main() -> ExitCode {
    // Usage errors exit with code 2, `--help` and `--version` with 0 (clap).
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches)
        .map_err(|error| error.format(&mut Cli::command()))
        .unwrap_or_else(|error| error.exit());
    let interactive_tui =
        cli.command.is_none() && io::stdin().is_terminal() && io::stdout().is_terminal();
    let (config, config_problem) = telemetry_config();
    // The watchdog writes to `log/loomd.log`, which needs timestamps like loomd's own lines.
    let timestamps = matches!(cli.command, Some(Command::UpgradeWatchdog(_)));
    let telemetry = init_telemetry(
        &cli.global,
        &config,
        timestamps,
        silent(cli.command.as_ref(), interactive_tui),
    );
    if let Some(problem) = config_problem {
        warn!("traces are not exported: {problem}");
    }

    // Root span of the command; every other span of `loom` is its descendant.
    let span = info_span!(
        "command",
        otel.name = %command_path(&matches),
        otel.status_code = Empty,
        otel.status_description = Empty,
        loom.context = Empty,
    );
    if let Some(context) = &cli.global.context {
        span.record("loom.context", context.as_str());
    }
    let result = span.in_scope(|| {
        let registration = registers_on_use(cli.command.as_ref(), interactive_tui)
            .then(|| on_use::spawn(layout().ok(), None))
            .flatten();
        let result = run(&cli);
        if let Some(registration) = registration {
            registration.wait();
        }
        result
    });
    let exit = match result {
        Ok(()) => Exit::Success,
        Err(report) => {
            span.record("otel.status_code", "ERROR");
            span.record("otel.status_description", report.to_string());
            // Same layout as `loomd`: miette's default `Debug` output is a struct dump.
            eprintln!("Error: {report}");
            for cause in report.chain().skip(1) {
                eprintln!("  caused by: {cause}");
            }
            Exit::Failure
        }
    };
    // Ends the root span, then exports every span, waiting at most `LOOM_SHUTDOWN_TIMEOUT`.
    drop(span);
    telemetry.shutdown();
    exit.into()
}

/// `[telemetry]` of `config.toml`, and why it was ignored. Only telemetry depends on it here: a
/// configuration that cannot be read or parsed leaves telemetry off and the command runs.
fn telemetry_config() -> (TelemetryConfig, Option<String>) {
    let Ok(cwd) = env::current_dir() else {
        return (TelemetryConfig::default(), None);
    };
    // No loom home, no configuration: commands that need the home report it themselves.
    let Ok(home) = resolve_home(
        None,
        env::var_os(LOOM_HOME_ENV).as_deref(),
        env::home_dir().as_deref(),
        &cwd,
    ) else {
        return (TelemetryConfig::default(), None);
    };
    let path = Layout::new(home).config;
    match fs::read_to_string(&path) {
        Ok(text) => match parse_config(&text, &path) {
            Ok(config) => (config.telemetry, None),
            Err(error) => (TelemetryConfig::default(), Some(error.to_string())),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => (TelemetryConfig::default(), None),
        Err(error) => (
            TelemetryConfig::default(),
            Some(format!(
                "cannot read the configuration {}: {error}",
                path.display()
            )),
        ),
    }
}

/// Logs to stderr, at the level and with the colors the global flags ask for (nothing when
/// `silent`), and exports traces when `[telemetry]` enables it.
fn init_telemetry(
    global: &GlobalArgs,
    config: &TelemetryConfig,
    timestamps: bool,
    silent: bool,
) -> TelemetryGuard {
    let color = use_color(
        global.color,
        env::var_os("NO_COLOR").as_deref(),
        io::stderr().is_terminal(),
    );
    let env_var = |name: &str| env::var(name).ok();
    let telemetry = resolve(
        config.enabled,
        config.otlp_endpoint.as_ref().map(OtlpEndpoint::as_str),
        &env_var,
    );
    let options = Options {
        service_name: "loom",
        version: &VERSION_INFO,
        level: if silent {
            LevelFilter::OFF
        } else {
            level(global.quiet, global.verbose)
        },
        color,
        timestamps,
        telemetry: &telemetry,
        shutdown_timeout: LOOM_SHUTDOWN_TIMEOUT,
    };
    loom_telemetry::init(&options, &env_var)
}

/// Whether the command registers the repository of the current directory on use. Not the
/// commands that register or remove repositories themselves, nor the internal watchdog.
const fn registers_on_use(command: Option<&Command>, interactive_tui: bool) -> bool {
    let Some(command) = command else {
        return interactive_tui;
    };
    match command {
        Command::Repo { command } => match command {
            // `sync` updates indexed repositories only: it never adds one.
            RepoCommand::Show(_)
            | RepoCommand::Register(_)
            | RepoCommand::Forget(_)
            | RepoCommand::Sync(_) => false,
            RepoCommand::Scan(args) => !args.rebuild,
            RepoCommand::Ls(_) | RepoCommand::Doctor(_) => true,
        },
        // Run by every new shell: no delay for it, and the hook it installs registers.
        Command::ShellInit(_) | Command::UpgradeWatchdog(_) => false,
        Command::Search(_)
        | Command::Add(_)
        | Command::Ls(_)
        | Command::Show(_)
        | Command::Set(_)
        | Command::Done(_)
        | Command::Cancel(_)
        | Command::Snooze(_)
        | Command::Undo(_)
        | Command::Edit(_)
        | Command::Reindex(_)
        | Command::Sync(_)
        | Command::Version { .. }
        | Command::Status { .. }
        | Command::Service { .. }
        | Command::Upgrade(_)
        | Command::Rollback(_) => true,
    }
}

/// Whether the command prints nothing on stderr, whatever happens:
/// `loom repo register --on-use --background`, or the interactive TUI (which owns the screen).
const fn silent(command: Option<&Command>, interactive_tui: bool) -> bool {
    match command {
        None => interactive_tui,
        Some(Command::Repo {
            command: RepoCommand::Register(args),
        }) => args.background,
        Some(_) => false,
    }
}

fn run(cli: &Cli) -> Result<()> {
    let Some(command) = &cli.command else {
        return tui_cmd::run(&cli.global);
    };
    match command {
        // Before `layout()?`: never fails.
        Command::Repo {
            command: RepoCommand::Register(args),
        } if args.background => {
            on_use::register_in_background(layout().ok(), args.path.clone());
            Ok(())
        }
        Command::ShellInit(args) => print(init_script(args.shell)),
        Command::Version { json } => print_version(&VERSION_INFO, *json),
        Command::Status { json, no_start } => status(*json, *no_start),
        Command::Upgrade(args) => print(&upgrade::upgrade(&VERSION_INFO, args, &layout()?)?),
        Command::Rollback(args) => print(&upgrade::rollback(&VERSION_INFO, args, &layout()?)?),
        Command::Service { command } => run_service(command),
        Command::Repo { command } => repo_cmd::run(command, &cli.global, layout()?),
        Command::Search(args) => search_cmd::run(args, &cli.global, &layout()?),
        Command::Add(args) => task_cmd::add(args, &cli.global, &layout()?),
        Command::Ls(args) => task_cmd::ls(args, &cli.global, &layout()?),
        Command::Show(args) => task_cmd::show(args, &layout()?),
        Command::Set(args) => task_cmd::set(args, &layout()?),
        Command::Done(args) => task_cmd::status(args, "done", &layout()?),
        Command::Cancel(args) => task_cmd::status(args, "cancelled", &layout()?),
        Command::Snooze(args) => task_cmd::snooze(args, &layout()?),
        Command::Undo(args) => task_cmd::undo(args, &layout()?),
        Command::Edit(args) => task_cmd::edit(args, &layout()?),
        Command::Reindex(args) => reindex_cmd::run(args, &layout()?),
        Command::Sync(args) => sync_cmd::run(args, &cli.global, &layout()?),
        Command::UpgradeWatchdog(args) => {
            let layout = layout()?;
            let supervisor = watchdog_supervisor(&layout);
            watchdog::run(args, &layout, supervisor)
        }
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
    let target = service_target(platform, &user_home, &layout)?;

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
        ServiceCommand::Restart { json } => {
            // Before anything else: an invalid configuration would stop the new loomd.
            load_config(&layout.config)?;
            restart_cmd::run(&target, &layout, &mut runner, *json)?
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

/// Service file and log directory of `loom service` for the loom home of `layout`.
fn service_target(platform: ServicePlatform, user_home: &Path, layout: &Layout) -> Result<Target> {
    let unit = unit_path(
        platform,
        user_home,
        env::var_os("XDG_CONFIG_HOME").as_deref(),
    );
    if unit.to_str().is_none() {
        return Err(miette!(
            "{} is not valid UTF-8: it cannot be passed to the service manager",
            unit.display()
        ));
    }
    Ok(Target {
        platform,
        paths: ServicePaths {
            unit,
            log_dir: layout.log_dir.clone(),
        },
    })
}

/// The supervisor of loomd for the watchdog: the user service when it is installed for this
/// loom home and loaded. Anything that prevents the check (unsupported platform, no `HOME`,
/// a failing query) is logged and means unsupervised.
fn watchdog_supervisor(layout: &Layout) -> Supervisor {
    let Some(platform) = current_platform() else {
        return Supervisor::Unsupervised;
    };
    let Some(user_home) = env::home_dir().filter(|home| !home.as_os_str().is_empty()) else {
        warn!("HOME is unset or empty: cannot check for the loomd service");
        return Supervisor::Unsupervised;
    };
    let found = service_target(platform, &user_home, layout)
        .and_then(|target| service_cmd::supervisor(&target, &layout.home, &mut SystemRunner));
    found.unwrap_or_else(|error| {
        warn!("cannot check for the loomd service, assuming none: {error}");
        Supervisor::Unsupervised
    })
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
