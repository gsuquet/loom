//! The schedule thread of loomd: runs the schedules of `config.toml` (ADR-0013, decision 9) on
//! the task thread at start, then every [`CHECK_INTERVAL`]. What each schedule does is decided
//! in `loomd::recurring`; the tasks are created by the task thread, between its other writes.

use std::collections::BTreeMap;
use std::env;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use loom_api::ContextName;
use loom_config::config::{Config, StoreSettings};
use loom_config::machine::MachineName;
use loomd::hooks;
use loomd::recurring::{Runner, runner};
use miette::{Context as _, IntoDiagnostic as _, Result};
use tracing::{info, warn};

use crate::daemon::local_today;
use crate::tasks::{Schedules, TaskHandle};

/// Time between two checks of the schedules. Occurrences are dates: a check soon after local
/// midnight, or after the machine wakes up, creates the day's tasks.
pub const CHECK_INTERVAL: Duration = Duration::from_mins(1);

/// The running schedule thread.
pub struct ScheduleThread {
    stop: mpsc::Sender<()>,
}

/// Queues a first run of the schedules this machine runs on the task thread, before the API
/// serves any request, then starts the thread that queues one every [`CHECK_INTERVAL`]. `None`
/// without any schedule.
///
/// This machine runs the schedules of the contexts it holds (`held`) and lists first in their
/// `machines`, or that have no `machines`, with a warning: every machine runs those.
pub fn start(
    config: &Config,
    held: &BTreeMap<ContextName, StoreSettings>,
    machine: &MachineName,
    tasks: TaskHandle,
) -> Result<Option<ScheduleThread>> {
    let mut schedules = Schedules::new();
    for (context, settings) in &config.contexts {
        // The contexts not held here have no store.
        if settings.schedules.is_empty() || !held.contains_key(context) {
            continue;
        }
        match runner(settings.machines.as_deref(), machine) {
            Runner::This => {}
            Runner::Other(first) => {
                info!(%context, runner = %first, "schedules of the context run on another machine");
                continue;
            }
            Runner::Unpinned => warn!(
                %context,
                "schedules run on every machine holding the context: set `machines` so that only its first machine creates their tasks"
            ),
        }
        schedules.insert(context.clone(), settings.schedules.clone());
    }
    let count: usize = schedules.values().map(Vec::len).sum();
    if count == 0 {
        return Ok(None);
    }
    info!(schedules = count, "schedules loaded");
    let schedules = Arc::new(schedules);
    tasks.run_schedules(Arc::clone(&schedules), local_today());
    let interval = check_interval();
    let (stop, stopped) = mpsc::channel();
    thread::Builder::new()
        .name("schedules".to_owned())
        .spawn(move || {
            loop {
                match stopped.recv_timeout(interval) {
                    Err(RecvTimeoutError::Timeout) => {}
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                }
                if !tasks.run_schedules(Arc::clone(&schedules), local_today()) {
                    return;
                }
            }
        })
        .into_diagnostic()
        .wrap_err("cannot start the schedule thread")?;
    Ok(Some(ScheduleThread { stop }))
}

/// [`CHECK_INTERVAL`], or the debug-only test hook.
fn check_interval() -> Duration {
    hooks::duration_hook(
        hooks::SCHEDULE_INTERVAL_ENV,
        env::var(hooks::SCHEDULE_INTERVAL_ENV).ok().as_deref(),
    )
    .unwrap_or_else(|message| {
        warn!("{message}");
        None
    })
    .unwrap_or(CHECK_INTERVAL)
}

impl ScheduleThread {
    /// Asks the thread to stop; does not wait for it.
    pub fn stop(self) {
        let _ = self.stop.send(());
    }
}
