//! `loom __upgrade-watchdog` (ADR-0011): started by loomd from the saved previous version after
//! it replaced the binaries. Waits for the new loomd to answer with the expected version; on
//! failure, restores the previous binaries with its own (previous) code, records the failed
//! version, and makes sure the previous loomd runs again.

use std::path::Path;
use std::process::Child;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use loom::cli::WatchdogArgs;
use loom_api::{Timestamp, UpdateState};
use loom_config::layout::Layout;
use loom_update::{auto, daemon, health, install, state};
use miette::{Result, miette};
use semver::Version;
use tracing::field::Empty;
use tracing::{error, info, info_span, warn};

use crate::start::spawn_detached;

const POLL: Duration = Duration::from_millis(200);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Runs the watchdog, traced as the span `update.watchdog` (attributes `update.from`,
/// `update.to`, `update.outcome`: `upgraded`, `rolled_back` or `rollback_failed`).
pub fn run(args: &WatchdogArgs, layout: &Layout) -> Result<()> {
    let span = info_span!(
        "update.watchdog",
        update.from = %args.from,
        update.to = %args.expect,
        update.outcome = Empty,
        otel.status_code = Empty,
        otel.status_description = Empty,
    );
    let (outcome, result) = span.in_scope(|| watch(args, layout));
    span.record("update.outcome", outcome);
    if let Err(error) = &result {
        loom_update::record_error(&span, error);
    }
    result
}

fn watch(args: &WatchdogArgs, layout: &Layout) -> (&'static str, Result<()>) {
    let deadline = Duration::from_secs(args.deadline);
    let loomd = args.install_dir.join("loomd");
    info!(
        from = %args.from,
        expect = %args.expect,
        deadline_secs = args.deadline,
        "watching the upgraded loomd"
    );
    let Err(reason) = wait_for(layout, &loomd, &args.expect, deadline) else {
        info!(version = %args.expect, "the upgraded loomd answers: upgrade complete");
        record(layout, |state| {
            auto::after_upgraded(state, &args.from, &args.expect, now())
        });
        return ("upgraded", Ok(()));
    };

    warn!(version = %args.expect, "the upgraded loomd failed its health check, rolling back to {}: {reason}", args.from);
    let saved = layout.versions_dir.join(args.from.to_string());
    let restored = install::replace_binaries(&args.install_dir, &saved);
    let message = match &restored {
        Ok(()) => reason,
        Err(error) => format!("{reason}; restoring {} failed: {error}", args.from),
    };
    record(layout, |state| {
        auto::after_rolled_back(state, &args.from, &args.expect, message, now())
    });
    if let Err(error) = restored {
        return (
            "rollback_failed",
            Err(miette!("cannot restore {}: {error}", args.from)),
        );
    }
    // `versions/<from>` now duplicates the running binaries.
    let (pruned, warnings) = loom_update::retain(&layout.versions_dir, &args.from, None);
    for warning in &warnings {
        warn!("{warning}");
    }
    if !pruned.is_empty() {
        info!(pruned = ?pruned.iter().map(ToString::to_string).collect::<Vec<_>>(), "removed saved versions");
    }

    // The unhealthy loomd may still hold the lock: stop it so the restored one can start.
    if daemon::lock_held(&layout.lock) {
        info!(
            "stopping the unhealthy loomd: {:?}",
            daemon::restart(&layout.lock)
        );
    }
    match wait_for(layout, &loomd, &args.from, deadline) {
        Ok(()) => {
            info!(version = %args.from, "rolled back: the previous loomd answers");
            // The upgrade failed: the span carries the reason as an error.
            (
                "rolled_back",
                Err(miette!(
                    "{} failed its health check, {} restored",
                    args.expect,
                    args.from
                )),
            )
        }
        Err(reason) => {
            error!(version = %args.from, "rolled back, but the previous loomd does not answer: {reason}");
            (
                "rollback_failed",
                Err(miette!(
                    "rolled back to {}, but loomd does not answer: {reason}",
                    args.from
                )),
            )
        }
    }
}

/// Waits up to `deadline` for a loomd answering with `version` on the socket. When nobody
/// holds the lock for the grace period (no service manager restarted it), starts `loomd`
/// detached, once.
fn wait_for(
    layout: &Layout,
    loomd: &Path,
    version: &Version,
    deadline: Duration,
) -> Result<(), String> {
    let grace = auto::restart_grace(deadline);
    let end = Instant::now() + deadline;
    let expected = version.to_string();
    let mut free_since: Option<Instant> = None;
    let mut started: Option<Child> = None;
    let mut last;
    loop {
        if let Some(child) = &mut started {
            // Reaps it if it exited.
            let _ = child.try_wait();
        }
        match health::status(&layout.socket, REQUEST_TIMEOUT) {
            Ok(status) if status.daemon.version == expected => return Ok(()),
            Ok(status) => {
                last = format!(
                    "loomd {} answered, expected {expected}",
                    status.daemon.version
                );
            }
            Err(error) => last = error,
        }
        if daemon::lock_held(&layout.lock) {
            free_since = None;
        } else {
            let since = *free_since.get_or_insert_with(Instant::now);
            if started.is_none() && since.elapsed() >= grace {
                info!(
                    loomd = %loomd.display(),
                    "no service manager restarted loomd within {} s, starting it",
                    grace.as_secs()
                );
                match spawn_detached(loomd, layout) {
                    Ok((child, _)) => started = Some(child),
                    Err(error) => {
                        last = error.to_string();
                        warn!("{last}");
                        return Err(last);
                    }
                }
            }
        }
        if Instant::now() >= end {
            return Err(format!("{last} (deadline {} s)", deadline.as_secs()));
        }
        thread::sleep(POLL);
    }
}

/// Changes the update state; a failure is logged (the binaries matter more than the record).
fn record(layout: &Layout, change: impl FnOnce(UpdateState) -> UpdateState) {
    let result = state::modify(
        layout,
        |error| warn!(%error, "replacing an invalid update state"),
        change,
    );
    if let Err(error) = result {
        error!(%error, "cannot write the update state");
    }
}

fn now() -> Timestamp {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    // After year 9999: the record is the least of the problems.
    Timestamp::from_unix_secs(secs).unwrap_or(Timestamp::EPOCH)
}
