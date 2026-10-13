//! The tasks of the context stores in the index: rows built from the files ([`IndexedTask`]),
//! their writer ([`TaskIndex`]), and [`Tasks`], which writes a task through its store and
//! indexes it in the same call.
//!
//! The files are the source of truth: [`Tasks::reindex`] rebuilds every row from them, and two
//! reindexes of the same files give the same rows.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use loom_core::{
    Command, ContextName, DomainError, Event, EventId, EventRecord, FieldChange, Importance, Link,
    NewTask, Origin, RandomSource, Status, Task, TaskDocument, TaskId, Timestamp, apply, create,
};
use rusqlite::{Connection, OptionalExtension as _, Row, params};
use thiserror::Error;
use tracing::debug_span;

use crate::port::{ContentHash, InvalidTask, PortError, StorePort, StoredTask, TaskWrite};
use crate::reconcile::{Reconciliation, decide};
use crate::search::{expressions, terms};
use crate::{StoreError, open_writer};

/// A task as indexed: the task and its notes, or why its file is invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedTask {
    /// ID.
    pub id: TaskId,
    /// Context of its store.
    pub context: ContextName,
    /// Hash of `task.md` as last read or written by loomd.
    pub hash: ContentHash,
    /// What the file holds.
    pub state: IndexedState,
}

/// What an indexed `task.md` holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexedState {
    /// A valid file.
    Valid {
        /// The task, `updated` taken from its last event.
        task: Box<Task>,
        /// The notes after the frontmatter.
        body: String,
    },
    /// A file that cannot be read: shown read-only, never rewritten.
    Invalid {
        /// Why, with the line.
        error: String,
        /// The whole file.
        text: String,
    },
}

impl IndexedTask {
    /// The row of `stored`.
    #[must_use]
    pub fn from_stored(stored: &StoredTask) -> Self {
        let state = match (stored.task(), &stored.file.document) {
            (Ok(task), Ok(document)) => IndexedState::Valid {
                task: Box::new(task),
                body: document.body().to_owned(),
            },
            (Err(error), _) | (_, Err(error)) => IndexedState::Invalid {
                error: error.to_string(),
                text: stored.file.text.clone(),
            },
        };
        Self {
            id: stored.id.clone(),
            context: stored.context.clone(),
            hash: stored.file.hash.clone(),
            state,
        }
    }

    /// The title; empty for an invalid file.
    #[must_use]
    pub fn title(&self) -> &str {
        match &self.state {
            IndexedState::Valid { task, .. } => &task.title,
            IndexedState::Invalid { .. } => "",
        }
    }

    /// Keys of the origin and related links, separated by spaces; empty for an invalid file.
    #[must_use]
    pub fn link_keys(&self) -> String {
        match &self.state {
            IndexedState::Valid { task, .. } => link_keys(&task.links),
            IndexedState::Invalid { .. } => String::new(),
        }
    }

    /// Whether the task cannot be changed: its file is invalid, or its kind, format or status
    /// was written by a newer version of loom.
    #[must_use]
    pub fn read_only(&self) -> bool {
        match &self.state {
            IndexedState::Valid { task, .. } => task.read_only_reason().is_some(),
            IndexedState::Invalid { .. } => true,
        }
    }
}

/// Keys of the origin and related links of `links`, in order, separated by spaces.
#[must_use]
pub fn link_keys(links: &[Link]) -> String {
    links
        .iter()
        .filter_map(|link| match link {
            Link::Origin(external) | Link::Related(external) => Some(external.key.as_str()),
            Link::Repo { .. } => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The writer of the tasks of the index: the only connection that changes them.
#[derive(Debug)]
pub struct TaskIndex {
    conn: Connection,
}

impl TaskIndex {
    /// Opens the index at `path`, creating and migrating it when needed (see
    /// [`crate::Store::open`]).
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the file cannot be created, opened or migrated.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let _span = debug_span!("task_index.open", path = %path.display()).entered();
        Ok(Self {
            conn: open_writer(path)?,
        })
    }

    /// Adds or replaces the row of `task`.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the write fails.
    pub fn upsert(&mut self, task: &IndexedTask) -> Result<(), StoreError> {
        insert(&self.conn, task)
    }

    /// Removes the row of task `id`; `false` when there was none.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the write fails.
    pub fn remove(&mut self, id: &TaskId) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .execute("DELETE FROM tasks WHERE id = ?1", [id.as_str()])?
            > 0)
    }

    /// Replaces every task row with `tasks`, in one transaction, except the rows that `keep`
    /// says to leave as they are (tasks that could not be read). The repositories are left as
    /// they are.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when a write fails; nothing is changed then.
    pub fn replace_all(
        &mut self,
        tasks: &[IndexedTask],
        keep: impl Fn(&ContextName, &TaskId) -> bool,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        let existing = list(&tx, None)?;
        for row in existing {
            if !keep(&row.context, &row.id) {
                tx.execute("DELETE FROM tasks WHERE id = ?1", [row.id.as_str()])?;
            }
        }
        for task in tasks {
            insert(&tx, task)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The indexed task with `id`.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the query fails or the row is invalid.
    pub fn get(&self, id: &TaskId) -> Result<Option<IndexedTask>, StoreError> {
        get(&self.conn, id)
    }

    /// Every indexed task ID, sorted.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the query fails or an ID is invalid.
    pub fn ids(&self) -> Result<Vec<TaskId>, StoreError> {
        let mut statement = self.conn.prepare("SELECT id FROM tasks ORDER BY id")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|id| parse_id(&id?)).collect()
    }
}

/// Why a task operation failed.
#[derive(Debug, Error)]
pub enum TaskError {
    /// No store holds this context.
    #[error("no task store for context `{0}`: add a [contexts.{0}] table")]
    UnknownContext(ContextName),
    /// No task has this ID.
    #[error("no task `{0}`")]
    NotFound(TaskId),
    /// The task file is invalid: the task is read-only until it is fixed.
    #[error("the task file is invalid, fix it by hand: {0}")]
    Invalid(InvalidTask),
    /// The command was refused.
    #[error(transparent)]
    Domain(#[from] DomainError),
    /// The task files cannot be read or written.
    #[error(transparent)]
    Files(#[from] PortError),
    /// The index cannot be read or written.
    #[error(transparent)]
    Index(#[from] StoreError),
}

/// The tasks of every context store, written through their store and indexed at once.
#[derive(Debug)]
pub struct Tasks<P> {
    stores: BTreeMap<ContextName, P>,
    index: TaskIndex,
}

impl<P: StorePort> Tasks<P> {
    /// The tasks of `stores`, one per context, indexed in `index`.
    #[must_use]
    pub fn new(stores: Vec<P>, index: TaskIndex) -> Self {
        Self {
            stores: stores
                .into_iter()
                .map(|store| (store.context().clone(), store))
                .collect(),
            index,
        }
    }

    /// The store of each context.
    #[must_use]
    pub const fn stores(&self) -> &BTreeMap<ContextName, P> {
        &self.stores
    }

    /// The writer of the index.
    pub const fn index(&mut self) -> &mut TaskIndex {
        &mut self.index
    }

    /// Creates a task at `now`, with an ID drawn from `random` and free in every context, and
    /// indexes it.
    ///
    /// # Errors
    ///
    /// [`TaskError::UnknownContext`], the refusal of [`loom_core::create`], or the write error.
    pub fn create(
        &mut self,
        new: NewTask,
        now: Timestamp,
        origin: &Origin,
        random: &mut impl RandomSource,
    ) -> Result<Task, TaskError> {
        let store = self
            .stores
            .get(&new.context)
            .ok_or_else(|| TaskError::UnknownContext(new.context.clone()))?;
        let taken = self.index.ids()?;
        let (task, events) = create(new, random, &taken, now, origin)?;
        store.create(&TaskWrite {
            document: Some(TaskDocument::new(&task)),
            events,
        })?;
        self.index
            .upsert(&IndexedTask::from_stored(&store.read(&task.id)?))?;
        Ok(task)
    }

    /// Applies `command` to task `id` at `now` (see [`loom_core::apply`]), writes and indexes
    /// the result. A command that changes nothing writes nothing.
    ///
    /// # Errors
    ///
    /// [`TaskError::NotFound`], [`TaskError::Invalid`] for an invalid file, the refusal of the
    /// command, or the write error.
    pub fn apply(
        &mut self,
        id: &TaskId,
        command: Command,
        now: Timestamp,
        origin: &Origin,
        random: &mut impl RandomSource,
    ) -> Result<Task, TaskError> {
        let indexed = self
            .index
            .get(id)?
            .ok_or_else(|| TaskError::NotFound(id.clone()))?;
        let store = self
            .stores
            .get(&indexed.context)
            .ok_or_else(|| TaskError::UnknownContext(indexed.context.clone()))?;
        let task = store.update(id, |stored| {
            let current = stored
                .task()
                .map_err(|error| TaskError::Invalid(error.clone()))?;
            let mut document = stored.file.document.map_err(TaskError::Invalid)?;
            let (task, events) =
                apply(current, command, &stored.events.events, now, origin, random)?;
            if events.is_empty() {
                return Ok((TaskWrite::default(), task));
            }
            document.set_task(&task).map_err(DomainError::ReadOnly)?;
            Ok::<_, TaskError>((
                TaskWrite {
                    document: Some(document),
                    events,
                },
                task,
            ))
        })?;
        self.index
            .upsert(&IndexedTask::from_stored(&store.read(id)?))?;
        Ok(task)
    }

    /// Rebuilds the task rows from the files of every store, in one transaction: rows of tasks
    /// whose directory is gone are dropped, the repositories are left as they are.
    ///
    /// A task (or a whole store) that cannot be read is reported in [`Reindexed::failed`] and
    /// keeps the row it had; the others are rebuilt.
    ///
    /// # Errors
    ///
    /// [`TaskError::Index`] when the index cannot be written; it is unchanged then.
    pub fn reindex(&mut self) -> Result<Reindexed, TaskError> {
        let _span = debug_span!("tasks.reindex").entered();
        let mut rows = Vec::new();
        let mut failed = Vec::new();
        for (context, store) in &self.stores {
            let ids = match store.list() {
                Ok(ids) => ids,
                Err(error) => {
                    failed.push(TaskFailure::new(context, None, &error.into()));
                    continue;
                }
            };
            for id in ids {
                match store.read(&id) {
                    Ok(stored) => rows.push(IndexedTask::from_stored(&stored)),
                    // Removed since it was listed.
                    Err(PortError::NotFound { .. }) => {}
                    Err(error) => failed.push(TaskFailure::new(context, Some(&id), &error.into())),
                }
            }
        }
        self.index
            .replace_all(&rows, |context, id| keeps(&failed, context, id))?;
        Ok(Reindexed { rows, failed })
    }

    /// Brings the index in line with the files of task `id` in the store of `context` after a
    /// change loomd may not have made (see [`decide`]). A hand edit of the task's fields is
    /// appended to its log as an `edited` event of `origin` (the actor `file`) at `now`, its ID
    /// drawn from `random`; `task.md` itself is never written.
    ///
    /// # Errors
    ///
    /// [`TaskError::UnknownContext`], or the read, write or index error.
    pub fn reconcile(
        &mut self,
        context: &ContextName,
        id: &TaskId,
        now: Timestamp,
        origin: &Origin,
        random: &mut impl RandomSource,
    ) -> Result<Reconciled, TaskError> {
        let store = self
            .stores
            .get(context)
            .ok_or_else(|| TaskError::UnknownContext(context.clone()))?;
        let indexed = self.index.get(id)?;
        let hash = indexed
            .as_ref()
            .filter(|row| row.context == *context)
            .map(|row| row.hash.clone());
        let result = store.update(id, |mut stored| match decide(hash.as_ref(), &stored) {
            Reconciliation::Unchanged => Ok((TaskWrite::default(), (Reconciled::Unchanged, None))),
            Reconciliation::Index => {
                Ok((TaskWrite::default(), (Reconciled::Indexed, Some(stored))))
            }
            Reconciliation::Record(changes) => {
                let taken: Vec<EventId> = stored
                    .events
                    .events
                    .iter()
                    .map(|event| event.id.clone())
                    .collect();
                let event_id = EventId::generate(random, &taken).map_err(DomainError::from)?;
                let event = Event::Edited {
                    changes: changes.clone(),
                };
                let record = EventRecord::new(event_id, event, now, origin);
                stored.events.events.push(record.clone());
                let write = TaskWrite {
                    document: None,
                    events: vec![record],
                };
                Ok::<_, TaskError>((write, (Reconciled::Edited(changes), Some(stored))))
            }
        });
        match result {
            Ok((reconciled, stored)) => {
                if let Some(stored) = stored {
                    self.index.upsert(&IndexedTask::from_stored(&stored))?;
                }
                Ok(reconciled)
            }
            Err(TaskError::Files(PortError::NotFound { .. })) => {
                if indexed.is_some() && self.index.remove(id)? {
                    Ok(Reconciled::Removed)
                } else {
                    Ok(Reconciled::Unchanged)
                }
            }
            Err(error) => Err(error),
        }
    }

    /// [`Tasks::reconcile`] of every task of every store, then removes the rows of the tasks
    /// that are in no store any more.
    ///
    /// A task that cannot be reconciled, or a store that cannot be listed, is reported in
    /// [`ReconciledAll::failed`] and keeps its rows; the other tasks are reconciled.
    ///
    /// # Errors
    ///
    /// [`TaskError::Index`] when the rows left to remove cannot be read or removed.
    pub fn reconcile_all(
        &mut self,
        now: Timestamp,
        origin: &Origin,
        random: &mut impl RandomSource,
    ) -> Result<ReconciledAll, TaskError> {
        let _span = debug_span!("tasks.reconcile_all").entered();
        let mut listed = Vec::new();
        let mut failed = Vec::new();
        for (context, store) in &self.stores {
            match store.list() {
                Ok(ids) => listed.extend(ids.into_iter().map(|id| (context.clone(), id))),
                Err(error) => failed.push(TaskFailure::new(context, None, &error.into())),
            }
        }
        let mut results = Vec::with_capacity(listed.len());
        let mut seen = HashSet::with_capacity(listed.len());
        for (context, id) in listed {
            match self.reconcile(&context, &id, now, origin, random) {
                Ok(reconciled) => results.push((id.clone(), reconciled)),
                Err(error) => failed.push(TaskFailure::new(&context, Some(&id), &error)),
            }
            seen.insert(id);
        }
        for row in list(&self.index.conn, None)? {
            if !seen.contains(&row.id)
                && !keeps(&failed, &row.context, &row.id)
                && self.index.remove(&row.id)?
            {
                results.push((row.id, Reconciled::Removed));
            }
        }
        Ok(ReconciledAll { results, failed })
    }
}

/// What [`Tasks::reconcile_all`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciledAll {
    /// What happened to each task, in store order, then the removed rows.
    pub results: Vec<(TaskId, Reconciled)>,
    /// The tasks, or stores, that could not be reconciled: their rows are kept as they were.
    pub failed: Vec<TaskFailure>,
}

/// What [`Tasks::reconcile`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciled {
    /// The file is the one last indexed: nothing done.
    Unchanged,
    /// The file was indexed (new, notes changed, invalid, or a log that cannot be replayed),
    /// without an event.
    Indexed,
    /// A hand edit of these fields was recorded, and the file indexed.
    Edited(Vec<FieldChange>),
    /// The task is gone from its store: its row was removed.
    Removed,
}

/// What [`Tasks::reindex`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reindexed {
    /// The rows written, sorted by context and ID.
    pub rows: Vec<IndexedTask>,
    /// The tasks, or stores, that could not be read: their rows are kept as they were.
    pub failed: Vec<TaskFailure>,
}

/// A task, or a whole store, that an operation over every task could not handle. The
/// operation went on with the others.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFailure {
    /// Context of the store.
    pub context: ContextName,
    /// The task; `None` when the store itself could not be listed.
    pub id: Option<TaskId>,
    /// What failed, with the path when it is a file error.
    pub error: String,
}

impl TaskFailure {
    fn new(context: &ContextName, id: Option<&TaskId>, error: &TaskError) -> Self {
        Self {
            context: context.clone(),
            id: id.cloned(),
            error: error.to_string(),
        }
    }
}

/// Whether the row of task `id` in `context` is kept as it is, because the task or its whole
/// store could not be read.
fn keeps(failed: &[TaskFailure], context: &ContextName, id: &TaskId) -> bool {
    failed.iter().any(|failure| {
        failure.context == *context && failure.id.as_ref().is_none_or(|failed| failed == id)
    })
}

/// Columns of [`read_row`].
const COLUMNS: &str = "id, context, hash, task, error, body";

fn insert(conn: &Connection, task: &IndexedTask) -> Result<(), StoreError> {
    let (json, error, status, importance, due, pinned, updated, body) = match &task.state {
        IndexedState::Valid { task, body } => (
            Some(
                serde_json::to_string(task)
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?,
            ),
            None,
            Some(status_text(&task.status)),
            Some(importance_text(task.importance)),
            task.due.map(|due| due.to_string()),
            Some(i64::from(task.pinned)),
            Some(unix(task.updated)?),
            body.as_str(),
        ),
        IndexedState::Invalid { error, text } => (
            None,
            Some(error.as_str()),
            None,
            None,
            None,
            None,
            None,
            text.as_str(),
        ),
    };
    conn.execute(
        "INSERT INTO tasks (id, context, hash, task, error, title, status, importance, due, \
                            pinned, updated, body, link_keys) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
         ON CONFLICT (id) DO UPDATE SET \
             context = excluded.context, hash = excluded.hash, task = excluded.task, \
             error = excluded.error, title = excluded.title, status = excluded.status, \
             importance = excluded.importance, due = excluded.due, pinned = excluded.pinned, \
             updated = excluded.updated, body = excluded.body, link_keys = excluded.link_keys",
        params![
            task.id.as_str(),
            task.context.as_str(),
            task.hash.as_str(),
            json,
            error,
            task.title(),
            status,
            importance,
            due,
            pinned,
            updated,
            body,
            task.link_keys(),
        ],
    )?;
    Ok(())
}

/// The status as written in `task.md`.
fn status_text(status: &Status) -> String {
    match status {
        Status::Unknown(name) => name.clone(),
        Status::Inbox
        | Status::Ready
        | Status::Active
        | Status::Waiting { .. }
        | Status::Done
        | Status::Cancelled => status
            .name()
            .map(|name| name.to_string())
            .unwrap_or_default(),
    }
}

const fn importance_text(importance: Importance) -> &'static str {
    match importance {
        Importance::P0 => "p0",
        Importance::P1 => "p1",
        Importance::P2 => "p2",
        Importance::P3 => "p3",
    }
}

fn unix(timestamp: Timestamp) -> Result<i64, StoreError> {
    i64::try_from(timestamp.unix_secs())
        .map_err(|_| StoreError::Corrupt(format!("timestamp {timestamp} out of range")))
}

fn parse_id(text: &str) -> Result<TaskId, StoreError> {
    text.parse()
        .map_err(|error: loom_core::IdError| StoreError::Corrupt(error.to_string()))
}

/// A row of `tasks` ([`COLUMNS`]).
fn read_row(row: &Row<'_>) -> rusqlite::Result<Result<IndexedTask, StoreError>> {
    let id: String = row.get(0)?;
    let context: String = row.get(1)?;
    let hash: String = row.get(2)?;
    let json: Option<String> = row.get(3)?;
    let error: Option<String> = row.get(4)?;
    let body: String = row.get(5)?;
    Ok((|| {
        let corrupt = |message: String| StoreError::Corrupt(message);
        let state = match (json, error) {
            (Some(json), None) => IndexedState::Valid {
                task: serde_json::from_str(&json).map_err(|error| corrupt(error.to_string()))?,
                body,
            },
            (None, Some(error)) => IndexedState::Invalid { error, text: body },
            (json, error) => {
                return Err(corrupt(format!(
                    "task {id}: task {} and error {}",
                    json.is_some(),
                    error.is_some()
                )));
            }
        };
        Ok(IndexedTask {
            id: parse_id(&id)?,
            context: context
                .parse()
                .map_err(|error: loom_core::ContextNameError| corrupt(error.to_string()))?,
            hash: ContentHash::from_hex(hash),
            state,
        })
    })())
}

/// The indexed task with `id`.
pub(crate) fn get(conn: &Connection, id: &TaskId) -> Result<Option<IndexedTask>, StoreError> {
    let sql = format!("SELECT {COLUMNS} FROM tasks WHERE id = ?1");
    conn.query_row(&sql, [id.as_str()], read_row)
        .optional()?
        .transpose()
}

/// The indexed tasks of `context`, or of every context, sorted by ID.
pub(crate) fn list(
    conn: &Connection,
    context: Option<&ContextName>,
) -> Result<Vec<IndexedTask>, StoreError> {
    let sql = format!("SELECT {COLUMNS} FROM tasks WHERE ?1 IS NULL OR context = ?1 ORDER BY id");
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map([context.map(ContextName::as_str)], read_row)?;
    rows.map(|row| row?).collect()
}

/// IDs of the tasks matching `query`, most relevant first (`bm25`, then ID), at most `limit`.
pub(crate) fn search(
    conn: &Connection,
    query: &str,
    context: Option<&ContextName>,
    limit: usize,
) -> Result<Vec<TaskId>, StoreError> {
    let Some(expressions) = expressions(&terms(query)) else {
        return Ok(Vec::new());
    };
    let mut statement = conn.prepare(
        "SELECT t.id FROM tasks_fts JOIN tasks t ON t.key = tasks_fts.rowid \
         WHERE tasks_fts MATCH ?1 AND (?2 IS NULL OR t.context = ?2) \
         ORDER BY bm25(tasks_fts), t.id LIMIT ?3",
    )?;
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let rows = statement.query_map(
        params![expressions.all, context.map(ContextName::as_str), limit],
        |row| row.get::<_, String>(0),
    )?;
    rows.map(|id| parse_id(&id?)).collect()
}

#[cfg(test)]
mod tests;
