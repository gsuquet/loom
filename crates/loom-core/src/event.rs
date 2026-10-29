//! Events: what happened to a task, appended to its ledger and never rewritten.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::Date;
use crate::context::ContextName;
use crate::id::EventId;
use crate::task::{ExternalRef, Importance, Kind, Link, Relations, Status, Task};
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
    /// The notes of the task changed, by a hand edit or in an editor. The event holds the hash of
    /// the notes before and after, never their text.
    NotesChanged(Change<NotesHash>),
    /// A previous change was reverted.
    Undone {
        /// ID of the reverted event.
        of: EventId,
        /// The changes made to revert it.
        changes: Vec<FieldChange>,
    },
    /// An MR became a `delivery` link of the task. The link itself is added by an `edited`
    /// event just before, so that a version of loom without this event still replays it.
    Mr {
        /// What `loom pr` did on the forge.
        action: MrAction,
        /// The MR: provider, key and web URL.
        mr: ExternalRef,
    },
}

/// What `loom pr` did on the forge for an [`Event::Mr`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MrAction {
    /// It opened a new draft MR.
    Opened,
    /// It found an open MR of the branch and linked it.
    Linked,
}

impl Event {
    /// The field changes this event made, or `None` for an event that changes no field
    /// (creation, note): only the former can be undone.
    #[must_use]
    pub fn changes(&self) -> Option<Vec<FieldChange>> {
        match self {
            Self::Edited { changes } | Self::Undone { changes, .. } => Some(changes.clone()),
            Self::StatusChanged(change) => Some(vec![FieldChange::Status(change.clone())]),
            Self::Created { .. }
            | Self::NoteLogged { .. }
            | Self::NotesChanged(_)
            | Self::Mr { .. } => None,
        }
    }
}

/// Length of the SHA-256 of notes, in lowercase hex characters.
const NOTES_HASH_LEN: usize = 64;

/// The SHA-256 of the notes of a task (the text after the frontmatter), in lowercase hex.
///
/// A [`Event::NotesChanged`] holds the hash of the notes before and after a change: the ledger
/// tells that the prose changed and when, but does not keep it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct NotesHash(String);

impl JsonSchema for NotesHash {
    fn schema_name() -> Cow<'static, str> {
        "NotesHash".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "SHA-256 of the notes of a task, in lowercase hex.",
            "type": "string",
            "pattern": "^[0-9a-f]{64}$"
        })
    }
}

/// A text that is not the lowercase hex of a SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not a SHA-256 in lowercase hex (64 characters)")]
pub struct NotesHashError(String);

impl NotesHash {
    /// The hash `hex`, checked.
    ///
    /// # Errors
    ///
    /// [`NotesHashError`] unless `hex` is 64 lowercase hex characters.
    pub fn from_hex(hex: String) -> Result<Self, NotesHashError> {
        if hex.len() == NOTES_HASH_LEN
            && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            Ok(Self(hex))
        } else {
            Err(NotesHashError(hex))
        }
    }

    /// The hash in hex.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for NotesHash {
    type Error = NotesHashError;

    fn try_from(hex: String) -> Result<Self, Self::Error> {
        Self::from_hex(hex)
    }
}

impl From<NotesHash> for String {
    fn from(hash: NotesHash) -> Self {
        hash.0
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
    /// `context`: the task moved to the store of another context.
    Context(Change<ContextName>),
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
            Self::Context(change) => Self::Context(change.inverse()),
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
            Self::Context(_) => "context",
        }
    }
}
