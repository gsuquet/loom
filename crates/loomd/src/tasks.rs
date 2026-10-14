//! The task store of loomd (ADR-0013): a thread owns the files of every context store and the
//! connection that writes the tasks of `index.db`, and runs their operations one at a time.
//!
//! - At start, it reconciles every task with the index: new files are indexed, hand edits made
//!   while loomd was stopped are recorded, rows of removed tasks dropped.
//! - A file watcher on each `tasks/` directory sends the tasks whose files change; each is
//!   reconciled the same way. A change loomd made itself has the hash the index holds and is
//!   skipped.
//! - `POST /v1/tasks/reindex` reconciles every task, then rebuilds the task rows from the files.
//!
//! Decisions are in `loom_store` and `loomd::task_index`; this module does the I/O.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use axum::http::StatusCode;
use loom_api::{ContextName, ReindexResult, TaskId, Timestamp};
use loom_core::{Actor, MachineId, Origin, RandomSource};
use loom_store::files::FileStore;
use loom_store::tasks::{Reconciled, TaskFailure, TaskIndex, Tasks};
use loomd::api::{internal, timestamp, unavailable};
use loomd::task_index::{failed_files, reindex_result, task_of_path};
use miette::{Context as _, IntoDiagnostic as _, Result};
use notify::event::{AccessKind, AccessMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use tokio::sync::{oneshot, watch};
use tracing::{Span, debug, info, info_span, warn};

use crate::repos::Failure;

type Reply<T> = oneshot::Sender<Result<T, Failure>>;

/// A request to the task thread, with the span of the API request that sent it.
enum Command {
    Reindex {
        reply: Reply<ReindexResult>,
        parent: Span,
    },
    /// Reconcile every task: at start, and when the watcher may have missed events.
    ReconcileAll,
    /// A file of this task changed.
    Changed {
        context: ContextName,
        id: TaskId,
    },
    Stop,
}

/// The running task thread and its file watcher.
pub struct TaskStore {
    commands: mpsc::Sender<Command>,
    thread: JoinHandle<()>,
    /// Dropped on stop: no more events.
    watcher: Option<RecommendedWatcher>,
}

/// What the API handlers use: the command channel, and whether the reconciliation on start is
/// over.
#[derive(Clone)]
pub struct TaskHandle {
    commands: mpsc::Sender<Command>,
    ready: watch::Receiver<bool>,
}

/// Creates `tasks/` in every context store, opens `db` (created and migrated when needed),
/// starts the file watcher, then the task thread, which reconciles every task at once.
pub fn start(
    db: &Path,
    stores: &BTreeMap<ContextName, PathBuf>,
) -> Result<(TaskStore, TaskHandle)> {
    let mut files = Vec::with_capacity(stores.len());
    for (context, root) in stores {
        let store = FileStore::new(context.clone(), root.clone());
        store
            .init()
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot create the task store of context `{context}`"))?;
        files.push(store);
    }
    let index = TaskIndex::open(db).into_diagnostic()?;
    let (commands, received) = mpsc::channel();
    // Before the reconciliation: an edit made while it runs is seen by one or the other.
    let watcher = watch(stores, &commands);
    let _ = commands.send(Command::ReconcileAll);
    let (ready_sender, ready) = watch::channel(false);
    let worker = Worker {
        ready: Some(ready_sender),
        tasks: Tasks::new(files, index),
        stores: stores.clone(),
        origin: Origin {
            actor: Actor::File,
            machine_id: machine_id(),
        },
        random: OsRandom::open(),
    };
    info!(
        stores = ?stores.values().map(|root| root.display().to_string()).collect::<Vec<_>>(),
        machine = %worker.origin.machine_id.0,
        watching = watcher.is_some(),
        "task store started"
    );
    let thread = thread::Builder::new()
        .name("task-store".to_owned())
        .spawn(move || worker.run(&received))
        .into_diagnostic()
        .wrap_err("cannot start the task store thread")?;
    Ok((
        TaskStore {
            commands: commands.clone(),
            thread,
            watcher,
        },
        TaskHandle { commands, ready },
    ))
}

/// Watches `tasks/` of every store and sends the tasks whose files change. `None`, with a
/// warning, when the watcher cannot start: hand edits are then picked up at the next start.
fn watch(
    stores: &BTreeMap<ContextName, PathBuf>,
    commands: &mpsc::Sender<Command>,
) -> Option<RecommendedWatcher> {
    // Events may name the canonical path (FSEvents resolves `/tmp` to `/private/tmp`): both
    // spellings of each root are matched.
    let mut roots: Vec<(ContextName, PathBuf)> = Vec::new();
    for (context, root) in stores {
        roots.push((context.clone(), root.clone()));
        if let Ok(canonical) = fs::canonicalize(root)
            && canonical != *root
        {
            roots.push((context.clone(), canonical));
        }
    }
    let sender = commands.clone();
    let handler = move |event: notify::Result<notify::Event>| match event {
        Ok(event) => {
            if event.need_rescan() {
                let _ = sender.send(Command::ReconcileAll);
                return;
            }
            if !relevant(event.kind) {
                return;
            }
            for path in &event.paths {
                if let Some((context, id)) = task_of_path(&roots, path) {
                    let _ = sender.send(Command::Changed { context, id });
                }
            }
        }
        Err(error) => {
            warn!(%error, "task file watcher error, reconciling every task");
            let _ = sender.send(Command::ReconcileAll);
        }
    };
    let mut watcher = match notify::recommended_watcher(handler) {
        Ok(watcher) => watcher,
        Err(error) => {
            warn!(%error, "cannot watch the task stores: hand edits are picked up at the next start");
            return None;
        }
    };
    for root in stores.values() {
        let tasks = root.join(loom_store::files::TASKS_DIR);
        if let Err(error) = watcher.watch(&tasks, RecursiveMode::Recursive) {
            warn!(path = %tasks.display(), %error, "cannot watch a task store");
        }
    }
    Some(watcher)
}

/// Whether an event of `kind` can change a file. Reads are not: loomd reads every file it
/// reconciles, and a read must not trigger another reconciliation.
const fn relevant(kind: EventKind) -> bool {
    match kind {
        // A file closed after a write (inotify `IN_CLOSE_WRITE`) is a change; opens and reads
        // are not.
        EventKind::Access(access) => matches!(access, AccessKind::Close(AccessMode::Write)),
        EventKind::Any
        | EventKind::Create(_)
        | EventKind::Modify(_)
        | EventKind::Remove(_)
        | EventKind::Other => true,
    }
}

/// The machine events are recorded on: the host name.
// TODO(#72 follow-up): read `~/.loom/machine.toml` once it exists (ADR-0013, decision 8).
fn machine_id() -> MachineId {
    let name = rustix::system::uname()
        .nodename()
        .to_string_lossy()
        .into_owned();
    MachineId(if name.is_empty() {
        "unknown".to_owned()
    } else {
        name
    })
}

/// Randomness of the IDs loomd draws: `/dev/urandom`, and the keys of the standard library's
/// hasher (seeded randomly per process) when it cannot be read. IDs need to be distinct, not
/// secret: every draw is checked against the IDs taken.
struct OsRandom {
    urandom: Option<File>,
    counter: u64,
}

impl OsRandom {
    fn open() -> Self {
        let urandom = File::open("/dev/urandom")
            .inspect_err(|error| warn!(%error, "cannot open /dev/urandom"))
            .ok();
        Self {
            urandom,
            counter: 0,
        }
    }
}

impl RandomSource for OsRandom {
    fn next_u64(&mut self) -> u64 {
        let mut bytes = [0; 8];
        if let Some(file) = &mut self.urandom
            && file.read_exact(&mut bytes).is_ok()
        {
            return u64::from_le_bytes(bytes);
        }
        self.counter += 1;
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(self.counter);
        hasher.finish()
    }
}

impl TaskStore {
    /// Stops the watcher, then the task thread after the command it is running; queued
    /// requests are answered `unavailable`.
    pub fn stop(mut self) {
        drop(self.watcher.take());
        let _ = self.commands.send(Command::Stop);
        if self.thread.join().is_err() {
            warn!("the task store thread panicked");
        }
    }
}

impl TaskHandle {
    /// Waits, at most `timeout`, for the reconciliation on start, so that a search does not
    /// answer from a tasks index still being filled. `false` when it is not over yet; the
    /// caller goes on with the index as it is.
    pub async fn wait_ready(&self, timeout: Duration) -> bool {
        let mut ready = self.ready.clone();
        // A thread gone without setting it (stopped, panicked) ends the wait as well.
        matches!(
            tokio::time::timeout(timeout, ready.wait_for(|ready| *ready)).await,
            Ok(Ok(_))
        )
    }

    /// Rebuilds the tasks index from the files.
    pub async fn reindex(&self) -> Result<ReindexResult, Failure> {
        let (reply, answer) = oneshot::channel();
        let stopped = || {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                unavailable("loomd is stopping"),
            )
        };
        self.commands
            .send(Command::Reindex {
                reply,
                parent: Span::current(),
            })
            .map_err(|_| stopped())?;
        answer.await.map_err(|_| stopped())?
    }
}

struct Worker {
    tasks: Tasks<FileStore>,
    /// Root of each context store, for the paths of the failures.
    stores: BTreeMap<ContextName, PathBuf>,
    /// Who hand edits are recorded as: the actor `file`, on this machine.
    origin: Origin,
    random: OsRandom,
    /// Set once the reconciliation on start is over, then dropped.
    ready: Option<watch::Sender<bool>>,
}

impl Worker {
    /// Runs the commands in order until `Stop` or until every sender is gone.
    fn run(mut self, received: &mpsc::Receiver<Command>) {
        while let Ok(command) = received.recv() {
            match command {
                Command::Reindex { reply, parent } => {
                    let _ = reply.send(self.reindex(&parent));
                }
                Command::ReconcileAll => {
                    self.reconcile_all();
                    // The first one is the reconciliation on start, queued before any other.
                    if let Some(ready) = self.ready.take() {
                        ready.send_replace(true);
                    }
                }
                Command::Changed { context, id } => self.reconcile(&context, &id),
                Command::Stop => return,
            }
        }
    }

    fn reindex(&mut self, parent: &Span) -> Result<ReindexResult, Failure> {
        let _span = info_span!(parent: parent, "tasks.reindex").entered();
        // A hand edit whose watcher event is still queued would be indexed by the rebuild,
        // and then look like loomd's own write: it is recorded first.
        let unreconciled = self.reconcile_all();
        match self.tasks.reindex() {
            Ok(mut reindexed) => {
                // A task that failed both is listed once, with its first error.
                reindexed.failed.splice(0..0, unreconciled);
                let result = reindex_result(&reindexed, &self.stores);
                for file in &result.invalid {
                    warn!(context = %file.context, id = %file.id, error = %file.error, "invalid task file");
                }
                for file in &result.failed {
                    warn!(path = %file.path.display(), error = %file.error, "cannot read a task, its row is kept");
                }
                info!(
                    tasks = result.tasks,
                    invalid = result.invalid.len(),
                    failed = result.failed.len(),
                    "tasks indexed"
                );
                Ok(result)
            }
            Err(error) => {
                warn!(%error, "cannot reindex the tasks");
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    internal(error.to_string()),
                ))
            }
        }
    }

    /// Reconciles every task; a task that fails is logged with its path and skipped. Returns
    /// the failures.
    fn reconcile_all(&mut self) -> Vec<TaskFailure> {
        let _span = info_span!("tasks.reconcile_all").entered();
        let now = now();
        match self
            .tasks
            .reconcile_all(now, &self.origin, &mut self.random)
        {
            Ok(report) => {
                let mut edited = 0;
                for (id, reconciled) in &report.results {
                    log_reconciled(id, reconciled);
                    if matches!(reconciled, Reconciled::Edited(_)) {
                        edited += 1;
                    }
                }
                for file in failed_files(&report.failed, &self.stores) {
                    warn!(path = %file.path.display(), error = %file.error, "cannot reconcile a task, skipped");
                }
                info!(
                    tasks = report.results.len(),
                    edited,
                    failed = report.failed.len(),
                    "tasks reconciled"
                );
                report.failed
            }
            Err(error) => {
                warn!(%error, "cannot drop the removed tasks from the index");
                Vec::new()
            }
        }
    }

    fn reconcile(&mut self, context: &ContextName, id: &TaskId) {
        let now = now();
        match self
            .tasks
            .reconcile(context, id, now, &self.origin, &mut self.random)
        {
            Ok(reconciled) => log_reconciled(id, &reconciled),
            Err(error) => warn!(%context, %id, %error, "cannot reconcile a task"),
        }
    }
}

fn log_reconciled(id: &TaskId, reconciled: &Reconciled) {
    match reconciled {
        Reconciled::Unchanged => {}
        Reconciled::Indexed => debug!(%id, "task indexed"),
        Reconciled::Edited(changes) => {
            let fields: Vec<&str> = changes.iter().map(loom_core::FieldChange::field).collect();
            info!(%id, ?fields, "hand edit recorded");
        }
        Reconciled::Removed => info!(%id, "removed task dropped from the index"),
    }
}

/// The current time; the epoch when the clock is out of range (events then sort first, which
/// the log tolerates).
fn now() -> Timestamp {
    timestamp(SystemTime::now()).unwrap_or(Timestamp::EPOCH)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use tokio::sync::watch;

    use super::TaskHandle;

    fn handle() -> (TaskHandle, watch::Sender<bool>) {
        let (commands, _) = mpsc::channel();
        let (sender, ready) = watch::channel(false);
        (TaskHandle { commands, ready }, sender)
    }

    fn wait(handle: &TaskHandle, timeout: Duration) -> bool {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(handle.wait_ready(timeout))
    }

    #[test]
    fn a_search_waits_for_the_reconciliation_on_start_at_most_the_timeout() {
        let (handle, sender) = handle();
        assert!(!wait(&handle, Duration::from_millis(10)));
        sender.send_replace(true);
        assert!(wait(&handle, Duration::ZERO));
        // Every clone sees it.
        let clone = handle.clone();
        drop(handle);
        assert!(wait(&clone, Duration::ZERO));
    }

    #[test]
    fn a_task_thread_gone_ends_the_wait() {
        let (handle, sender) = handle();
        drop(sender);
        assert!(!wait(&handle, Duration::from_secs(60)));
    }
}
