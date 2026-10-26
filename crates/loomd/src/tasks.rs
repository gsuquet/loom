//! The task store of loomd (ADR-0013): a thread owns the files of every context store and the
//! connection that writes the tasks of `index.db`, and runs their operations one at a time.
//!
//! - At start, it reconciles every task with the index: new files are indexed, hand edits made
//!   while loomd was stopped are recorded, rows of removed tasks dropped.
//! - A file watcher on each `tasks/` and `archive/` directory sends the tasks whose files change; each is
//!   reconciled the same way. A change loomd made itself has the hash the index holds and is
//!   skipped.
//! - `POST /v1/tasks/reindex` reconciles every task, then rebuilds the task rows from the files.
//! - Creations, changes and undos of the API run here too, one at a time: two requests on one
//!   task never interleave, and none is lost. Their events are of the actor `user`. Event logs
//!   are read here, so that a read never sees an append in progress.
//! - The other reads (list, one task) go through a read connection to the index, on a
//!   blocking thread, without waiting for the task thread.
//! - Closed tasks move to the archive (ADR-0013, decision 10) in a pass right after the
//!   reconciliation on start, then every hour (`loomd::archiver`): each move is one commit.
//! - Each change is committed to the store's git repository right after it is written
//!   (`loomd::store_git`): a hand edit recorded as an event, a task removed, and at the end of a
//!   reconciliation of every task, whatever no other commit took. A commit that fails is logged
//!   and the store left pending: its next commit takes every change. The write is never undone.
//!
//! Decisions are in `loom_store`, `loomd::task_index` and `loomd::task_api`; this module does
//! the I/O.

use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use axum::http::StatusCode;
use loom_api::{
    ChangeEvent, ContextName, Date, FailedTaskFile, FocusList, GoResult, NewTask, ReindexResult,
    TaskDetail, TaskEvents, TaskId, TaskList, TaskStoreHealth, Timestamp,
};
use loom_config::schedules::Schedule;
use loom_core::{Actor, Event, MachineId, Origin, RandomSource};
use loom_store::files::FileStore;
use loom_store::port::TaskLocation;
use loom_store::tasks::{IndexedTask, Reconciled, TaskError, TaskFailure, TaskIndex, Tasks};
use loom_store::{Reader, StoreError};
use loomd::api::{internal, timestamp, unavailable};
use loomd::backup::{
    archived_message, catch_up_message, commit_message, hand_edit_message, moved_message,
    removed_message,
};
use loomd::events::Bus;
use loomd::recurring::{Outcome, plan};
use loomd::repo_update::GitRunner;
use loomd::store_git::{Committed, GitError, LocalStep, StoreCommits};
use loomd::task_api::{
    TEST_TODAY_ENV, TaskFilter, focus_list, go_result, linked_repo_slug, resolve, task_detail,
    task_list, task_problem,
};
use loomd::task_archive::{ARCHIVE_BATCH, archive_now};
use loomd::task_index::{
    WatchedPath, failed_files, reindex_result, task_store_health, watched_path,
};
use miette::{Context as _, IntoDiagnostic as _, Result};
use notify::event::{AccessKind, AccessMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use tokio::sync::{oneshot, watch};
use tracing::{Span, debug, info, info_span, warn};

use crate::archiver::{self, Archiver};
use crate::repos::Failure;

type Reply<T> = oneshot::Sender<Result<T, Failure>>;

/// The schedules of each context held here.
pub type Schedules = BTreeMap<ContextName, Vec<Schedule>>;

/// How long a read of the tasks index (a search that covers tasks, a list, a task, an ID prefix)
/// waits for the reconciliation and the archive pass on start before it answers from the tasks
/// indexed so far.
pub const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// A request to the task thread, with the span of the API request that sent it.
enum Command {
    Reindex {
        reply: Reply<ReindexResult>,
        parent: Span,
    },
    Create {
        new: Box<NewTask>,
        reply: Reply<TaskDetail>,
        parent: Span,
    },
    Apply {
        id: TaskId,
        command: loom_core::Command,
        reply: Reply<TaskDetail>,
        parent: Span,
    },
    Go {
        id: TaskId,
        reply: Reply<GoResult>,
        parent: Span,
    },
    Undo {
        id: TaskId,
        reply: Reply<TaskDetail>,
        parent: Span,
    },
    Move {
        id: TaskId,
        to: ContextName,
        reply: Reply<TaskDetail>,
        parent: Span,
    },
    Events {
        id: TaskId,
        reply: Reply<TaskEvents>,
        parent: Span,
    },
    /// Reconcile every task: at start, and when the watcher may have missed events.
    ReconcileAll,
    /// Move the closed tasks whose time has come to the archive.
    Archive,
    /// A file of this task changed.
    Changed {
        context: ContextName,
        id: TaskId,
    },
    /// Before a sync: create the store's repository if needed, commit every change.
    PrepareSync {
        context: ContextName,
        reply: mpsc::Sender<Result<(), GitError>>,
    },
    /// A sync fast-forwards the store to the commit `to`.
    FastForward {
        context: ContextName,
        to: String,
        reply: mpsc::Sender<Result<(), GitError>>,
    },
    /// Create the tasks of the schedules due on `today` (`loomd::recurring`).
    RunSchedules {
        schedules: Arc<Schedules>,
        today: Date,
    },
    /// Whether the file watcher runs, and the tasks that the last reconciliation could not read.
    Health {
        reply: oneshot::Sender<HealthState>,
    },
    /// The stores whose changes are not all committed, with the last failure.
    Pending {
        reply: oneshot::Sender<BTreeMap<ContextName, String>>,
    },
    Stop,
}

/// What the task thread knows about the health of the stores.
struct HealthState {
    watching: bool,
    unwatched: Vec<FailedTaskFile>,
    failed: Vec<FailedTaskFile>,
    stores: BTreeMap<ContextName, PathBuf>,
}

/// The running task thread and its file watcher.
pub struct TaskStore {
    commands: mpsc::Sender<Command>,
    thread: JoinHandle<()>,
    /// Dropped on stop: no more events.
    watcher: Option<RecommendedWatcher>,
    /// Asks for an archive pass every hour.
    archiver: Option<Archiver>,
}

/// What the API handlers use: the command channel, the index for reads, and whether the
/// reconciliation and the archive pass on start are over.
#[derive(Clone)]
pub struct TaskHandle {
    commands: mpsc::Sender<Command>,
    db: PathBuf,
    ready: watch::Receiver<bool>,
}

/// Creates `tasks/` and `archive/` in every context store, opens `db` (created and migrated when needed),
/// starts the file watcher, then the task thread, which reconciles every task at once. Events
/// are recorded on the machine `machine_id`; closed tasks are archived `archive_after` after
/// their closing, at start and every hour.
pub fn start(
    db: &Path,
    stores: &BTreeMap<ContextName, PathBuf>,
    machine_id: MachineId,
    archive_after: Duration,
    events: Arc<Bus>,
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
    // Set: the reconciliation on start is queued below, before any watcher event.
    let reconcile_queued = Arc::new(AtomicBool::new(true));
    // Before the reconciliation: an edit made while it runs is seen by one or the other.
    let (watcher, unwatched) = watch(stores, &commands, Arc::clone(&reconcile_queued));
    let _ = commands.send(Command::ReconcileAll);
    // After the reconciliation on start: the closings recorded by it count.
    let _ = commands.send(Command::Archive);
    let (ready_sender, ready) = watch::channel(false);
    let worker = Worker {
        ready: Some(ready_sender),
        tasks: Tasks::new(files, index),
        commits: StoreCommits::new(GitRunner::from_env(), stores.clone()),
        stores: stores.clone(),
        watching: watcher.is_some(),
        unwatched,
        failures: BTreeMap::new(),
        origin: Origin {
            actor: Actor::File,
            machine_id: machine_id.clone(),
        },
        user: Origin {
            actor: Actor::User,
            machine_id,
        },
        random: OsRandom::open(),
        archive_after,
        archive_pending: false,
        reconcile_queued,
        schedule_failures: BTreeMap::new(),
        events,
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
    let timer = commands.clone();
    let archiver = archiver::start(move || timer.send(Command::Archive).is_ok())
        .inspect_err(|error| warn!(%error, "closed tasks are archived at start only"))
        .ok();
    Ok((
        TaskStore {
            commands: commands.clone(),
            thread,
            watcher,
            archiver,
        },
        TaskHandle {
            commands,
            db: db.to_path_buf(),
            ready,
        },
    ))
}

/// Watches `tasks/` and `archive/` of every store and sends the tasks whose files change. `None`, with a
/// warning, when the watcher cannot start: hand edits are then picked up at the next start. The
/// directories that it could not watch (a missing directory, a permission, the inotify limit) are
/// returned with the error.
fn watch(
    stores: &BTreeMap<ContextName, PathBuf>,
    commands: &mpsc::Sender<Command>,
    reconcile_queued: Arc<AtomicBool>,
) -> (Option<RecommendedWatcher>, Vec<FailedTaskFile>) {
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
                queue_reconcile_all(&reconcile_queued, &sender);
                return;
            }
            if !relevant(event.kind) {
                return;
            }
            for path in &event.paths {
                match watched_path(&roots, path) {
                    WatchedPath::Task(context, id) => {
                        let _ = sender.send(Command::Changed { context, id });
                    }
                    // Rare: the first move of a year, a sync, a hand copy or removal.
                    WatchedPath::ArchiveYear => {
                        queue_reconcile_all(&reconcile_queued, &sender);
                    }
                    WatchedPath::Other => {}
                }
            }
        }
        Err(error) => {
            warn!(%error, "task file watcher error, reconciling every task");
            queue_reconcile_all(&reconcile_queued, &sender);
        }
    };
    let mut watcher = match notify::recommended_watcher(handler) {
        Ok(watcher) => watcher,
        Err(error) => {
            warn!(%error, "cannot watch the task stores: hand edits are picked up at the next start");
            return (None, Vec::new());
        }
    };
    let mut unwatched = Vec::new();
    for root in stores.values() {
        for dir in [loom_store::files::TASKS_DIR, loom_store::files::ARCHIVE_DIR] {
            let dir = root.join(dir);
            if let Err(error) = watcher.watch(&dir, RecursiveMode::Recursive) {
                warn!(path = %dir.display(), %error, "cannot watch a task store");
                unwatched.push(FailedTaskFile {
                    path: dir,
                    error: error.to_string(),
                });
            }
        }
    }
    (Some(watcher), unwatched)
}

/// Queues a reconciliation of every task, unless one is queued and not started yet: a burst of
/// watcher events (a sync of several archive years, coalesced events) costs at most two scans,
/// the one queued and one for what changes while it runs.
fn queue_reconcile_all(queued: &AtomicBool, sender: &mpsc::Sender<Command>) {
    if !queued.swap(true, Ordering::SeqCst) {
        let _ = sender.send(Command::ReconcileAll);
    }
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
        if let Some(archiver) = self.archiver.take() {
            archiver.stop();
        }
        drop(self.watcher.take());
        let _ = self.commands.send(Command::Stop);
        if self.thread.join().is_err() {
            warn!("the task store thread panicked");
        }
    }
}

impl TaskHandle {
    /// Runs `step` of a sync of the store of `context` on the task thread, and waits for it:
    /// called by the sync thread.
    ///
    /// # Errors
    ///
    /// The git failure, or a failure when the task thread is stopping.
    pub fn sync_step(&self, context: &ContextName, step: LocalStep<'_>) -> Result<(), GitError> {
        let (reply, answer) = mpsc::channel();
        let command = match step {
            LocalStep::Prepare => Command::PrepareSync {
                context: context.clone(),
                reply,
            },
            LocalStep::FastForward { to } => Command::FastForward {
                context: context.clone(),
                to: to.to_owned(),
                reply,
            },
        };
        let stopped = || GitError {
            step: "sync",
            error: "loomd is stopping".to_owned(),
            stderr_tail: String::new(),
        };
        self.commands.send(command).map_err(|_| stopped())?;
        answer.recv().map_err(|_| stopped())?
    }

    /// Queues a run of `schedules` for `today` on the task thread, after the commands before
    /// it (the reconciliation on start first); does not wait for it. `false` when the task
    /// thread is gone.
    pub fn run_schedules(&self, schedules: Arc<Schedules>, today: Date) -> bool {
        self.commands
            .send(Command::RunSchedules { schedules, today })
            .is_ok()
    }

    /// The problems of the task stores: invalid files, tasks that could not be read, and whether
    /// the file watcher runs.
    pub async fn health(&self) -> Result<TaskStoreHealth, Failure> {
        let (reply, answer) = oneshot::channel();
        let stopped = || {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                unavailable("loomd is stopping"),
            )
        };
        self.commands
            .send(Command::Health { reply })
            .map_err(|_| stopped())?;
        let state = answer.await.map_err(|_| stopped())?;
        // The read loads every row, as the other task endpoints do: the invalid ones are
        // picked out of it.
        self.read(move |rows| {
            Ok(task_store_health(
                state.watching,
                state.unwatched,
                state.failed,
                &rows,
                &state.stores,
            ))
        })
        .await
    }

    /// The stores whose changes are not all committed, with the last failure.
    pub async fn pending(&self) -> Result<BTreeMap<ContextName, String>, Failure> {
        let (reply, answer) = oneshot::channel();
        let stopped = || {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                unavailable("loomd is stopping"),
            )
        };
        self.commands
            .send(Command::Pending { reply })
            .map_err(|_| stopped())?;
        answer.await.map_err(|_| stopped())
    }

    /// Waits, at most `timeout`, for the reconciliation and the archive pass on start, so that a
    /// read does not answer from a tasks index still being filled. `false` when it is not over
    /// yet; the caller goes on with the index as it is.
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
        self.send(|reply, parent| Command::Reindex { reply, parent })
            .await
    }

    /// Creates a task.
    pub async fn create(&self, new: NewTask) -> Result<TaskDetail, Failure> {
        self.send(|reply, parent| Command::Create {
            new: Box::new(new),
            reply,
            parent,
        })
        .await
    }

    /// Applies `command` to the task with `id`.
    pub async fn apply(
        &self,
        id: TaskId,
        command: loom_core::Command,
    ) -> Result<TaskDetail, Failure> {
        self.send(|reply, parent| Command::Apply {
            id,
            command,
            reply,
            parent,
        })
        .await
    }

    /// Makes the task with `id` the active focus of its context.
    pub async fn go(&self, id: TaskId) -> Result<GoResult, Failure> {
        self.send(|reply, parent| Command::Go { id, reply, parent })
            .await
    }

    /// Moves the task with `id` to the store of context `to`.
    pub async fn move_task(&self, id: TaskId, to: ContextName) -> Result<TaskDetail, Failure> {
        self.send(|reply, parent| Command::Move {
            id,
            to,
            reply,
            parent,
        })
        .await
    }

    /// Reverts the last change of the task with `id`.
    pub async fn undo(&self, id: TaskId) -> Result<TaskDetail, Failure> {
        self.send(|reply, parent| Command::Undo { id, reply, parent })
            .await
    }

    /// The event log of the task with `id`.
    pub async fn events(&self, id: TaskId) -> Result<TaskEvents, Failure> {
        self.send(|reply, parent| Command::Events { id, reply, parent })
            .await
    }

    /// The focused tasks of `context`, or of every context when `None`.
    pub async fn focus(&self, context: Option<ContextName>) -> Result<FocusList, Failure> {
        if !self.wait_ready(READY_TIMEOUT).await {
            warn!("tasks still being indexed: the answer may miss some");
        }
        let db = self.db.clone();
        let read = tokio::task::spawn_blocking(move || {
            let reader = Reader::open(&db)?;
            let rows = reader.tasks(None)?;
            let ids: Vec<TaskId> = rows.iter().map(|row| row.id.clone()).collect();
            let focused = reader.focus(context.as_ref())?;
            Ok(focus_list(&focused, &ids, |slug| {
                reader
                    .get(slug)
                    .ok()
                    .flatten()
                    .map(|summary| summary.identity.path)
            }))
        })
        .await;
        match read {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(index_failure(&error)),
            Err(error) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                internal(format!("index read failed: {error}")),
            )),
        }
    }

    /// The tasks `filter` keeps, ranked on `today` when it asks so.
    pub async fn list(&self, filter: TaskFilter, today: Date) -> Result<TaskList, Failure> {
        self.read(move |rows| Ok(task_list(&rows, &filter, today)))
            .await
    }

    /// The task whose ID starts with `prefix`.
    pub async fn get(&self, prefix: String) -> Result<TaskDetail, Failure> {
        self.read(move |rows| {
            let ids: Vec<TaskId> = rows.iter().map(|row| row.id.clone()).collect();
            resolve(&prefix, &rows).map(|row| task_detail(row, &ids))
        })
        .await
    }

    /// The ID of the task whose ID starts with `prefix`.
    pub async fn resolve(&self, prefix: String) -> Result<TaskId, Failure> {
        self.read(move |rows| resolve(&prefix, &rows).map(|row| row.id.clone()))
            .await
    }

    async fn send<T>(&self, command: impl FnOnce(Reply<T>, Span) -> Command) -> Result<T, Failure> {
        let (reply, answer) = oneshot::channel();
        let stopped = || {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                unavailable("loomd is stopping"),
            )
        };
        self.commands
            .send(command(reply, Span::current()))
            .map_err(|_| stopped())?;
        answer.await.map_err(|_| stopped())?
    }

    /// `decide` on every indexed task, read in one snapshot on a blocking thread.
    async fn read<T: Send + 'static>(
        &self,
        decide: impl FnOnce(Vec<IndexedTask>) -> Result<T, Failure> + Send + 'static,
    ) -> Result<T, Failure> {
        if !self.wait_ready(READY_TIMEOUT).await {
            warn!("tasks still being indexed: the answer may miss some");
        }
        let db = self.db.clone();
        let read = tokio::task::spawn_blocking(move || {
            Reader::open(&db)
                .and_then(|reader| reader.tasks(None))
                .map(decide)
        })
        .await;
        match read {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => Err(index_failure(&error)),
            Err(error) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                internal(format!("index read failed: {error}")),
            )),
        }
    }
}

fn index_failure(error: &StoreError) -> Failure {
    warn!(%error, "cannot read the tasks index");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        internal(error.to_string()),
    )
}

struct Worker {
    tasks: Tasks<FileStore>,
    /// The git repository of each store.
    commits: StoreCommits,
    /// Root of each context store, for the paths of the failures.
    stores: BTreeMap<ContextName, PathBuf>,
    /// The file watcher runs.
    watching: bool,
    /// The directories that the watcher could not watch.
    unwatched: Vec<FailedTaskFile>,
    /// The tasks (or stores) that the last read of them could not handle, by context and task.
    failures: BTreeMap<(ContextName, Option<TaskId>), TaskFailure>,
    /// Who hand edits are recorded as: the actor `file`, on this machine.
    origin: Origin,
    /// Who the changes of the API are recorded as: the actor `user`, on this machine.
    user: Origin,
    random: OsRandom,
    /// Set once the reconciliation and the archive pass on start are over, then dropped.
    ready: Option<watch::Sender<bool>>,
    /// `[tasks] archive_after`.
    archive_after: Duration,
    /// A pass reached [`ARCHIVE_BATCH`]: the next one runs once the queue is empty, after the
    /// requests waiting.
    archive_pending: bool,
    /// A reconciliation of every task is queued and not started yet: shared with the watcher,
    /// which then queues none more.
    reconcile_queued: Arc<AtomicBool>,
    /// The last failure of each schedule (context, ID) to create its task: a failure that
    /// repeats at each check is warned once.
    schedule_failures: BTreeMap<(ContextName, String), String>,
    /// Where each change is announced, once written and indexed (its commit attempted).
    events: Arc<Bus>,
}

impl Worker {
    /// The next command: the one waiting, else the archive pass left pending, else the next one
    /// to come. `None` once every sender is gone.
    fn next(&mut self, received: &mpsc::Receiver<Command>) -> Option<Command> {
        if self.archive_pending {
            match received.try_recv() {
                Ok(command) => return Some(command),
                Err(mpsc::TryRecvError::Empty) => {
                    self.archive_pending = false;
                    return Some(Command::Archive);
                }
                Err(mpsc::TryRecvError::Disconnected) => return None,
            }
        }
        received.recv().ok()
    }

    /// Runs the commands in order until `Stop` or until every sender is gone.
    fn run(mut self, received: &mpsc::Receiver<Command>) {
        while let Some(command) = self.next(received) {
            match command {
                Command::Reindex { reply, parent } => {
                    let _ = reply.send(self.reindex(&parent));
                }
                Command::Create { new, reply, parent } => {
                    let _ = reply.send(self.create(*new, &parent));
                }
                Command::Apply {
                    id,
                    command,
                    reply,
                    parent,
                } => {
                    let _ = reply.send(self.apply(&id, command, &parent));
                }
                Command::Go { id, reply, parent } => {
                    let _ = reply.send(self.go(&id, &parent));
                }
                Command::Undo { id, reply, parent } => {
                    let _ = reply.send(self.undo(&id, &parent));
                }
                Command::Move {
                    id,
                    to,
                    reply,
                    parent,
                } => {
                    let _ = reply.send(self.move_task(&id, &to, &parent));
                }
                Command::Events { id, reply, parent } => {
                    let _ = reply.send(self.events(&id, &parent));
                }
                Command::ReconcileAll => {
                    self.reconcile_all();
                }
                Command::Archive => {
                    self.archive();
                    // The first one is the pass on start, queued right after the reconciliation
                    // on start: reads then see the tasks indexed and archived.
                    if let Some(ready) = self.ready.take() {
                        ready.send_replace(true);
                    }
                }
                Command::Changed { context, id } => self.reconcile(&context, &id),
                Command::PrepareSync { context, reply } => {
                    let _ = reply.send(
                        self.commits
                            .prepare_sync(&context, &catch_up_message(&context)),
                    );
                }
                Command::FastForward { context, to, reply } => {
                    let result = self.commits.fast_forward(&context, &to);
                    // Commits of another machine came in: any task of the store may have
                    // changed, and the watcher may report them late or merged.
                    if result.is_ok() {
                        self.events.publish(ChangeEvent::Resync);
                    }
                    let _ = reply.send(result);
                }
                Command::RunSchedules { schedules, today } => {
                    self.run_schedules(&schedules, today);
                }
                Command::Health { reply } => {
                    let failed: Vec<TaskFailure> = self.failures.values().cloned().collect();
                    let _ = reply.send(HealthState {
                        watching: self.watching,
                        unwatched: self.unwatched.clone(),
                        failed: failed_files(&failed, &self.stores),
                        stores: self.stores.clone(),
                    });
                }
                Command::Pending { reply } => {
                    let _ = reply.send(
                        self.commits
                            .pending()
                            .iter()
                            .map(|(context, error)| (context.clone(), error.to_string()))
                            .collect(),
                    );
                }
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
                self.failures = failures_by_task(&reindexed.failed);
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
                self.events.publish(ChangeEvent::Resync);
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

    fn create(&mut self, new: NewTask, parent: &Span) -> Result<TaskDetail, Failure> {
        let _span = info_span!(parent: parent, "tasks.create", context = %new.context).entered();
        let task = self
            .tasks
            .create(new, now(), &self.user, &mut self.random)
            .map_err(|error| refused(&error))?;
        info!(id = %task.id, context = %task.context, "task created");
        self.commit_change(&task.context, &task.id, 0);
        self.changed(&task.context, &task.id);
        self.detail(&task.id)
    }

    fn apply(
        &mut self,
        id: &TaskId,
        command: loom_core::Command,
        parent: &Span,
    ) -> Result<TaskDetail, Failure> {
        let _span = info_span!(parent: parent, "tasks.apply", %id).entered();
        let before = self.event_count(id);
        let task = self
            .tasks
            .apply(id, command, now(), &self.user, &mut self.random)
            .map_err(|error| refused(&error))?;
        debug!(%id, "task changed");
        self.commit_change(&task.context, id, before);
        // A request that changes nothing (pin a pinned task) writes no event: nothing to tell.
        if self.event_count(id) > before {
            self.changed(&task.context, id);
        }
        self.detail(id)
    }

    fn go(&mut self, id: &TaskId, parent: &Span) -> Result<GoResult, Failure> {
        let _span = info_span!(parent: parent, "tasks.go", %id).entered();
        let before = self.event_count(id);
        let prev_counts: BTreeMap<TaskId, usize> = self
            .tasks
            .index()
            .focus(None)
            .unwrap_or_default()
            .into_iter()
            .map(|(focus, _)| {
                let count = self.event_count(&focus.task_id);
                (focus.task_id, count)
            })
            .collect();
        let outcome = self
            .tasks
            .go(id, now(), &self.user, &mut self.random)
            .map_err(|error| refused(&error))?;
        info!(%id, context = %outcome.task.context, "task focused");
        self.commit_change(&outcome.task.context, id, before);
        if self.event_count(id) > before {
            self.changed(&outcome.task.context, id);
        }
        if let Some(demoted) = &outcome.switched_from {
            let prev_before = prev_counts.get(&demoted.id).copied().unwrap_or(0);
            self.commit_change(&demoted.context, &demoted.id, prev_before);
            self.changed(&demoted.context, &demoted.id);
        }
        let index = self.tasks.index();
        let row = index
            .get(id)
            .map_err(|error| index_failure(&error))?
            .ok_or_else(|| refused(&TaskError::NotFound(id.clone())))?;
        let switched_row = outcome
            .switched_from
            .as_ref()
            .map(|demoted| index.get(&demoted.id))
            .transpose()
            .map_err(|error| index_failure(&error))?
            .flatten();
        let ids = index.ids().map_err(|error| index_failure(&error))?;
        let repo_path = linked_repo_slug(&outcome.task)
            .and_then(|slug| index.repo(slug).ok().flatten())
            .map(|summary| summary.identity.path);
        Ok(go_result(
            &row,
            switched_row.as_ref(),
            outcome.since,
            &ids,
            repo_path,
        ))
    }

    fn undo(&mut self, id: &TaskId, parent: &Span) -> Result<TaskDetail, Failure> {
        let _span = info_span!(parent: parent, "tasks.undo", %id).entered();
        let before = self.event_count(id);
        let task = self
            .tasks
            .undo(id, now(), &self.user, &mut self.random)
            .map_err(|error| refused(&error))?;
        info!(%id, "task change undone");
        self.commit_change(&task.context, id, before);
        // A request that changes nothing (pin a pinned task) writes no event: nothing to tell.
        if self.event_count(id) > before {
            self.changed(&task.context, id);
        }
        self.detail(id)
    }

    fn move_task(
        &mut self,
        id: &TaskId,
        to: &ContextName,
        parent: &Span,
    ) -> Result<TaskDetail, Failure> {
        let _span = info_span!(parent: parent, "tasks.move", %id, %to).entered();
        let before = self.event_count(id);
        let from = self
            .tasks
            .index()
            .get(id)
            .map_err(|error| index_failure(&error))?
            .map(|row| row.context);
        let task = self
            .tasks
            .move_to(id, to, now(), &self.user, &mut self.random)
            .map_err(|error| refused(&error))?;
        info!(%id, %to, "task moved");
        // The target store gets the task and its new events; the source store loses the task
        // directory, which may never have been committed there: every change is taken.
        self.commit_change(&task.context, id, before);
        if let Some(from) = from {
            let result = self.commits.commit_all(&from, &moved_message(id, to));
            log_commit(&from, Some(id), result);
            self.changed(&from, id);
        }
        self.changed(&task.context, id);
        self.detail(id)
    }

    fn events(&self, id: &TaskId, parent: &Span) -> Result<TaskEvents, Failure> {
        let _span = info_span!(parent: parent, "tasks.events", %id).entered();
        let log = self.tasks.events(id).map_err(|error| refused(&error))?;
        Ok(TaskEvents {
            id: id.clone(),
            events: log.events,
            skipped: log.skipped,
        })
    }

    /// Number of events in the log of task `id`; 0 when it cannot be read (the commit message
    /// then names the first event of the log).
    fn event_count(&self, id: &TaskId) -> usize {
        self.tasks.events(id).map_or(0, |log| log.events.len())
    }

    /// Commits the change an API request just wrote to task `id` of `context`: the events after
    /// the first `before` name the commit. A change that wrote no event commits nothing.
    fn commit_change(&mut self, context: &ContextName, id: &TaskId, before: usize) {
        let events = match self.tasks.events(id) {
            Ok(log) => log.events,
            Err(error) => {
                warn!(%id, %error, "cannot read the events of a change, committed later");
                return;
            }
        };
        let new: Vec<&Event> = events
            .iter()
            .skip(before)
            .map(|record| &record.event)
            .collect();
        let Some(message) = commit_message(id, &new) else {
            return;
        };
        // Archived tasks refuse every change: an API change is always of an active task.
        let result = self
            .commits
            .commit_task(context, id, TaskLocation::Active, &message);
        log_commit(context, Some(id), result);
    }

    /// The response of task `id`, as just indexed.
    fn detail(&mut self, id: &TaskId) -> Result<TaskDetail, Failure> {
        let index = self.tasks.index();
        let row = index
            .get(id)
            .map_err(|error| index_failure(&error))?
            .ok_or_else(|| refused(&TaskError::NotFound(id.clone())))?;
        let ids = index.ids().map_err(|error| index_failure(&error))?;
        Ok(task_detail(&row, &ids))
    }

    /// Reconciles every task; a task that fails is logged with its path and skipped. Returns
    /// the failures.
    fn reconcile_all(&mut self) -> Vec<TaskFailure> {
        let _span = info_span!("tasks.reconcile_all").entered();
        // Before the scan: an event during it queues one more.
        self.reconcile_queued.store(false, Ordering::SeqCst);
        let now = now();
        match self
            .tasks
            .reconcile_all(now, &self.origin, &mut self.random)
        {
            Ok(report) => {
                let mut edited = 0;
                for (id, reconciled) in &report.results {
                    log_reconciled(id, reconciled);
                    if let Reconciled::Edited(changes) = reconciled {
                        edited += 1;
                        self.commit_edit(id, changes);
                    }
                }
                // New, notes-only and removed tasks, and the changes of failed commits.
                let contexts: Vec<ContextName> = self.stores.keys().cloned().collect();
                for context in &contexts {
                    let result = self.commits.commit_all(context, &catch_up_message(context));
                    log_commit(context, None, result);
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
                // Any number of tasks may have changed: clients refetch everything.
                if report
                    .results
                    .iter()
                    .any(|(_, reconciled)| !matches!(reconciled, Reconciled::Unchanged))
                {
                    self.events.publish(ChangeEvent::Resync);
                }
                self.failures = failures_by_task(&report.failed);
                report.failed
            }
            Err(error) => {
                warn!(%error, "cannot drop the removed tasks from the index");
                Vec::new()
            }
        }
    }

    /// Moves the closed tasks whose time has come to the archive, one commit each; a task that
    /// cannot be moved is logged and stays.
    fn archive(&mut self) {
        let _span = info_span!("tasks.archive").entered();
        // Set again only by a pass that reaches its limit: a failing pass never loops.
        self.archive_pending = false;
        let now = archive_now(
            env::var(TEST_TODAY_ENV).ok().as_deref(),
            cfg!(debug_assertions),
            now(),
        );
        let report = match self
            .tasks
            .archive_due(now, self.archive_after, ARCHIVE_BATCH)
        {
            Ok(report) => report,
            Err(error) => {
                warn!(%error, "cannot archive the closed tasks");
                return;
            }
        };
        for moved in &report.moved {
            info!(context = %moved.context, id = %moved.id, year = moved.year, "task archived");
            let result = self.commits.commit_task(
                &moved.context,
                &moved.id,
                TaskLocation::Archived(moved.year),
                &archived_message(&moved.id, moved.year),
            );
            log_commit(&moved.context, Some(&moved.id), result);
            // After the move, its index row and its commit attempt, as for any change.
            self.events.publish(ChangeEvent::TaskArchived {
                id: moved.id.clone(),
                context: moved.context.clone(),
            });
        }
        for failure in &report.failed {
            warn!(
                context = %failure.context,
                id = failure.id.as_ref().map(TaskId::as_str),
                error = %failure.error,
                "cannot archive a task, it stays in tasks/"
            );
        }
        self.archive_pending = report.more;
        if !report.moved.is_empty() || !report.failed.is_empty() {
            info!(
                archived = report.moved.len(),
                failed = report.failed.len(),
                "closed tasks archived"
            );
        }
    }

    fn reconcile(&mut self, context: &ContextName, id: &TaskId) {
        let now = now();
        match self
            .tasks
            .reconcile(context, id, now, &self.origin, &mut self.random)
        {
            Ok(reconciled) => {
                self.failures.remove(&(context.clone(), Some(id.clone())));
                log_reconciled(id, &reconciled);
                let location = self.location(id);
                let result = match &reconciled {
                    Reconciled::Unchanged => return,
                    // Nothing was written to either copy: nothing to commit, the clients refetch.
                    Reconciled::Duplicate(contexts) => {
                        // The row is in the first context, not necessarily the watcher's.
                        self.changed(contexts.first().unwrap_or(context), id);
                        return;
                    }
                    Reconciled::Indexed | Reconciled::Withheld => {
                        self.commits
                            .commit_task(context, id, location, &hand_edit_message(id))
                    }
                    Reconciled::Edited(changes) => {
                        let event = Event::Edited {
                            changes: changes.clone(),
                        };
                        let message =
                            commit_message(id, &[&event]).unwrap_or_else(|| hand_edit_message(id));
                        self.commits.commit_task(context, id, location, &message)
                    }
                    // Its directory may never have been committed: every change is taken.
                    Reconciled::Removed => self.commits.commit_all(context, &removed_message(id)),
                };
                log_commit(context, Some(id), result);
                self.changed(context, id);
            }
            Err(error) => {
                warn!(%context, %id, %error, "cannot reconcile a task");
                self.failures.insert(
                    (context.clone(), Some(id.clone())),
                    TaskFailure {
                        context: context.clone(),
                        id: Some(id.clone()),
                        error: error.to_string(),
                    },
                );
            }
        }
    }
}

/// `failures`, one per context and task (the first error of a task stays).
fn failures_by_task(
    failures: &[TaskFailure],
) -> BTreeMap<(ContextName, Option<TaskId>), TaskFailure> {
    let mut by_task = BTreeMap::new();
    for failure in failures {
        by_task
            .entry((failure.context.clone(), failure.id.clone()))
            .or_insert_with(|| failure.clone());
    }
    by_task
}

/// The problem of a refused task operation; failures of the daemon are logged.
fn refused(error: &TaskError) -> Failure {
    let problem = task_problem(error);
    if problem.0.is_server_error() {
        warn!(%error, "task operation failed");
    }
    problem
}

impl Worker {
    /// Announces that task `id` of `context` changed: written and indexed, its commit attempted
    /// (a failed commit is retried later and does not hold the event).
    fn changed(&self, context: &ContextName, id: &TaskId) {
        self.events.publish(ChangeEvent::TaskChanged {
            id: id.clone(),
            context: context.clone(),
        });
    }

    /// Commits the hand edit of `changes` recorded on task `id`, found by a reconciliation of
    /// every task: its context is the one its index row names.
    fn commit_edit(&mut self, id: &TaskId, changes: &[loom_core::FieldChange]) {
        let (context, location) = match self.tasks.index().get(id) {
            Ok(Some(row)) => (row.context, row.location),
            // Left to the commit of every change that ends the reconciliation.
            Ok(None) => return,
            Err(error) => {
                warn!(%id, %error, "cannot find the context of an edited task, committed later");
                return;
            }
        };
        let event = Event::Edited {
            changes: changes.to_vec(),
        };
        let message = commit_message(id, &[&event]).unwrap_or_else(|| hand_edit_message(id));
        let result = self.commits.commit_task(&context, id, location, &message);
        log_commit(&context, Some(id), result);
    }

    /// Where task `id` is, as just indexed: active when it cannot be read (the commit then
    /// takes `tasks/<id>`, and the catch-up commit whatever is left).
    fn location(&mut self, id: &TaskId) -> TaskLocation {
        match self.tasks.index().get(id) {
            Ok(Some(row)) => row.location,
            Ok(None) => TaskLocation::Active,
            Err(error) => {
                warn!(%id, %error, "cannot read the location of a task");
                TaskLocation::Active
            }
        }
    }
}

impl Worker {
    /// Creates the task of each schedule due on `today`, and cancels the tasks it replaces.
    /// Events are of the actor `daemon`. A failure is logged; the next run tries again.
    fn run_schedules(&mut self, schedules: &Schedules, today: Date) {
        if schedules.values().all(Vec::is_empty) {
            return;
        }
        let _span = info_span!("tasks.schedules", %today).entered();
        let rows = match self.tasks.index().tasks() {
            Ok(rows) => rows,
            Err(error) => {
                warn!(%error, "cannot read the tasks index, schedules checked later");
                return;
            }
        };
        let daemon = Origin {
            actor: Actor::Daemon,
            machine_id: self.user.machine_id.clone(),
        };
        for (context, schedules) in schedules {
            for schedule in schedules {
                let id = schedule.id.get_ref();
                match plan(context, schedule, &rows, today) {
                    Outcome::NotDue => {}
                    Outcome::Skipped { occurrence, open } => debug!(
                        %context, schedule = %id, %occurrence, open = ?open.iter().map(TaskId::as_str).collect::<Vec<_>>(),
                        "occurrence skipped: a task of an earlier one is open"
                    ),
                    Outcome::Create {
                        occurrence,
                        new,
                        cancel,
                    } => {
                        let key = (context.clone(), id.as_str().to_owned());
                        let task = match self.tasks.create(*new, now(), &daemon, &mut self.random) {
                            Ok(task) => {
                                self.schedule_failures.remove(&key);
                                task
                            }
                            Err(error) => {
                                let failure = format!("{occurrence}: {error}");
                                if self.schedule_failures.get(&key) == Some(&failure) {
                                    debug!(%context, schedule = %id, %occurrence, %error, "cannot create the task of a schedule, again");
                                } else {
                                    warn!(%context, schedule = %id, %occurrence, %error, "cannot create the task of a schedule, retried every minute");
                                    self.schedule_failures.insert(key, failure);
                                }
                                continue;
                            }
                        };
                        info!(%context, schedule = %id, %occurrence, id = %task.id, "task of a schedule created");
                        self.commit_change(context, &task.id, 0);
                        self.changed(context, &task.id);
                        for previous in cancel {
                            self.cancel_replaced(context, &previous, &daemon);
                        }
                    }
                }
            }
        }
    }

    /// Cancels task `id` of `context`, replaced by the task of a newer occurrence.
    fn cancel_replaced(&mut self, context: &ContextName, id: &TaskId, daemon: &Origin) {
        let before = self.event_count(id);
        let command = loom_core::Command::SetStatus {
            status: loom_api::Status::Cancelled,
        };
        match self
            .tasks
            .apply(id, command, now(), daemon, &mut self.random)
        {
            Ok(_) => {
                info!(%context, %id, "task replaced by a newer occurrence cancelled");
                self.commit_change(context, id, before);
                // A task already cancelled writes no event: nothing to tell.
                if self.event_count(id) > before {
                    self.changed(context, id);
                }
            }
            Err(error) => {
                warn!(%context, %id, %error, "cannot cancel a task replaced by a newer occurrence");
            }
        }
    }
}

/// Logs the result of a commit of the store of `context`: a failure is a warning, the change
/// stays written and is committed later.
fn log_commit(context: &ContextName, id: Option<&TaskId>, result: Result<Committed, GitError>) {
    match result {
        Ok(Committed::Created) => debug!(%context, id = id.map(TaskId::as_str), "change committed"),
        Ok(Committed::Nothing) => {}
        Err(error) => warn!(
            %context,
            id = id.map(TaskId::as_str),
            %error,
            "cannot commit a change of the store: it stays written and is committed later"
        ),
    }
}

fn log_reconciled(id: &TaskId, reconciled: &Reconciled) {
    match reconciled {
        Reconciled::Unchanged => {}
        Reconciled::Indexed => debug!(%id, "task indexed"),
        Reconciled::Withheld => warn!(
            %id,
            "task indexed without an `edited` event: its events.jsonl has a line written by a newer loom, upgrade loom to record hand edits"
        ),
        Reconciled::Edited(changes) => {
            let fields: Vec<&str> = changes.iter().map(loom_core::FieldChange::field).collect();
            info!(%id, ?fields, "hand edit recorded");
        }
        Reconciled::Duplicate(contexts) => warn!(
            %id,
            ?contexts,
            "duplicate task ID across contexts: the task is read-only until one copy is removed"
        ),
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

    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{Command, TaskHandle, queue_reconcile_all, watch as watch_stores};

    #[test]
    fn a_directory_that_cannot_be_watched_is_listed_with_its_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("work");
        std::fs::create_dir_all(root.join("tasks")).unwrap();
        // No `archive/`: the watcher cannot watch it.
        let stores = std::collections::BTreeMap::from([("work".parse().unwrap(), root.clone())]);
        let (commands, _received) = mpsc::channel();
        let (watcher, unwatched) = watch_stores(
            &stores,
            &commands,
            std::sync::Arc::new(AtomicBool::new(false)),
        );
        assert!(watcher.is_some());
        assert_eq!(unwatched.len(), 1);
        assert_eq!(unwatched[0].path, root.join("archive"));
        assert_ne!(unwatched[0].error, "");

        std::fs::create_dir(root.join("archive")).unwrap();
        let (_watcher, unwatched) = watch_stores(
            &stores,
            &commands,
            std::sync::Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(unwatched.len(), 0);
    }

    #[test]
    fn a_burst_of_events_queues_one_reconciliation_until_it_starts() {
        let (sender, received) = mpsc::channel();
        let queued = AtomicBool::new(false);
        for _ in 0..3 {
            queue_reconcile_all(&queued, &sender);
        }
        assert!(matches!(received.try_recv(), Ok(Command::ReconcileAll)));
        assert!(received.try_recv().is_err());
        // Started: the next event queues another one.
        queued.store(false, Ordering::SeqCst);
        queue_reconcile_all(&queued, &sender);
        queue_reconcile_all(&queued, &sender);
        assert!(matches!(received.try_recv(), Ok(Command::ReconcileAll)));
        assert!(received.try_recv().is_err());
    }

    fn handle() -> (TaskHandle, watch::Sender<bool>) {
        let (commands, _) = mpsc::channel();
        let (sender, ready) = watch::channel(false);
        let db = std::path::PathBuf::new();
        (
            TaskHandle {
                commands,
                db,
                ready,
            },
            sender,
        )
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
