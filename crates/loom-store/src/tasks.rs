//! The tasks of the context stores in the index: rows built from the files ([`IndexedTask`]),
//! their writer ([`TaskIndex`]), and [`Tasks`], which writes a task through its store and
//! indexes it in the same call.
//!
//! The files are the source of truth: [`Tasks::reindex`] rebuilds every row from them, and two
//! reindexes of the same files give the same rows.
//!
//! Archived tasks (ADR-0013, decision 10) are indexed as the others, with their archive year:
//! they are listed, searched and read, never changed. [`Tasks::archive_due`] moves the closed
//! tasks whose time has come.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::Duration;

use loom_core::{
    Actor, Command, ContextName, DomainError, Event, EventId, EventLog, EventRecord, FieldChange,
    Importance, Link, NewTask, Origin, ParentLink, RandomSource, Status, Task, TaskDocument,
    TaskId, Timestamp, apply, archive_year, check_parent, create, move_task, undo,
};
use rusqlite::{Connection, OptionalExtension as _, Row, params};
use thiserror::Error;
use tracing::{debug, debug_span, warn};

use crate::port::{
    ContentHash, InvalidTask, PortError, StorePort, StoredTask, TaskLocation, TaskWrite,
};
use crate::reconcile::{Reconciliation, decide, duplicate_ids};
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
    /// Where its directory is: active, or archived.
    pub location: TaskLocation,
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
            location: stored.location,
            state,
        }
    }

    /// The row of the ID of `first`, held by the stores of `contexts` (see [`duplicate_ids`]):
    /// invalid, in the context and with the file of `first`. Its hash is not the one of the
    /// file, so that reconciling the file after the other copies are gone indexes it again.
    #[must_use]
    pub fn duplicate(first: &StoredTask, contexts: &[ContextName]) -> Self {
        let error = InvalidTask::DuplicateId {
            id: first.id.clone(),
            contexts: contexts.to_vec(),
        };
        let marked = format!("duplicate {}", first.file.hash.as_str());
        Self {
            id: first.id.clone(),
            context: first.context.clone(),
            hash: ContentHash::of(marked.as_bytes()),
            location: first.location,
            state: IndexedState::Invalid {
                error: error.to_string(),
                text: first.file.text.clone(),
            },
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

    /// Whether the task cannot be changed: it is archived, its file is invalid, or its kind,
    /// format or status was written by a newer version of loom.
    #[must_use]
    pub fn read_only(&self) -> bool {
        if self.location != TaskLocation::Active {
            return true;
        }
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
        self.conn
            .execute("DELETE FROM focus WHERE task_id = ?1", [id.as_str()])?;
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
        tx.execute(
            "DELETE FROM focus \
             WHERE NOT EXISTS ( \
                 SELECT 1 FROM tasks t \
                 WHERE t.id = focus.task_id \
                   AND t.context = focus.context \
                   AND t.error IS NULL \
                   AND t.archived IS NULL \
                   AND t.status = 'active' \
             )",
            [],
        )?;
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

    /// Every indexed task, sorted by ID.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the query fails or a row is invalid.
    pub fn tasks(&self) -> Result<Vec<IndexedTask>, StoreError> {
        list(&self.conn, None)
    }

    /// The focused tasks of `context`, or of every context when `None`, sorted by context.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the query fails or a row is invalid.
    pub fn focus(
        &self,
        context: Option<&ContextName>,
    ) -> Result<Vec<(FocusRow, IndexedTask)>, StoreError> {
        focus(&self.conn, context)
    }

    /// The repository with `slug`.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the query fails or the row is invalid.
    pub fn repo(&self, slug: &str) -> Result<Option<loom_api::RepoSummary>, StoreError> {
        crate::repos::get(&self.conn, slug)
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
    /// The task is archived: read-only.
    #[error("task `{id}` is archived (archive/{year}): an archived task is read-only")]
    Archived {
        /// The task.
        id: TaskId,
        /// Year of its archive.
        year: u16,
    },
    /// The command was refused.
    #[error(transparent)]
    Domain(#[from] DomainError),
    /// A move failed before it changed anything that remains: the task is still in `from`.
    #[error("the task `{id}` was not moved and stays in context `{from}`: {source}")]
    MoveFailed {
        /// The task.
        id: TaskId,
        /// Context of the store that still holds the task.
        from: ContextName,
        /// What failed.
        source: Box<PortError>,
    },
    /// A move failed halfway and its rollback failed too: both stores hold a copy.
    #[error(
        "the task `{id}` is in the stores of `{from}` and `{to}`, a move failed halfway ({source}) and could not be undone: remove the task directory `{id}` from the store you do not want, then run `loom reindex`"
    )]
    MoveIncomplete {
        /// The task.
        id: TaskId,
        /// Context the task was moved from.
        from: ContextName,
        /// Context the task was moved to.
        to: ContextName,
        /// What failed.
        source: Box<PortError>,
    },
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
        if let Some(parent) = &new.relations.parent {
            check_parent::<TaskError>(None, &new.context, parent, |id| self.parent_link(id))?;
        }
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
        if let Command::Edit(edit) = &command
            && let Some(parent) = edit.relations.as_ref().and_then(|r| r.parent.as_ref())
        {
            let row = self
                .index
                .get(id)?
                .ok_or_else(|| TaskError::NotFound(id.clone()))?;
            if let TaskLocation::Archived(year) = row.location {
                return Err(TaskError::Archived {
                    id: id.clone(),
                    year,
                });
            }
            // Only a new parent is checked: an edit of the other relations resends the current
            // one, which may have become invalid since (a hand edit), and must not be refused for
            // it. A read-only task is left to `change`, which refuses it as such.
            let current = match &row.state {
                IndexedState::Valid { task, .. } if task.read_only_reason().is_none() => {
                    Some(task.relations.parent.as_ref())
                }
                IndexedState::Valid { .. } | IndexedState::Invalid { .. } => None,
            };
            if let Some(current) = current
                && current != Some(parent)
            {
                check_parent::<TaskError>(Some(id), &row.context, parent, |task| {
                    self.parent_link(task)
                })?;
            }
        }
        self.change(id, |_, _| Ok(Some(command)), now, origin, random)
    }

    /// The context and parent of task `id` as indexed; `None` when it is not indexed. An
    /// invalid file has no known parent.
    fn parent_link(&self, id: &TaskId) -> Result<Option<ParentLink>, TaskError> {
        let Some(row) = self.index.get(id)? else {
            return Ok(None);
        };
        let parent = match row.state {
            IndexedState::Valid { task, .. } => task.relations.parent,
            IndexedState::Invalid { .. } => None,
        };
        Ok(Some(ParentLink {
            context: row.context,
            parent,
        }))
    }

    /// Makes task `id` the active focus of its context at `now`: sets its status to `active`,
    /// sets the previous focused task of that context (when it is another active task) back to
    /// `ready`, and records the focus in the machine-local index.
    ///
    /// # Errors
    ///
    /// [`TaskError::NotFound`], [`TaskError::Archived`], [`TaskError::Invalid`], the refusal of
    /// [`loom_core::apply`] (for instance on a closed task, before the previous focus is
    /// touched), or the write or index error.
    pub fn go(
        &mut self,
        id: &TaskId,
        now: Timestamp,
        origin: &Origin,
        random: &mut impl RandomSource,
    ) -> Result<GoOutcome, TaskError> {
        let task = self.change(
            id,
            |current, _| {
                Ok(
                    (current.status != Status::Active).then_some(Command::SetStatus {
                        status: Status::Active,
                    }),
                )
            },
            now,
            origin,
            random,
        )?;
        let context = task.context.clone();
        let previous = focus_row(&self.index.conn, &context)?;
        if let Some(prev) = &previous
            && prev.task_id == *id
        {
            return Ok(GoOutcome {
                task,
                switched_from: None,
                since: prev.since,
            });
        }
        let since = set_focus(&self.index.conn, &context, id, now)?;
        let switched_from = if let Some(prev) = previous {
            let can_demote = self.index.get(&prev.task_id)?.is_some_and(|indexed| {
                indexed.context == context
                    && !indexed.read_only()
                    && matches!(
                        &indexed.state,
                        IndexedState::Valid { task, .. } if task.status == Status::Active
                    )
            });
            if can_demote {
                Some(self.change(
                    &prev.task_id,
                    |current, _| {
                        Ok(
                            (current.status == Status::Active).then_some(Command::SetStatus {
                                status: Status::Ready,
                            }),
                        )
                    },
                    now,
                    origin,
                    random,
                )?)
            } else {
                None
            }
        } else {
            None
        };
        Ok(GoOutcome {
            task,
            switched_from,
            since,
        })
    }

    /// Reverts the last change of task `id` at `now`: the compensating command is computed
    /// from its event log (see [`loom_core::undo`]) under the task's lock, then applied,
    /// written and indexed.
    ///
    /// # Errors
    ///
    /// [`TaskError::NotFound`], [`TaskError::Invalid`] for an invalid file,
    /// [`DomainError::NothingToUndo`], [`DomainError::Conflict`] when a field changed since,
    /// or the write error.
    pub fn undo(
        &mut self,
        id: &TaskId,
        now: Timestamp,
        origin: &Origin,
        random: &mut impl RandomSource,
    ) -> Result<Task, TaskError> {
        self.change(
            id,
            |task, events| undo(task, events).map(Some),
            now,
            origin,
            random,
        )
    }

    /// The event log of task `id`, read from its store.
    ///
    /// # Errors
    ///
    /// [`TaskError::NotFound`], or the read error.
    pub fn events(&self, id: &TaskId) -> Result<EventLog, TaskError> {
        let store = self.store_of(id)?;
        store.read_events(id).map_err(|error| match error {
            PortError::NotFound { .. } => TaskError::NotFound(id.clone()),
            error => TaskError::Files(error),
        })
    }

    /// The store holding task `id`, as indexed.
    fn store_of(&self, id: &TaskId) -> Result<&P, TaskError> {
        store_of(&self.index, &self.stores, id)
    }

    /// Applies the command `command` builds from the task and its log, under the task's lock,
    /// then writes and indexes the result.
    fn change(
        &mut self,
        id: &TaskId,
        command: impl FnOnce(&Task, &[EventRecord]) -> Result<Option<Command>, DomainError>,
        now: Timestamp,
        origin: &Origin,
        random: &mut impl RandomSource,
    ) -> Result<Task, TaskError> {
        let store = store_of(&self.index, &self.stores, id)?;
        // The row of a duplicate ID is invalid in the index, but the file the store returns is
        // a valid copy: which of the copies the command is for is unknown.
        let holders = self.holders(id);
        if holders.len() > 1 {
            return Err(TaskError::Invalid(InvalidTask::DuplicateId {
                id: id.clone(),
                contexts: holders,
            }));
        }
        let indexed = self.index.get(id)?;
        let last = indexed.as_ref().map(|row| (row.hash.clone(), row.location));
        let last = last.as_ref().map(|(hash, location)| (hash, *location));
        let task = store.update(id, |mut stored| {
            if let TaskLocation::Archived(year) = stored.location {
                return Err(TaskError::Archived {
                    id: id.clone(),
                    year,
                });
            }
            let mut prepended_events = Vec::new();
            if let Reconciliation::Record(changes) = decide(last, &stored) {
                let taken: Vec<EventId> = stored
                    .events
                    .events
                    .iter()
                    .map(|event| event.id.clone())
                    .collect();
                let event_id = EventId::generate(random, &taken).map_err(DomainError::from)?;
                let event = Event::Edited { changes };
                let file_origin = Origin {
                    actor: Actor::File,
                    machine_id: origin.machine_id.clone(),
                };
                let record = EventRecord::new(event_id, event, now, &file_origin);
                stored.events.events.push(record.clone());
                prepended_events.push(record);
            }
            let current = stored
                .task()
                .map_err(|error| TaskError::Invalid(error.clone()))?;
            let mut document = stored.file.document.map_err(TaskError::Invalid)?;
            if let Some(reason) = current.read_only_reason() {
                return Err(TaskError::Domain(DomainError::ReadOnly(reason)));
            }
            let Some(command) = command(&current, &stored.events.events)? else {
                return Ok((TaskWrite::default(), current));
            };
            let (task, mut events) =
                apply(current, command, &stored.events.events, now, origin, random)?;
            prepended_events.append(&mut events);
            if prepended_events.is_empty() {
                return Ok((TaskWrite::default(), task));
            }
            document.set_task(&task).map_err(DomainError::ReadOnly)?;
            Ok::<_, TaskError>((
                TaskWrite {
                    document: Some(document),
                    events: prepended_events,
                },
                task,
            ))
        })?;
        self.index
            .upsert(&IndexedTask::from_stored(&store.read(id)?))?;
        Ok(task)
    }

    /// Moves the active, closed tasks whose closing is at least `after` before `now` to the
    /// archive of the year they were closed (see [`loom_core::archive_year`]), at most `limit`,
    /// and indexes them there. [`ArchivedAll::more`] says the limit was reached: another pass
    /// may have tasks to move.
    ///
    /// A task that cannot be read or moved is reported in [`ArchivedAll::failed`] and stays
    /// where it is; the others are moved.
    ///
    /// # Errors
    ///
    /// [`TaskError::Index`] when the index cannot be read.
    pub fn archive_due(
        &mut self,
        now: Timestamp,
        after: Duration,
        limit: usize,
    ) -> Result<ArchivedAll, TaskError> {
        let _span = debug_span!("tasks.archive_due", limit).entered();
        let mut report = ArchivedAll::default();
        for row in list(&self.index.conn, None)? {
            if report.moved.len() >= limit {
                report.more = true;
                break;
            }
            let IndexedState::Valid { task, .. } = &row.state else {
                continue;
            };
            // Only closed tasks can be due: the others are skipped without reading their log.
            if row.location != TaskLocation::Active
                || !matches!(task.status, Status::Done | Status::Cancelled)
            {
                continue;
            }
            let due = self
                .events(&row.id)
                .map(|log| archive_year(task, &log.events, now, after));
            let result = match due {
                Ok(Some(year)) => self.archive(&row.id, year).map(|()| Some(year)),
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            };
            match result {
                Ok(Some(year)) => report.moved.push(ArchivedTask {
                    context: row.context,
                    id: row.id,
                    year,
                }),
                Ok(None) => {}
                Err(error) => {
                    report
                        .failed
                        .push(TaskFailure::new(&row.context, Some(&row.id), &error));
                }
            }
        }
        Ok(report)
    }

    /// Moves the active task `id` to the archive of `year`, and indexes it there.
    ///
    /// # Errors
    ///
    /// [`TaskError::NotFound`], [`TaskError::Archived`] when it is archived already, or the
    /// move or index error.
    pub fn archive(&mut self, id: &TaskId, year: u16) -> Result<(), TaskError> {
        let store = store_of(&self.index, &self.stores, id)?;
        store.archive(id, year).map_err(|error| match error {
            PortError::NotFound { .. } => TaskError::NotFound(id.clone()),
            PortError::Archived { year, .. } => TaskError::Archived {
                id: id.clone(),
                year,
            },
            error => TaskError::Files(error),
        })?;
        self.index
            .upsert(&IndexedTask::from_stored(&store.read(id)?))?;
        Ok(())
    }

    /// Moves the active task `id` to the store of context `to` at `now`, and indexes it there.
    ///
    /// The task keeps its ID, its files and its history; an `edited` event records the change
    /// of context (after the event of a hand edit that was not recorded yet). The copy is made
    /// in the target store first, and the task removed from the source after: if the removal
    /// fails, the copy is removed again and the task stays where it was
    /// ([`TaskError::MoveFailed`]). Only a crash between the two steps, or a failed rollback
    /// ([`TaskError::MoveIncomplete`]), leaves the task in both stores, where the reconciliation
    /// flags it as a duplicate ID: read-only until one copy is removed by hand.
    ///
    /// # Errors
    ///
    /// [`TaskError::NotFound`], [`TaskError::UnknownContext`] for a context this machine holds no
    /// store of, [`TaskError::Archived`], [`TaskError::Invalid`] for an invalid file or a
    /// duplicate ID, the refusal of [`loom_core::move_task`], [`TaskError::MoveFailed`],
    /// [`TaskError::MoveIncomplete`], or the read or index error.
    pub fn move_to(
        &mut self,
        id: &TaskId,
        to: &ContextName,
        now: Timestamp,
        origin: &Origin,
        random: &mut impl RandomSource,
    ) -> Result<Task, TaskError> {
        let from_store = store_of(&self.index, &self.stores, id)?;
        let to_store = self
            .stores
            .get(to)
            .ok_or_else(|| TaskError::UnknownContext(to.clone()))?;
        let holders = self.holders(id);
        if holders.len() > 1 {
            return Err(TaskError::Invalid(InvalidTask::DuplicateId {
                id: id.clone(),
                contexts: holders,
            }));
        }
        let from = from_store.context().clone();
        let stored = from_store.read(id).map_err(|error| match error {
            PortError::NotFound { .. } => TaskError::NotFound(id.clone()),
            error => TaskError::Files(error),
        })?;
        if let TaskLocation::Archived(year) = stored.location {
            return Err(TaskError::Archived {
                id: id.clone(),
                year,
            });
        }
        let indexed = self.index.get(id)?;
        let last = indexed.as_ref().map(|row| (&row.hash, row.location));
        let mut events = stored.events.events.clone();
        let mut written = Vec::new();
        if let Reconciliation::Record(changes) = decide(last, &stored) {
            let taken: Vec<EventId> = events.iter().map(|event| event.id.clone()).collect();
            let event_id = EventId::generate(random, &taken).map_err(DomainError::from)?;
            let file_origin = Origin {
                actor: Actor::File,
                machine_id: origin.machine_id.clone(),
            };
            let record = EventRecord::new(event_id, Event::Edited { changes }, now, &file_origin);
            events.push(record.clone());
            written.push(record);
        }
        let current = stored
            .task()
            .map_err(|error| TaskError::Invalid(error.clone()))?;
        let mut document = stored.file.document.map_err(TaskError::Invalid)?;
        let children = self
            .index
            .tasks()?
            .iter()
            .filter(|row| match &row.state {
                IndexedState::Valid { task, .. } => task.relations.parent.as_ref() == Some(id),
                IndexedState::Invalid { .. } => false,
            })
            .count();
        let (moved, record) = move_task(current, to, children, &events, now, origin, random)?;
        written.push(record);
        document.set_task(&moved).map_err(DomainError::ReadOnly)?;
        let write = TaskWrite {
            document: Some(document),
            events: written,
        };
        to_store
            .receive(from_store, id, &write)
            .map_err(|source| TaskError::MoveFailed {
                id: id.clone(),
                from: from.clone(),
                source: Box::new(source),
            })?;
        if let Err(source) = from_store.remove(id) {
            return Err(match to_store.remove(id) {
                Ok(()) => TaskError::MoveFailed {
                    id: id.clone(),
                    from,
                    source: Box::new(source),
                },
                Err(_) => TaskError::MoveIncomplete {
                    id: id.clone(),
                    from,
                    to: to.clone(),
                    source: Box::new(source),
                },
            });
        }
        self.index
            .upsert(&IndexedTask::from_stored(&to_store.read(id)?))?;
        Ok(moved)
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
        let mut read = Vec::new();
        let mut listed = Vec::new();
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
                listed.push((context.clone(), id.clone()));
                match store.read(&id) {
                    Ok(stored) => read.push(stored),
                    // Removed since it was listed.
                    Err(PortError::NotFound { .. }) => {}
                    Err(error) => failed.push(TaskFailure::new(context, Some(&id), &error.into())),
                }
            }
        }
        // An ID held by several stores gets one flagged row, at the place of its first copy that
        // could be read. The listing decides, not the reads: a copy that cannot be read still
        // makes a duplicate, as in `reconcile_all`.
        let duplicates = duplicate_ids(listed.iter().map(|(context, id)| (context, id)));
        let mut rows = Vec::with_capacity(read.len());
        let mut flagged = HashSet::new();
        for stored in &read {
            match duplicates.get(&stored.id) {
                None => rows.push(IndexedTask::from_stored(stored)),
                Some(contexts) => {
                    if flagged.insert(stored.id.clone()) {
                        warn!(id = %stored.id, ?contexts, "duplicate task ID across contexts");
                        rows.push(IndexedTask::duplicate(stored, contexts));
                    }
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
    /// An ID that the stores of several contexts hold is not indexed as any of them: its row is
    /// flagged invalid, [`Reconciled::Duplicate`], and no file is written.
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
        let holders = self.holders(id);
        self.reconcile_held(context, id, &holders, now, origin, random)
    }

    /// The contexts whose stores hold task `id`, in the order of the stores.
    ///
    /// A store that cannot be looked at (a permission, an I/O error) is skipped, as a reindex goes
    /// on without it: one broken store never blocks the tasks of the others. The skip is logged at
    /// `debug` only: it repeats for every command, and a reindex reports the store in `failed`.
    /// An ID that such a store holds is thus not seen as a duplicate until it can be read.
    fn holders(&self, id: &TaskId) -> Vec<ContextName> {
        let mut holders = Vec::new();
        for (context, store) in &self.stores {
            match store.contains(id) {
                Ok(true) => holders.push(context.clone()),
                Ok(false) => {}
                Err(error) => {
                    debug!(%context, %id, %error, "cannot tell whether this store holds the task");
                }
            }
        }
        holders
    }

    /// [`Tasks::reconcile`] of task `id`, which the stores of `holders` hold.
    fn reconcile_held(
        &mut self,
        context: &ContextName,
        id: &TaskId,
        holders: &[ContextName],
        now: Timestamp,
        origin: &Origin,
        random: &mut impl RandomSource,
    ) -> Result<Reconciled, TaskError> {
        let context = match holders {
            [] => context,
            [only] => only,
            [first, ..] => return self.flag_duplicate(first, id, holders),
        };
        let store = self
            .stores
            .get(context)
            .ok_or_else(|| TaskError::UnknownContext(context.clone()))?;
        let indexed = self.index.get(id)?;
        let last = indexed
            .as_ref()
            .filter(|row| row.context == *context)
            .map(|row| (row.hash.clone(), row.location));
        let last = last.as_ref().map(|(hash, location)| (hash, *location));
        let result = store.update(id, |mut stored| match decide(last, &stored) {
            Reconciliation::Unchanged => Ok((TaskWrite::default(), (Reconciled::Unchanged, None))),
            Reconciliation::Index => {
                Ok((TaskWrite::default(), (Reconciled::Indexed, Some(stored))))
            }
            Reconciliation::Withhold => {
                Ok((TaskWrite::default(), (Reconciled::Withheld, Some(stored))))
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

    /// Flags the row of `id`, held by the stores of `holders`, as a duplicate; `first` is the
    /// context that gives its file.
    fn flag_duplicate(
        &mut self,
        first: &ContextName,
        id: &TaskId,
        holders: &[ContextName],
    ) -> Result<Reconciled, TaskError> {
        let store = self
            .stores
            .get(first)
            .ok_or_else(|| TaskError::UnknownContext(first.clone()))?;
        let row = IndexedTask::duplicate(&store.read(id)?, holders);
        if self.index.get(id)?.as_ref() == Some(&row) {
            return Ok(Reconciled::Unchanged);
        }
        warn!(%id, contexts = ?holders, "duplicate task ID across contexts");
        self.index.upsert(&row)?;
        Ok(Reconciled::Duplicate(holders.to_vec()))
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
        let duplicates = duplicate_ids(listed.iter().map(|(context, id)| (context, id)));
        let mut results = Vec::with_capacity(listed.len());
        let mut seen = HashSet::with_capacity(listed.len());
        for (context, id) in listed {
            let holders = duplicates
                .get(&id)
                .cloned()
                .unwrap_or_else(|| vec![context.clone()]);
            match self.reconcile_held(&context, &id, &holders, now, origin, random) {
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

/// The store of `stores` holding task `id`, by the context `index` gives it.
fn store_of<'a, P>(
    index: &TaskIndex,
    stores: &'a BTreeMap<ContextName, P>,
    id: &TaskId,
) -> Result<&'a P, TaskError> {
    let indexed = index
        .get(id)?
        .ok_or_else(|| TaskError::NotFound(id.clone()))?;
    stores
        .get(&indexed.context)
        .ok_or(TaskError::UnknownContext(indexed.context))
}

/// The focused task of a context, recorded in the machine-local index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusRow {
    /// Context of the focus.
    pub context: ContextName,
    /// Focused task ID.
    pub task_id: TaskId,
    /// When this task became the focus of `context`.
    pub since: Timestamp,
}

/// What [`Tasks::go`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoOutcome {
    /// The task that is now the active focus of its context.
    pub task: Task,
    /// The previous focus of that context, moved back to `ready`, when it was another active task.
    pub switched_from: Option<Task>,
    /// When `task` became the focus of its context.
    pub since: Timestamp,
}

/// What [`Tasks::archive_due`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchivedAll {
    /// The tasks moved to the archive, by context, then ID.
    pub moved: Vec<ArchivedTask>,
    /// The tasks that could not be read or moved: they stay where they are.
    pub failed: Vec<TaskFailure>,
    /// The limit of moves was reached before every task was looked at.
    pub more: bool,
}

/// A task moved to the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedTask {
    /// Context of its store.
    pub context: ContextName,
    /// The task.
    pub id: TaskId,
    /// Year of its archive, the year it was closed.
    pub year: u16,
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
    /// The file was indexed (new, notes changed, invalid, or a log without creation event),
    /// without an event.
    Indexed,
    /// The file was indexed without an event, although it may hold a hand edit: the log has a
    /// line written by a newer loom, so the edit cannot be told apart from that line's change.
    Withheld,
    /// A hand edit of these fields was recorded, and the file indexed.
    Edited(Vec<FieldChange>),
    /// The ID is in the stores of these contexts: its row was flagged invalid and no file was
    /// written (see [`IndexedTask::duplicate`]).
    Duplicate(Vec<ContextName>),
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
const COLUMNS: &str = "id, context, hash, task, error, body, archived";

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
                            pinned, updated, body, link_keys, archived) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
         ON CONFLICT (id) DO UPDATE SET \
             context = excluded.context, hash = excluded.hash, task = excluded.task, \
             error = excluded.error, title = excluded.title, status = excluded.status, \
             importance = excluded.importance, due = excluded.due, pinned = excluded.pinned, \
             updated = excluded.updated, body = excluded.body, link_keys = excluded.link_keys, \
             archived = excluded.archived",
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
            task.location.archive_year(),
        ],
    )?;
    let keep_focus = task.location == TaskLocation::Active
        && matches!(
            &task.state,
            IndexedState::Valid { task, .. } if task.status == Status::Active
        );
    if keep_focus {
        conn.execute(
            "DELETE FROM focus WHERE task_id = ?1 AND context <> ?2",
            params![task.id.as_str(), task.context.as_str()],
        )?;
    } else {
        conn.execute("DELETE FROM focus WHERE task_id = ?1", [task.id.as_str()])?;
    }
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

fn parse_context(text: &str) -> Result<ContextName, StoreError> {
    text.parse()
        .map_err(|error: loom_core::ContextNameError| StoreError::Corrupt(error.to_string()))
}

fn parse_timestamp(secs: i64) -> Result<Timestamp, StoreError> {
    u64::try_from(secs)
        .ok()
        .and_then(Timestamp::from_unix_secs)
        .ok_or_else(|| StoreError::Corrupt(format!("timestamp {secs} out of range")))
}

/// A row of `tasks` ([`COLUMNS`]).
fn read_row(row: &Row<'_>) -> rusqlite::Result<Result<IndexedTask, StoreError>> {
    let id: String = row.get(0)?;
    let context: String = row.get(1)?;
    let hash: String = row.get(2)?;
    let json: Option<String> = row.get(3)?;
    let error: Option<String> = row.get(4)?;
    let body: String = row.get(5)?;
    let archived: Option<u16> = row.get(6)?;
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
            context: parse_context(&context)?,
            hash: ContentHash::from_hex(hash),
            location: archived.map_or(TaskLocation::Active, TaskLocation::Archived),
            state,
        })
    })())
}

/// The focus row of `context`, when one is recorded.
fn focus_row(conn: &Connection, context: &ContextName) -> Result<Option<FocusRow>, StoreError> {
    conn.query_row(
        "SELECT context, task_id, since FROM focus WHERE context = ?1",
        [context.as_str()],
        |row| {
            let context: String = row.get(0)?;
            let task_id: String = row.get(1)?;
            let since: i64 = row.get(2)?;
            Ok((|| {
                Ok(FocusRow {
                    context: parse_context(&context)?,
                    task_id: parse_id(&task_id)?,
                    since: parse_timestamp(since)?,
                })
            })())
        },
    )
    .optional()?
    .transpose()
}

/// Records `task_id` as the focus of `context` at `now`, keeping `since` when `task_id` was
/// already the focus of `context`.
fn set_focus(
    conn: &Connection,
    context: &ContextName,
    task_id: &TaskId,
    now: Timestamp,
) -> Result<Timestamp, StoreError> {
    conn.execute(
        "INSERT INTO focus (context, task_id, since) VALUES (?1, ?2, ?3) \
         ON CONFLICT (context) DO UPDATE SET \
             task_id = excluded.task_id, \
             since = CASE WHEN focus.task_id = excluded.task_id THEN focus.since ELSE excluded.since END",
        params![context.as_str(), task_id.as_str(), unix(now)?],
    )?;
    focus_row(conn, context)?
        .map(|row| row.since)
        .ok_or_else(|| StoreError::Corrupt(format!("missing focus row for {context}")))
}

/// The focused tasks of `context`, or of every context when `None`, sorted by context.
pub(crate) fn focus(
    conn: &Connection,
    context: Option<&ContextName>,
) -> Result<Vec<(FocusRow, IndexedTask)>, StoreError> {
    let mut statement = conn.prepare(
        "SELECT t.id, t.context, t.hash, t.task, t.error, t.body, t.archived, f.since \
         FROM focus f \
         JOIN tasks t ON t.id = f.task_id AND t.context = f.context \
         WHERE (?1 IS NULL OR f.context = ?1) \
           AND t.error IS NULL \
           AND t.archived IS NULL \
           AND t.status = 'active' \
         ORDER BY f.context",
    )?;
    let rows = statement.query_map([context.map(ContextName::as_str)], |row| {
        let task = read_row(row)?;
        let since: i64 = row.get(7)?;
        Ok((|| {
            let task = task?;
            let focus = FocusRow {
                context: task.context.clone(),
                task_id: task.id.clone(),
                since: parse_timestamp(since)?,
            };
            Ok((focus, task))
        })())
    })?;
    rows.map(|row| row?).collect()
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

#[cfg(test)]
mod tests;
