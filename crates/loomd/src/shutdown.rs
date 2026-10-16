//! Bounded shutdown of loomd: the registry of the git process groups of the sync thread, and
//! the join of a thread with a deadline.
//!
//! Git commands run in their own process group (`ssh`, hooks and credential helpers are in it).
//! A command that outlives loomd is reparented to PID 1 and can still hold a lock of the store
//! when the next loomd starts, so the daemon stops them before it exits.

use std::collections::BTreeSet;
use std::io;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tracing::warn;

/// Time that in-flight API requests have to finish after the first signal. A second signal ends
/// the wait at once.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Time a thread has to end after it was told to stop, before loomd exits without it.
pub const JOIN_DEADLINE: Duration = Duration::from_secs(2);

/// The process groups of the commands that loomd stops at shutdown.
#[derive(Debug, Default)]
pub struct ChildGroups {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// [`ChildGroups::terminate`] ran: nothing starts any more.
    stopping: bool,
    /// Process group IDs (the PID of each leader) of the commands that run.
    groups: BTreeSet<u32>,
}

/// Proof that a command is in the registry; drop it when the command has ended.
#[derive(Debug)]
pub struct Registration {
    groups: Arc<ChildGroups>,
    pgid: u32,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.groups.lock().groups.remove(&self.pgid);
    }
}

impl ChildGroups {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Number of commands that run.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().groups.len()
    }

    /// Whether no command runs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether [`Self::terminate`] ran: a caller that goes through a list of commands stops
    /// there instead of starting each one just to see it refused.
    #[must_use]
    pub fn is_stopping(&self) -> bool {
        self.lock().stopping
    }

    /// Starts `command` as the leader of a new process group and registers the group. The
    /// registry is locked from the start to the registration, so that [`Self::terminate`] never
    /// misses a command.
    ///
    /// # Errors
    ///
    /// The error of the start, or an error of kind `Interrupted` when [`Self::terminate`] ran.
    pub fn spawn(self: &Arc<Self>, command: &mut Command) -> io::Result<(Child, Registration)> {
        let mut state = self.lock();
        if state.stopping {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "loomd is stopping: no new command is started",
            ));
        }
        let child = command.process_group(0).spawn()?;
        let pgid = child.id();
        state.groups.insert(pgid);
        drop(state);
        Ok((
            child,
            Registration {
                groups: Arc::clone(self),
                pgid,
            },
        ))
    }

    /// Refuses new commands, sends `SIGTERM` to the group of each command that runs (git removes
    /// its lock files), waits up to `grace` for them to end, then sends `SIGKILL` to what is
    /// left. Returns when no command is registered, or after the `SIGKILL`.
    pub fn terminate(&self, grace: Duration) {
        let running: Vec<u32> = {
            let mut state = self.lock();
            state.stopping = true;
            state.groups.iter().copied().collect()
        };
        if running.is_empty() {
            return;
        }
        for pgid in &running {
            signal_group(*pgid, "-TERM");
        }
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline && !self.is_empty() {
            thread::sleep(Duration::from_millis(10));
        }
        // A group stays registered until its command was waited for, pipes drained: a process
        // of the group that outlives the leader and holds the pipes is covered. A group that is
        // not registered any more is never signalled, its PID could be reused.
        let left: Vec<u32> = self.lock().groups.iter().copied().collect();
        for pgid in left {
            warn!(pgid, "a git command ignored SIGTERM: killed");
            signal_group(pgid, "-KILL");
        }
    }
}

/// Sends `signal` (`-TERM`, `-KILL`) to the process group `pgid`, through the `kill` command
/// (the workspace has no `unsafe`).
pub fn signal_group(pgid: u32, signal: &str) {
    let group = format!("-{pgid}");
    let sent = Command::new("kill")
        .args([signal, "--", group.as_str()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if let Err(error) = sent {
        warn!(%error, "cannot run kill");
    }
}

/// Waits up to `deadline` for `thread` to end. `true` when it ended; a panic of the thread is
/// logged. `false` when it did not end in time: the thread is left to the exit of the process.
pub fn join_within<T>(thread: JoinHandle<T>, deadline: Duration, name: &str) -> bool {
    let limit = Instant::now() + deadline;
    while !thread.is_finished() {
        if Instant::now() >= limit {
            warn!(
                thread = name,
                "thread still running at shutdown: left behind"
            );
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    if thread.join().is_err() {
        warn!(thread = name, "thread panicked");
    }
    true
}
