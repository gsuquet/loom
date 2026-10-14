//! The task store of loomd (ADR-0013): a thread owns the files of every context store and the
//! connection that writes the tasks of `index.db`, and runs their operations one at a time.
//!
//! - At start, it reconciles every task with the index: new files are indexed, hand edits made
//!   while loomd was stopped are recorded, rows of removed tasks dropped.
//! - A file watcher on each `tasks/` directory sends the tasks whose files change; each is
//!   reconciled the same way. A change loomd made itself has the hash the index holds and is
//!   skipped.
//! - `POST /v1/tasks/reindex` reconciles every task, then rebuilds the task rows from the files.
//! - Creations, changes and undos of the API run here too, one at a time: two requests on one
//!   task never interleave, and none is lost. Their events are of the actor `user`. Event logs
//!   are read here, so that a read never sees an append in progress.
//! - The other reads (list, one task) go through a read connection to the index, on a
//!   blocking thread, without waiting for the task thread.
//! - Each change is committed to the store's git repository right after it is written
//!   (`loomd::store_git`): a hand edit recorded as an event, a task removed, and at the end of a
//!   reconciliation of every task, whatever no other commit took. A commit that fails is logged
//!   and the store left pending: its next commit takes every change. The write is never undone.
//!
//! Decisions are in `loom_store`, `loomd::task_index` and `loomd::task_api`; this module does
//! the I/O.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use axum::http::StatusCode;
use loom_api::{
    ContextName, Date, NewTask, ReindexResult, TaskDetail, TaskEvents, TaskId, TaskList, Timestamp,
};
use loom_core::{Actor, Event, MachineId, Origin, RandomSource};
use loom_store::files::FileStore;
use loom_store::tasks::{IndexedTask, Reconciled, TaskError, TaskFailure, TaskIndex, Tasks};
use loom_store::{Reader, StoreError};
use loomd::api::{internal, timestamp, unavailable};
use loomd::backup::{catch_up_message, commit_message, hand_edit_message, removed_message};
use loomd::repo_update::GitRunner;
use loomd::store_git::{Committed, GitError, LocalStep, StoreCommits};
use loomd::task_api::{TaskFilter, resolve, task_detail, task_list, task_problem};
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
    Undo {
        id: TaskId,
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
    /// The stores whose changes are not all committed, with the last failure.
    Pending {
        reply: oneshot::Sender<BTreeMap<ContextName, String>>,
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

/// What the API handlers use: the command channel, the index for reads, and whether the
/// reconciliation on start is over.
#[derive(Clone)]
pub struct TaskHandle {
    commands: mpsc::Sender<Command>,
    db: PathBuf,
    ready: watch::Receiver<bool>,
}

/// Creates `tasks/` in every context store, opens `db` (created and migrated when needed),
/// starts the file watcher, then the task thread, which reconciles every task at once. Events
/// are recorded on the machine `machine_id`.
pub fn start(
    db: &Path,
    stores: &BTreeMap<ContextName, PathBuf>,
    machine_id: MachineId,
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
        commits: StoreCommits::new(GitRunner::from_env(), stores.clone()),
        stores: stores.clone(),
        origin: Origin {
            actor: Actor::File,
            machine_id: machine_id.clone(),
        },
        user: Origin {
            actor: Actor::User,
            machine_id,
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
        TaskHandle {
            commands,
            db: db.to_path_buf(),
            ready,
        },
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
    /// Who hand edits are recorded as: the actor `file`, on this machine.
    origin: Origin,
    /// Who the changes of the API are recorded as: the actor `user`, on this machine.
    user: Origin,
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
                Command::Undo { id, reply, parent } => {
                    let _ = reply.send(self.undo(&id, &parent));
                }
                Command::Events { id, reply, parent } => {
                    let _ = reply.send(self.events(&id, &parent));
                }
                Command::ReconcileAll => {
                    self.reconcile_all();
                    // The first one is the reconciliation on start, queued before any other.
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
                    let _ = reply.send(self.commits.fast_forward(&context, &to));
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

    fn create(&mut self, new: NewTask, parent: &Span) -> Result<TaskDetail, Failure> {
        let _span = info_span!(parent: parent, "tasks.create", context = %new.context).entered();
        let task = self
            .tasks
            .create(new, now(), &self.user, &mut self.random)
            .map_err(|error| refused(&error))?;
        info!(id = %task.id, context = %task.context, "task created");
        self.commit_change(&task.context, &task.id, 0);
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
        self.detail(id)
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
        let result = self.commits.commit_task(context, id, &message);
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
            Ok(reconciled) => {
                log_reconciled(id, &reconciled);
                let result = match &reconciled {
                    Reconciled::Unchanged => return,
                    Reconciled::Indexed => {
                        self.commits
                            .commit_task(context, id, &hand_edit_message(id))
                    }
                    Reconciled::Edited(changes) => {
                        let event = Event::Edited {
                            changes: changes.clone(),
                        };
                        let message =
                            commit_message(id, &[&event]).unwrap_or_else(|| hand_edit_message(id));
                        self.commits.commit_task(context, id, &message)
                    }
                    // Its directory may never have been committed: every change is taken.
                    Reconciled::Removed => self.commits.commit_all(context, &removed_message(id)),
                };
                log_commit(context, Some(id), result);
            }
            Err(error) => warn!(%context, %id, %error, "cannot reconcile a task"),
        }
    }
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
    /// Commits the hand edit of `changes` recorded on task `id`, found by a reconciliation of
    /// every task: its context is the one its index row names.
    fn commit_edit(&mut self, id: &TaskId, changes: &[loom_core::FieldChange]) {
        let context = match self.tasks.index().get(id) {
            Ok(Some(row)) => row.context,
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
        let result = self.commits.commit_task(&context, id, &message);
        log_commit(&context, Some(id), result);
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
