//! `loom service restart`: restarts loomd through its service manager, or by a signal and a
//! detached start, then waits for the new loomd to answer. The decisions are in
//! `loom::service` ([`plan_restart`]).

use std::thread;
use std::time::{Duration, Instant};

use loom::service::{RestartPlan, format_restart, is_new_daemon, plan_restart};
use loom_api::{DaemonRestart, DaemonRestartReport, StatusResponse};
use loom_config::layout::Layout;
use loom_update::daemon::{holder_pid, lock_held, restart};
use miette::{IntoDiagnostic as _, Result, miette};
use tracing::info;

use crate::client::Client;
use crate::service_cmd::{self, CommandRunner, Supervisor, Target};
use crate::start;

/// Longest wait for the signalled loomd to release the lock.
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest wait for the loomd restarted by the service manager to answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(15);
/// Interval between two checks of the lock or the socket.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Runs `loom service restart` for the loom home of `layout`, once the caller has checked the
/// configuration. Returns the stdout text.
pub fn run(
    target: &Target,
    layout: &Layout,
    runner: &mut impl CommandRunner,
    json: bool,
) -> Result<String> {
    let started = Instant::now();
    let previous_pid = holder_pid(&layout.lock);
    let was_running = lock_held(&layout.lock);
    let service = match service_cmd::supervisor(target, &layout.home, runner)? {
        Supervisor::Service(manager) => Some(manager),
        Supervisor::Unsupervised => None,
    };
    let plan = plan_restart(service, was_running);
    info!(?plan, previous_pid, "restarting loomd");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .into_diagnostic()?;
    let client = Client::new(layout.socket.clone());
    let status = match plan {
        RestartPlan::Manager(manager) => {
            service_cmd::recover(manager, runner)?;
            runtime.block_on(wait_for_new(&client, previous_pid, layout, plan))?
        }
        RestartPlan::Signal => {
            signal_and_wait(layout)?;
            runtime.block_on(start::start(layout, &client))?
        }
        RestartPlan::Start => runtime.block_on(start::start(layout, &client))?,
    };
    if !is_new_daemon(previous_pid, status.pid) {
        return Err(miette!(
            "loomd (pid {}) still answers: it was not restarted; log: {}",
            status.pid,
            layout.daemon_log.display()
        ));
    }
    let report = DaemonRestartReport {
        was_running,
        previous_pid,
        pid: status.pid,
        version: status.daemon.version,
        via: plan.via(),
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    };
    if json {
        Ok(serde_json::to_string_pretty(&report).into_diagnostic()? + "\n")
    } else {
        Ok(format_restart(&report))
    }
}

/// Sends `SIGTERM` to the loomd holding the lock and waits for it to release the lock. Never
/// sends `SIGKILL`: a loomd that does not exit keeps running and the command fails.
fn signal_and_wait(layout: &Layout) -> Result<()> {
    let pid = match restart(&layout.lock) {
        DaemonRestart::Signalled { pid } => pid,
        // Exited since the lock was checked: nothing to wait for.
        DaemonRestart::NotRunning => return Ok(()),
        DaemonRestart::Failed { pid, message } => {
            let pid = pid.map_or_else(String::new, |pid| format!(" (pid {pid})"));
            return Err(miette!("cannot stop loomd{pid}: {message}"));
        }
        DaemonRestart::Skipped => return Err(miette!("loomd was left alone: nothing to restart")),
    };
    let deadline = Instant::now() + EXIT_TIMEOUT;
    while lock_held(&layout.lock) {
        if Instant::now() >= deadline {
            return Err(miette!(
                "loomd (pid {pid}) did not exit within {} s of SIGTERM and keeps running; log: {}",
                EXIT_TIMEOUT.as_secs(),
                layout.daemon_log.display()
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
    Ok(())
}

/// Waits for a loomd other than `previous` to answer, after the service manager restarted it.
async fn wait_for_new(
    client: &Client,
    previous: Option<u32>,
    layout: &Layout,
    plan: RestartPlan,
) -> Result<StatusResponse> {
    let deadline = Instant::now() + ANSWER_TIMEOUT;
    loop {
        // Unreachable while the manager swaps the processes; the old loomd may still answer.
        if let Ok(status) = client.status().await
            && is_new_daemon(previous, status.pid)
        {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(miette!(
                "no new loomd answered on {} within {} s of the restart via {}; log: {}",
                client.socket().display(),
                ANSWER_TIMEOUT.as_secs(),
                plan.via().as_str(),
                layout.daemon_log.display()
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
