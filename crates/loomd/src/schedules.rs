//! The schedule thread of loomd: runs the schedules of `config.toml` (ADR-0013, decision 9) on
//! the task thread at start, then every [`CHECK_INTERVAL`]. What each schedule does is decided
//! in `loomd::recurring`; the tasks are created by the task thread, between its other writes.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use loom_api::ContextName;
use loom_config::config::{Config, StoreSettings};
use miette::{Context as _, IntoDiagnostic as _, Result};
use tracing::info;

use crate::daemon::local_today;
use crate::tasks::{Schedules, TaskHandle};

/// Time between two checks of the schedules. Occurrences are dates: a check soon after local
/// midnight, or after the machine wakes up, creates the day's tasks.
pub const CHECK_INTERVAL: Duration = Duration::from_mins(1);

/// The running schedule thread.
pub struct ScheduleThread {
    stop: mpsc::Sender<()>,
}

/// Queues a first run of the schedules of the contexts `held` here on the task thread, before
/// the API serves any request, then starts the thread that queues one every
/// [`CHECK_INTERVAL`]. `None` without any schedule.
pub fn start(
    config: &Config,
    held: &BTreeMap<ContextName, StoreSettings>,
    tasks: TaskHandle,
) -> Result<Option<ScheduleThread>> {
    // The contexts not held here have no store.
    let schedules: Schedules = config
        .contexts
        .iter()
        .filter(|(context, _)| held.contains_key(*context))
        .map(|(context, settings)| (context.clone(), settings.schedules.clone()))
        .collect();
    let count: usize = schedules.values().map(Vec::len).sum();
    if count == 0 {
        return Ok(None);
    }
    info!(schedules = count, "schedules loaded");
    let schedules = Arc::new(schedules);
    tasks.run_schedules(Arc::clone(&schedules), local_today());
    let (stop, stopped) = mpsc::channel();
    thread::Builder::new()
        .name("schedules".to_owned())
        .spawn(move || {
            loop {
                match stopped.recv_timeout(CHECK_INTERVAL) {
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

impl ScheduleThread {
    /// Asks the thread to stop; does not wait for it.
    pub fn stop(self) {
        let _ = self.stop.send(());
    }
}
