//! The sync thread of loomd: pushes and fast-forward pulls each context store that has a
//! `store.backup.remote`, at start, then every `[tasks] sync_interval`, and on
//! `POST /v1/tasks/sync` (`loom sync`).
//!
//! The network steps (fetch, push) run on this thread; the steps that write the store (commit,
//! fast-forward) run on the task thread, between its other writes. What a sync does is decided
//! in `loomd::backup`, the git commands are in `loomd::store_git`.

use std::collections::BTreeMap;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use axum::http::StatusCode;
use loom_api::{BackupReport, BackupSync, ContextName, Timestamp};
use loom_config::config::StoreSettings;
use loomd::api::{invalid_request, timestamp, unavailable};
use loomd::backup::{report, sync_targets};
use loomd::repo_update::{GitRunner, KILL_GRACE};
use loomd::shutdown::{ChildGroups, JOIN_DEADLINE, join_within};
use loomd::store_git;
use miette::{Context as _, IntoDiagnostic as _, Result};
use tokio::sync::oneshot;
use tracing::{info, info_span, warn};

use crate::repos::Failure;
use crate::tasks::TaskHandle;

/// What the sync thread knows of the stores.
pub struct Stores {
    /// This machine.
    pub machine: String,
    /// The stores held here.
    pub held: BTreeMap<ContextName, StoreSettings>,
    /// The contexts not held here.
    pub not_held: Vec<ContextName>,
}

enum Command {
    /// Sync these contexts, then answer.
    Sync {
        contexts: Vec<ContextName>,
        reply: oneshot::Sender<()>,
    },
    Stop,
}

/// The running sync thread.
pub struct SyncThread {
    commands: mpsc::Sender<Command>,
    thread: JoinHandle<()>,
    children: Arc<ChildGroups>,
}

/// What the API handlers use.
#[derive(Clone)]
pub struct SyncHandle {
    commands: mpsc::Sender<Command>,
    stores: Arc<Stores>,
    synced: Arc<Mutex<BTreeMap<ContextName, BackupSync>>>,
    tasks: TaskHandle,
    /// The git commands of a sync: stopped at shutdown.
    children: Arc<ChildGroups>,
}

/// Starts the sync thread: a first sync of every store with a remote at once, then one every
/// `interval`.
pub fn start(
    stores: Stores,
    interval: Duration,
    tasks: TaskHandle,
    children: Arc<ChildGroups>,
) -> Result<(SyncThread, SyncHandle)> {
    let (commands, received) = mpsc::channel();
    let handle = SyncHandle {
        commands: commands.clone(),
        stores: Arc::new(stores),
        synced: Arc::new(Mutex::new(BTreeMap::new())),
        tasks,
        children,
    };
    let worker = handle.clone();
    let thread = thread::Builder::new()
        .name("store-sync".to_owned())
        .spawn(move || worker.run(&received, interval))
        .into_diagnostic()
        .wrap_err("cannot start the store sync thread")?;
    let children = Arc::clone(&handle.children);
    Ok((
        SyncThread {
            commands,
            thread,
            children,
        },
        handle,
    ))
}

impl SyncThread {
    /// Stops the thread: tells it to stop, stops the git commands of the sync it runs (`SIGTERM`,
    /// then `SIGKILL` after [`KILL_GRACE`]), and waits for it up to [`JOIN_DEADLINE`]. Without
    /// this, a `git fetch` or `git push` would outlive loomd and could hold a lock of the store
    /// when the next loomd starts.
    pub fn stop(self) {
        let _ = self.commands.send(Command::Stop);
        self.children.terminate(KILL_GRACE);
        join_within(self.thread, JOIN_DEADLINE, "store-sync");
    }
}

impl SyncHandle {
    /// Syncs `context`, or every store with a remote, and answers with their report.
    pub async fn sync(&self, context: Option<ContextName>) -> Result<BackupReport, Failure> {
        let contexts = sync_targets(context.as_ref(), &self.stores.held, &self.stores.not_held)
            .map_err(|message| (StatusCode::BAD_REQUEST, invalid_request(message)))?;
        let (reply, answer) = oneshot::channel();
        let stopped = || {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                unavailable("loomd is stopping"),
            )
        };
        self.commands
            .send(Command::Sync {
                contexts: contexts.clone(),
                reply,
            })
            .map_err(|_| stopped())?;
        answer.await.map_err(|_| stopped())?;
        self.report(Some(&contexts)).await
    }

    /// The backup state of every store held here, or of `only`.
    pub async fn report(&self, only: Option<&[ContextName]>) -> Result<BackupReport, Failure> {
        let pending = self.tasks.pending().await?;
        let synced = self
            .synced
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        Ok(report(
            &self.stores.machine,
            &self.stores.held,
            &self.stores.not_held,
            &pending,
            &synced,
            only,
        ))
    }

    /// Syncs on the timer and on request until `Stop` or until every sender is gone.
    fn run(&self, received: &mpsc::Receiver<Command>, interval: Duration) {
        let all = sync_targets(None, &self.stores.held, &self.stores.not_held).unwrap_or_default();
        // Without a remote, nothing to do until a request (which then fails before reaching
        // this thread).
        let mut next = (!all.is_empty()).then(Instant::now);
        loop {
            let command = match next {
                Some(at) => {
                    match received.recv_timeout(at.saturating_duration_since(Instant::now())) {
                        Ok(command) => command,
                        Err(RecvTimeoutError::Timeout) => {
                            self.sync_all(&all);
                            next = Some(Instant::now() + interval);
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                None => match received.recv() {
                    Ok(command) => command,
                    Err(_) => return,
                },
            };
            match command {
                Command::Sync { contexts, reply } => {
                    self.sync_all(&contexts);
                    let _ = reply.send(());
                }
                Command::Stop => return,
            }
        }
    }

    fn sync_all(&self, contexts: &[ContextName]) {
        let mut runner = GitRunner::from_env();
        runner.children = Some(Arc::clone(&self.children));
        for context in contexts {
            // The shutdown began: no command would start, and the local steps would write the
            // stores for a sync that cannot go on.
            if self.children.is_stopping() {
                break;
            }
            let Some(StoreSettings {
                path,
                remote: Some(url),
            }) = self.stores.held.get(context)
            else {
                continue;
            };
            let _span = info_span!("tasks.sync", %context).entered();
            let outcome = store_git::sync(&runner, path, url, &mut |step| {
                self.tasks.sync_step(context, step)
            });
            if outcome.failed() {
                warn!(%context, %outcome, "context store not synced");
            } else {
                info!(%context, %outcome, "context store synced");
            }
            let time = timestamp(SystemTime::now()).unwrap_or(Timestamp::EPOCH);
            self.synced
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(context.clone(), BackupSync { time, outcome });
        }
    }
}
