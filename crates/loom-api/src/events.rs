//! Change events of `GET /v1/events`: what changed, for a client to refetch it.

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use loom_core::{ContextName, TaskId};

/// A change in loomd, sent as one Server-Sent Event: `event:` is its `type`, `data:` this
/// object as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum ChangeEvent {
    /// A task was created or changed: through the API, a hand edit of its file, or a
    /// reconciliation.
    #[serde(rename = "task.changed")]
    TaskChanged {
        /// ID of the task.
        id: TaskId,
        /// Context of its store.
        context: ContextName,
    },
    /// A task was archived.
    #[serde(rename = "task.archived")]
    TaskArchived {
        /// ID of the task.
        id: TaskId,
        /// Context of its store.
        context: ContextName,
    },
    /// A repository of the index was added, changed or removed.
    #[serde(rename = "repo.changed")]
    RepoChanged {
        /// Slug of the repository, `<context>/<name>`.
        slug: String,
    },
    /// The number of inbox or waiting tasks of a context changed.
    #[serde(rename = "counts.changed")]
    CountsChanged {
        /// The context.
        context: ContextName,
        /// Tasks in the inbox.
        inbox: usize,
        /// Waiting tasks.
        waiting: usize,
    },
    /// Events were lost (a resume point too old, a client too slow, many changes at once): the
    /// client refetches everything it shows.
    #[serde(rename = "resync")]
    Resync,
}

impl ChangeEvent {
    /// The `type` of the event, as serialized and sent in `event:`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::TaskChanged { .. } => "task.changed",
            Self::TaskArchived { .. } => "task.archived",
            Self::RepoChanged { .. } => "repo.changed",
            Self::CountsChanged { .. } => "counts.changed",
            Self::Resync => "resync",
        }
    }

    /// Whether a client following `context` (every context when `None`) gets the event. A
    /// repository belongs to the context its slug starts with; `resync` goes to every client.
    #[must_use]
    pub fn concerns(&self, context: Option<&ContextName>) -> bool {
        let Some(context) = context else {
            return true;
        };
        match self {
            Self::TaskChanged { context: of, .. }
            | Self::TaskArchived { context: of, .. }
            | Self::CountsChanged { context: of, .. } => of == context,
            Self::RepoChanged { slug } => slug.split('/').next() == Some(context.as_str()),
            Self::Resync => true,
        }
    }
}

/// The `id:` of an event, `<run>-<seq>`: `run` names the loomd process (its start, in
/// milliseconds since the Unix epoch), `seq` counts its events from 1. A `Last-Event-ID` of
/// another run cannot be resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventCursor {
    /// The loomd process.
    pub run: u64,
    /// Number of the event in that process, from 1.
    pub seq: u64,
}

impl fmt::Display for EventCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.run, self.seq)
    }
}

/// A `Last-Event-ID` that is not `<run>-<seq>`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not an event id `<run>-<seq>`: {0:?}")]
pub struct EventCursorError(pub String);

impl FromStr for EventCursor {
    type Err = EventCursorError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || EventCursorError(text.to_owned());
        let (run, seq) = text.split_once('-').ok_or_else(invalid)?;
        let number = |part: &str| {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid());
            }
            part.parse::<u64>().map_err(|_| invalid())
        };
        Ok(Self {
            run: number(run)?,
            seq: number(seq)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn context(name: &str) -> ContextName {
        name.parse().unwrap()
    }

    #[test]
    fn events_are_tagged_by_type() {
        let event = ChangeEvent::TaskChanged {
            id: "k3q7zm2a".parse().unwrap(),
            context: context("work"),
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({ "type": "task.changed", "id": "k3q7zm2a", "context": "work" })
        );
        assert_eq!(event.kind(), "task.changed");
        assert_eq!(
            serde_json::to_value(ChangeEvent::Resync).unwrap(),
            json!({ "type": "resync" })
        );
        let counts = ChangeEvent::CountsChanged {
            context: context("work"),
            inbox: 4,
            waiting: 3,
        };
        assert_eq!(
            serde_json::from_value::<ChangeEvent>(serde_json::to_value(&counts).unwrap()).unwrap(),
            counts
        );
    }

    #[test]
    fn a_context_filter_keeps_its_events_and_resync() {
        let work = context("work");
        let task = ChangeEvent::TaskChanged {
            id: "k3q7zm2a".parse().unwrap(),
            context: context("home"),
        };
        assert!(task.concerns(None));
        assert!(!task.concerns(Some(&work)));
        let repo = ChangeEvent::RepoChanged {
            slug: "work/api".to_owned(),
        };
        assert!(repo.concerns(Some(&work)));
        assert!(
            !ChangeEvent::RepoChanged {
                slug: "workshop/api".to_owned()
            }
            .concerns(Some(&work))
        );
        assert!(ChangeEvent::Resync.concerns(Some(&work)));
    }

    #[test]
    fn cursors_are_run_dash_seq() {
        let cursor: EventCursor = "1791792000000-42".parse().unwrap();
        assert_eq!(
            cursor,
            EventCursor {
                run: 1_791_792_000_000,
                seq: 42
            }
        );
        assert_eq!(cursor.to_string(), "1791792000000-42");
        for bad in ["", "42", "-42", "1-", "1--2", "a-1", "1-+2", "1-2-3"] {
            assert!(bad.parse::<EventCursor>().is_err(), "{bad}");
        }
    }
}
