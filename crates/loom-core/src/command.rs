//! Commands on tasks: what can be done to a task, and the events doing it produces.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::Date;
use crate::context::ContextName;
use crate::event::{Change, Event, EventRecord, FieldChange, Origin};
use crate::id::{EventId, IdError, RandomSource, TaskId};
use crate::relations::ParentProblem;
use crate::task::{
    Importance, Kind, Link, ReadOnlyReason, Relations, Status, StatusName, Task,
    is_legal_transition,
};
use crate::timestamp::Timestamp;

/// Why a command was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    /// The task has a kind, format or status unknown to this version of loom.
    #[error("the task is read-only: {0}")]
    ReadOnly(ReadOnlyReason),
    /// The status cannot move from `from` to `to`.
    #[error("a task cannot move from {from} to {to}")]
    IllegalTransition {
        /// Current status.
        from: StatusName,
        /// Requested status.
        to: StatusName,
    },
    /// The requested status, kind or format is unknown to this version of loom.
    #[error("cannot set a task to an unknown value: {0}")]
    UnknownValue(ReadOnlyReason),
    /// The title is empty.
    #[error("a task title cannot be empty")]
    EmptyTitle,
    /// The note is empty.
    #[error("a note cannot be empty")]
    EmptyNote,
    /// A field no longer has the value an undo expects: something changed it since.
    #[error("cannot undo: `{0}` changed since")]
    Conflict(&'static str),
    /// No event of the task changed a field.
    #[error("nothing to undo")]
    NothingToUndo,
    /// No task ID could be generated.
    #[error(transparent)]
    Id(#[from] IdError),
    /// The parent cannot be the parent of this task.
    #[error("{parent} cannot be the parent: {problem}")]
    InvalidParent {
        /// The requested parent.
        parent: TaskId,
        /// Why.
        problem: ParentProblem,
    },
}

/// A change requested on a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    /// Move the task to another status.
    SetStatus {
        /// The new status.
        status: Status,
    },
    /// Change fields other than the status.
    Edit(TaskEdit),
    /// Pin the task.
    Pin,
    /// Unpin the task.
    Unpin,
    /// Log a note on the task.
    LogNote {
        /// The note.
        text: String,
    },
    /// Revert an event, as computed by [`undo`]. Never read from input: only [`undo`] builds it,
    /// so a client cannot send arbitrary changes that bypass the transition table.
    #[serde(skip_deserializing)]
    #[schemars(skip)]
    Undo {
        /// ID of the reverted event.
        of: EventId,
        /// The changes that revert it.
        changes: Vec<FieldChange>,
    },
    /// Accept an inbox task: move to ready, and optionally change kind, importance or due date.
    Accept {
        /// New kind.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<Kind>,
        /// New importance.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        importance: Option<Importance>,
        /// New due date.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        due: Option<Date>,
    },
}

/// Fields to change. `None` leaves a field as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskEdit {
    /// New title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// New kind. The importance is kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    /// New importance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub importance: Option<Importance>,
    /// New due date. `Some(None)` (`null`) removes it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub due: Option<Option<Date>>,
    /// New links, which replace all the links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<Link>>,
    /// New relations, which replace all the relations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relations: Option<Relations>,
}

/// Reads a field that is present, `null` included, as `Some`.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

/// A gesture available on a task, as offered by the CLI and the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", content = "status", rename_all = "snake_case")]
pub enum Action {
    /// Move to this status.
    SetStatus(StatusName),
    /// Edit fields.
    Edit,
    /// Pin.
    Pin,
    /// Unpin.
    Unpin,
    /// Log a note.
    LogNote,
}

/// A task to create.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewTask {
    /// Title.
    pub title: String,
    /// Kind.
    #[serde(flatten)]
    pub kind: Kind,
    /// Context.
    pub context: ContextName,
    /// Initial status, usually `inbox`.
    #[serde(flatten)]
    pub status: Status,
    /// Importance. If `None`, the default of the kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub importance: Option<Importance>,
    /// Due date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<Date>,
    /// Pinned.
    #[serde(default)]
    pub pinned: bool,
    /// Links.
    #[serde(default)]
    pub links: Vec<Link>,
    /// Relations.
    #[serde(default)]
    pub relations: Relations,
}

/// Creates a task with a new ID drawn from `random`, distinct from every ID of `taken`.
///
/// # Errors
///
/// [`DomainError::EmptyTitle`], [`DomainError::UnknownValue`] for an unknown kind, format or
/// status, [`DomainError::Id`] when no free ID is drawn.
pub fn create(
    new: NewTask,
    random: &mut impl RandomSource,
    taken: &[TaskId],
    now: Timestamp,
    origin: &Origin,
) -> Result<(Task, Vec<EventRecord>), DomainError> {
    let title = valid_title(new.title)?;
    known(&new.kind, &new.status)?;
    let task = Task {
        id: TaskId::generate(random, taken)?,
        title,
        importance: new
            .importance
            .unwrap_or_else(|| Importance::default_for(&new.kind)),
        kind: new.kind,
        context: new.context,
        status: new.status,
        due: new.due,
        pinned: new.pinned,
        links: new.links,
        relations: new.relations,
        created: now,
        updated: now,
    };
    let created = Event::Created {
        task: Box::new(task.clone()),
    };
    let id = EventId::generate(random, &[])?;
    Ok((task, vec![EventRecord::new(id, created, now, origin)]))
}

/// The gestures that [`apply`] accepts on `task`; none for a read-only task.
#[must_use]
pub fn available_actions(task: &Task) -> Vec<Action> {
    let Some(from) = task.status.name() else {
        return Vec::new();
    };
    if task.read_only_reason().is_some() {
        return Vec::new();
    }
    let mut actions: Vec<Action> = StatusName::ALL
        .into_iter()
        .filter(|to| is_legal_transition(from, *to))
        .map(Action::SetStatus)
        .collect();
    actions.push(Action::Edit);
    actions.push(if task.pinned {
        Action::Unpin
    } else {
        Action::Pin
    });
    actions.push(Action::LogNote);
    actions
}

/// Applies `command` to `task` at `now`: the changed task and the events to append to `log`,
/// the task's event log. Event IDs are drawn from `random`, distinct from those of `log`.
///
/// A command that changes nothing (pin a pinned task, edit to the same values) produces no
/// event and leaves `updated` as it is.
///
/// # Errors
///
/// [`DomainError::ReadOnly`] for a read-only task, and the reason the command is invalid
/// otherwise.
pub fn apply(
    task: Task,
    command: Command,
    log: &[EventRecord],
    now: Timestamp,
    origin: &Origin,
    random: &mut impl RandomSource,
) -> Result<(Task, Vec<EventRecord>), DomainError> {
    if let Some(reason) = task.read_only_reason() {
        return Err(DomainError::ReadOnly(reason));
    }
    let mut task = task;
    let event = match command {
        Command::SetStatus { status } => set_status(&task, status)?.map(Event::StatusChanged),
        Command::Edit(edit) => {
            let changes = edit_changes(&task, edit)?;
            (!changes.is_empty()).then_some(Event::Edited { changes })
        }
        Command::Pin | Command::Unpin => {
            let pinned = matches!(command, Command::Pin);
            (task.pinned != pinned).then(|| Event::Edited {
                changes: vec![FieldChange::Pinned(Change {
                    from: task.pinned,
                    to: pinned,
                })],
            })
        }
        Command::LogNote { text } => {
            if text.trim().is_empty() {
                return Err(DomainError::EmptyNote);
            }
            Some(Event::NoteLogged { text })
        }
        Command::Undo { of, changes } => {
            if changes.is_empty() {
                return Err(DomainError::NothingToUndo);
            }
            Some(Event::Undone { of, changes })
        }
        Command::Accept {
            kind,
            importance,
            due,
        } => {
            let status_change = set_status(&task, Status::Ready)?;
            let mut changes = Vec::new();
            if let Some(change) = status_change {
                changes.push(FieldChange::Status(change));
            }
            if let Some(kind) = kind
                && kind != task.kind
            {
                known(&kind, &Status::Ready)?;
                changes.push(FieldChange::Kind(Change {
                    from: task.kind.clone(),
                    to: kind,
                }));
            }
            if let Some(importance) = importance
                && importance != task.importance
            {
                changes.push(FieldChange::Importance(Change {
                    from: task.importance,
                    to: importance,
                }));
            }
            if let Some(due) = due
                && Some(due) != task.due
            {
                changes.push(FieldChange::Due(Change {
                    from: task.due,
                    to: Some(due),
                }));
            }
            match changes.as_slice() {
                [] => None,
                [FieldChange::Status(change)] => Some(Event::StatusChanged(change.clone())),
                _ => Some(Event::Edited { changes }),
            }
        }
    };
    let Some(event) = event else {
        return Ok((task, Vec::new()));
    };
    for change in event.changes().unwrap_or_default() {
        write(&mut task, change)?;
    }
    task.updated = now;
    let taken: Vec<EventId> = log.iter().map(|record| record.id.clone()).collect();
    let id = EventId::generate(random, &taken)?;
    Ok((task, vec![EventRecord::new(id, event, now, origin)]))
}

/// The command that reverts the last change recorded in `events`, the log of `task`.
///
/// Notes and the creation are not changes and are skipped. The last change can itself be an
/// undo: reverting it redoes the change it reverted.
///
/// # Errors
///
/// [`DomainError::ReadOnly`] for a read-only task, [`DomainError::NothingToUndo`] when no event
/// changed a field.
pub fn undo(task: &Task, events: &[EventRecord]) -> Result<Command, DomainError> {
    if let Some(reason) = task.read_only_reason() {
        return Err(DomainError::ReadOnly(reason));
    }
    let (of, changes) = events
        .iter()
        .rev()
        .find_map(|record| {
            record
                .event
                .changes()
                .map(|changes| (record.id.clone(), changes))
        })
        .ok_or(DomainError::NothingToUndo)?;
    Ok(Command::Undo {
        of,
        changes: changes
            .into_iter()
            .rev()
            .map(FieldChange::inverse)
            .collect(),
    })
}

/// Why an event log does not replay into a task.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplayError {
    /// The log does not start with the creation of the task.
    #[error("the log does not start with the creation of the task")]
    NoCreation,
    /// The log creates the task more than once.
    #[error("the log creates the task more than once")]
    CreatedTwice,
    /// A change expects a value the field no longer has: the log is not consistent.
    #[error("a change of `{0}` does not follow from the events before it")]
    Conflict(&'static str),
}

/// The task as its log `events` describes it: the created task, every field change applied in
/// order, last updated at the last event. Notes change no field.
///
/// # Errors
///
/// [`ReplayError`] when the log does not start with the creation, creates twice, or holds a
/// change whose `from` value is not the field's value at that point.
pub fn replay(events: &[EventRecord]) -> Result<Task, ReplayError> {
    let (first, rest) = events.split_first().ok_or(ReplayError::NoCreation)?;
    let Event::Created { task } = &first.event else {
        return Err(ReplayError::NoCreation);
    };
    let mut task = Task {
        updated: first.at,
        ..(**task).clone()
    };
    for record in rest {
        match &record.event {
            Event::Created { .. } => return Err(ReplayError::CreatedTwice),
            Event::Edited { changes } | Event::Undone { changes, .. } => {
                for change in changes {
                    replay_change(&mut task, change.clone())?;
                }
            }
            Event::StatusChanged(change) => {
                replay_change(&mut task, FieldChange::Status(change.clone()))?;
            }
            Event::NoteLogged { .. } => {}
        }
        task.updated = record.at;
    }
    Ok(task)
}

fn replay_change(task: &mut Task, change: FieldChange) -> Result<(), ReplayError> {
    write(task, change).map_err(|error| match error {
        DomainError::Conflict(field) => ReplayError::Conflict(field),
        // `write` refuses nothing else.
        DomainError::ReadOnly(_)
        | DomainError::IllegalTransition { .. }
        | DomainError::UnknownValue(_)
        | DomainError::EmptyTitle
        | DomainError::EmptyNote
        | DomainError::NothingToUndo
        | DomainError::Id(_)
        | DomainError::InvalidParent { .. } => ReplayError::Conflict("task"),
    })
}

/// The changes a hand edit made, from `before` (the task its log describes) to `after`.
///
/// `after` is the task its file holds. One change per field that differs, in field order;
/// `id`, `context`, `created` and `updated` are not fields a change records.
///
/// No transition rule applies: the file is the truth, and an unknown kind or status is recorded
/// as written.
#[must_use]
pub fn hand_edit_changes(before: &Task, after: &Task) -> Vec<FieldChange> {
    fn change<T: PartialEq + Clone>(from: &T, to: &T) -> Option<Change<T>> {
        (from != to).then(|| Change {
            from: from.clone(),
            to: to.clone(),
        })
    }
    [
        change(&before.title, &after.title).map(FieldChange::Title),
        change(&before.kind, &after.kind).map(FieldChange::Kind),
        change(&before.status, &after.status).map(FieldChange::Status),
        change(&before.importance, &after.importance).map(FieldChange::Importance),
        change(&before.due, &after.due).map(FieldChange::Due),
        change(&before.pinned, &after.pinned).map(FieldChange::Pinned),
        change(&before.links, &after.links).map(FieldChange::Links),
        change(&before.relations, &after.relations).map(FieldChange::Relations),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn valid_title(title: String) -> Result<String, DomainError> {
    if title.trim().is_empty() {
        Err(DomainError::EmptyTitle)
    } else {
        Ok(title)
    }
}

/// Refuses a kind, format or status unknown to this version of loom.
fn known(kind: &Kind, status: &Status) -> Result<(), DomainError> {
    if let Some(reason) = kind.unknown() {
        return Err(DomainError::UnknownValue(reason));
    }
    match status {
        Status::Unknown(name) => Err(DomainError::UnknownValue(ReadOnlyReason::UnknownStatus(
            name.clone(),
        ))),
        Status::Inbox
        | Status::Ready
        | Status::Active
        | Status::Waiting { .. }
        | Status::Done
        | Status::Cancelled => Ok(()),
    }
}

/// The status change to `to`, or `None` when the task already has exactly that status.
fn set_status(task: &Task, to: Status) -> Result<Option<Change<Status>>, DomainError> {
    known(&task.kind, &to)?;
    // A task with an unknown status is read-only and never reaches this point.
    if let (Some(from), Some(target)) = (task.status.name(), to.name())
        && !is_legal_transition(from, target)
    {
        return Err(DomainError::IllegalTransition { from, to: target });
    }
    Ok((task.status != to).then(|| Change {
        from: task.status.clone(),
        to,
    }))
}

/// The changes `edit` makes to `task`, leaving out fields set to their current value.
fn edit_changes(task: &Task, edit: TaskEdit) -> Result<Vec<FieldChange>, DomainError> {
    fn change<T: PartialEq + Clone>(current: &T, new: Option<T>) -> Option<Change<T>> {
        new.filter(|new| new != current).map(|to| Change {
            from: current.clone(),
            to,
        })
    }
    let title = edit.title.map(valid_title).transpose()?;
    if let Some(kind) = &edit.kind {
        known(kind, &task.status)?;
    }
    let changes = [
        change(&task.title, title).map(FieldChange::Title),
        change(&task.kind, edit.kind).map(FieldChange::Kind),
        change(&task.importance, edit.importance).map(FieldChange::Importance),
        change(&task.due, edit.due).map(FieldChange::Due),
        change(&task.links, edit.links).map(FieldChange::Links),
        change(&task.relations, edit.relations).map(FieldChange::Relations),
    ];
    Ok(changes.into_iter().flatten().collect())
}

/// Writes the `to` value of `change`, if the field still has its `from` value.
fn write(task: &mut Task, change: FieldChange) -> Result<(), DomainError> {
    fn set<T: PartialEq>(
        field: &mut T,
        change: Change<T>,
        name: &'static str,
    ) -> Result<(), DomainError> {
        if *field != change.from {
            return Err(DomainError::Conflict(name));
        }
        *field = change.to;
        Ok(())
    }
    let name = change.field();
    match change {
        FieldChange::Title(change) => set(&mut task.title, change, name),
        FieldChange::Kind(change) => set(&mut task.kind, change, name),
        FieldChange::Status(change) => set(&mut task.status, change, name),
        FieldChange::Importance(change) => set(&mut task.importance, change, name),
        FieldChange::Due(change) => set(&mut task.due, change, name),
        FieldChange::Pinned(change) => set(&mut task.pinned, change, name),
        FieldChange::Links(change) => set(&mut task.links, change, name),
        FieldChange::Relations(change) => set(&mut task.relations, change, name),
    }
}

#[cfg(test)]
mod tests;
