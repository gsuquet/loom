//! `loomd`: the daemon. Runs in the foreground until `SIGTERM` or `SIGINT`.

mod daemon;
mod repos;
mod updater;

use std::env;
use std::io::{self, IsTerminal as _};
use std::process::ExitCode;

use clap::Parser as _;
use loom_config::config::OtlpEndpoint;
use loom_telemetry::logging::{level, use_color};
use loom_telemetry::settings::resolve;
use loom_telemetry::{LOOMD_SHUTDOWN_TIMEOUT, Options, TelemetryGuard};
use loomd::cli::{Cli, VERSION_INFO};
use loomd::exit::FAILURE;

fn main() -> ExitCode {
    // Usage errors exit with code 2, `--help` and `--version` with 0 (clap).
    let cli = Cli::parse();
    // The configuration decides whether traces are exported, so it is read before logging is
    // set up; its errors are printed directly.
    let result = daemon::load(&cli).and_then(|setup| {
        let telemetry = init_telemetry(&cli, &setup);
        let result = daemon::run(setup);
        // Exports the spans of the last requests, waiting at most `LOOMD_SHUTDOWN_TIMEOUT`.
        telemetry.shutdown();
        result
    });
    match result {
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

/// Logs to stderr, at the level and with the colors the global flags ask for, and exports
/// traces when `[telemetry]` enables it.
fn init_telemetry(cli: &Cli, setup: &daemon::Setup) -> TelemetryGuard {
    let color = use_color(
        cli.color,
        env::var_os("NO_COLOR").as_deref(),
        io::stderr().is_terminal(),
    );
    let env_var = |name: &str| env::var(name).ok();
    let config = &setup.config.telemetry;
    let telemetry = resolve(
        config.enabled,
        config.otlp_endpoint.as_ref().map(OtlpEndpoint::as_str),
        &env_var,
    );
    let version = VERSION_INFO.clone();
    let options = Options {
        service_name: "loomd",
        version: &version,
        level: level(cli.quiet, cli.verbose),
        color,
        timestamps: true,
        telemetry: &telemetry,
        shutdown_timeout: LOOMD_SHUTDOWN_TIMEOUT,
    };
    loom_telemetry::init(&options, &env_var)
}
