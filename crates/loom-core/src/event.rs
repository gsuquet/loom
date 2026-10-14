//! Events: what happened to a task, appended to its ledger and never rewritten.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Date;
use crate::id::EventId;
use crate::task::{Importance, Kind, Link, Relations, Status, Task};
use crate::timestamp::Timestamp;

/// Schema version written in the `v` field of every event.
pub const EVENT_SCHEMA_VERSION: u32 = 1;

/// Who caused an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Actor {
    /// Me, through the CLI or the TUI.
    User,
    /// A hand edit of `task.md`, detected by loomd: a change it did not make.
    File,
    /// loomd on its own: sync, schedules.
    Daemon,
    /// An agent.
    Agent {
        /// Name of the agent.
        name: String,
    },
}

/// Name of the machine an event was recorded on, from `~/.loom/machine.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct MachineId(pub String);

/// Who makes a change, and where: stamped on every event it produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// Who causes the change.
    pub actor: Actor,
    /// The machine the change is made on.
    pub machine_id: MachineId,
}

/// One line of a task's event log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EventRecord {
    /// Schema version, [`EVENT_SCHEMA_VERSION`].
    pub v: u32,
    /// Identifier, unique within the task's log.
    pub id: EventId,
    /// When it happened.
    pub at: Timestamp,
    /// Who caused it.
    pub actor: Actor,
    /// Where it was recorded.
    pub machine_id: MachineId,
    /// What happened.
    #[serde(flatten)]
    pub event: Event,
}

impl EventRecord {
    /// Stamps `event` with the current schema version, `id`, `at` and `origin`.
    #[must_use]
    pub fn new(id: EventId, event: Event, at: Timestamp, origin: &Origin) -> Self {
        Self {
            v: EVENT_SCHEMA_VERSION,
            id,
            at,
            actor: origin.actor.clone(),
            machine_id: origin.machine_id.clone(),
            event,
        }
    }
}

/// What happened to a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The task was created, as it was then.
    Created {
        /// The new task.
        task: Box<Task>,
    },
    /// Fields other than the status changed.
    Edited {
        /// Each changed field, with its previous and new value.
        changes: Vec<FieldChange>,
    },
    /// The status changed.
    StatusChanged(Change<Status>),
    /// A note was logged on the task.
    NoteLogged {
        /// The note.
        text: String,
    },
    /// A previous change was reverted.
    Undone {
        /// ID of the reverted event.
        of: EventId,
        /// The changes made to revert it.
        changes: Vec<FieldChange>,
    },
}

impl Event {
    /// The field changes this event made, or `None` for an event that changes no field
    /// (creation, note): only the former can be undone.
    #[must_use]
    pub fn changes(&self) -> Option<Vec<FieldChange>> {
        match self {
            Self::Edited { changes } | Self::Undone { changes, .. } => Some(changes.clone()),
            Self::StatusChanged(change) => Some(vec![FieldChange::Status(change.clone())]),
            Self::Created { .. } | Self::NoteLogged { .. } => None,
        }
    }
}

/// A value before and after a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Change<T> {
    /// Value before.
    pub from: T,
    /// Value after.
    pub to: T,
}

impl<T> Change<T> {
    fn inverse(self) -> Self {
        Self {
            from: self.to,
            to: self.from,
        }
    }
}

/// A change of one field of a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "field", rename_all = "snake_case")]
pub enum FieldChange {
    /// `title`.
    Title(Change<String>),
    /// `kind` and `format`.
    Kind(Change<Kind>),
    /// `status` and its details.
    Status(Change<Status>),
    /// `importance`.
    Importance(Change<Importance>),
    /// `due`.
    Due(Change<Option<Date>>),
    /// `pinned`.
    Pinned(Change<bool>),
    /// `links`, as a whole.
    Links(Change<Vec<Link>>),
    /// `relations`, as a whole.
    Relations(Change<Relations>),
}

impl FieldChange {
    /// The change that reverts this one.
    #[must_use]
    pub fn inverse(self) -> Self {
        match self {
            Self::Title(change) => Self::Title(change.inverse()),
            Self::Kind(change) => Self::Kind(change.inverse()),
            Self::Status(change) => Self::Status(change.inverse()),
            Self::Importance(change) => Self::Importance(change.inverse()),
            Self::Due(change) => Self::Due(change.inverse()),
            Self::Pinned(change) => Self::Pinned(change.inverse()),
            Self::Links(change) => Self::Links(change.inverse()),
            Self::Relations(change) => Self::Relations(change.inverse()),
        }
    }

    /// Name of the changed field.
    #[must_use]
    pub const fn field(&self) -> &'static str {
        match self {
            Self::Title(_) => "title",
            Self::Kind(_) => "kind",
            Self::Status(_) => "status",
            Self::Importance(_) => "importance",
            Self::Due(_) => "due",
            Self::Pinned(_) => "pinned",
            Self::Links(_) => "links",
            Self::Relations(_) => "relations",
        }
    }
}
