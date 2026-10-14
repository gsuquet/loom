//! The task store of loomd (ADR-0013): a thread owns the files of every context store and the
//! connection that writes the tasks of `index.db`, and runs their operations one at a time.
//!
//! At start, it reindexes the tasks from the files; `POST /v1/tasks/reindex` does it again.
//! Decisions are in `loom_store` and `loomd::task_index`; this module does the I/O.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use axum::http::StatusCode;
use loom_api::{ContextName, ReindexResult};
use loom_store::files::FileStore;
use loom_store::tasks::{TaskIndex, Tasks};
use loomd::api::{internal, unavailable};
use loomd::task_index::reindex_result;
use miette::{Context as _, IntoDiagnostic as _, Result};
use tokio::sync::oneshot;
use tracing::{Span, info, info_span, warn};

use crate::repos::Failure;

type Reply<T> = oneshot::Sender<Result<T, Failure>>;

/// A request to the task thread, with the span of the API request that sent it.
enum Command {
    /// `reply`: `None` for the reindex on start.
    Reindex {
        reply: Option<Reply<ReindexResult>>,
        parent: Span,
    },
    Stop,
}

/// The running task thread.
pub struct TaskStore {
    commands: mpsc::Sender<Command>,
    thread: JoinHandle<()>,
}

/// What the API handlers use: the command channel.
#[derive(Clone)]
pub struct TaskHandle {
    commands: mpsc::Sender<Command>,
}

/// Creates `tasks/` in every context store, opens `db` (created and migrated when needed), then
/// starts the task thread, which reindexes at once.
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
    let tasks = Tasks::new(files, index);
    let roots = stores.clone();
    info!(
        stores = ?stores.values().map(|root| root.display().to_string()).collect::<Vec<_>>(),
        "task store started"
    );
    let (commands, received) = mpsc::channel();
    // The reindex on start, queued before any request.
    let _ = commands.send(Command::Reindex {
        reply: None,
        parent: Span::none(),
    });
    let thread = thread::Builder::new()
        .name("task-store".to_owned())
        .spawn(move || {
            Worker {
                tasks,
                stores: roots,
            }
            .run(&received);
        })
        .into_diagnostic()
        .wrap_err("cannot start the task store thread")?;
    Ok((
        TaskStore {
            commands: commands.clone(),
            thread,
        },
        TaskHandle { commands },
    ))
}

impl TaskStore {
    /// Stops the task thread after the command it is running; queued requests are answered
    /// `unavailable`.
    pub fn stop(self) {
        let _ = self.commands.send(Command::Stop);
        if self.thread.join().is_err() {
            warn!("the task store thread panicked");
        }
    }
}

impl TaskHandle {
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
                reply: Some(reply),
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
}

impl Worker {
    /// Runs the commands in order until `Stop` or until every sender is gone.
    fn run(mut self, received: &mpsc::Receiver<Command>) {
        while let Ok(command) = received.recv() {
            match command {
                Command::Reindex { reply, parent } => {
                    let result = self.reindex(&parent);
                    if let Some(reply) = reply {
                        let _ = reply.send(result);
                    }
                }
                Command::Stop => return,
            }
        }
    }

    fn reindex(&mut self, parent: &Span) -> Result<ReindexResult, Failure> {
        let _span = info_span!(parent: parent, "tasks.reindex").entered();
        match self.tasks.reindex() {
            Ok(reindexed) => {
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
}
