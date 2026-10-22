//! Tasks over the API: `/v1/tasks…`. The task, its events and the commands are the types of
//! `loom-core`; this module wraps them in the responses and requests of the endpoints.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use loom_core::{
    Action, ContextName, Date, EventRecord, Importance, Kind, RankReason, ReadOnlyReason,
    SkippedLine, StatusName, Task, TaskId, Timestamp,
};

/// Answer of `GET /v1/tasks`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskList {
    /// The tasks: sorted by ID, or in priority order with `ranked=true`.
    pub tasks: Vec<TaskSummary>,
}

/// A task as listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskSummary {
    /// ID, the name of its directory.
    pub id: TaskId,
    /// Shortest prefix of the ID that no other task ID starts with, across every context.
    pub prefix: String,
    /// Context of the store that holds the task.
    pub context: ContextName,
    /// The task. `null` when its `task.md` is invalid.
    pub task: Option<Task>,
    /// Why the task cannot be changed. Absent when it can.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<TaskReadOnly>,
    /// Its place in the priority list. Present with `ranked=true` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<TaskRank>,
}

/// Place of a task in the priority list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskRank {
    /// Position, from 1.
    pub position: usize,
    /// The rule that placed the task after its predecessor.
    pub reason: RankReason,
}

/// One task with its notes and the actions it accepts: answer of `GET /v1/tasks/{id}` and of
/// every write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskDetail {
    /// The task, as listed (without `rank`).
    #[serde(flatten)]
    pub summary: TaskSummary,
    /// The notes after the frontmatter. The whole file when it is invalid.
    pub body: String,
    /// The gestures the task accepts. Empty for a read-only task.
    pub actions: Vec<Action>,
}

/// Answer of `GET /v1/tasks/{id}/events`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskEvents {
    /// The task.
    pub id: TaskId,
    /// Its events, in log order.
    pub events: Vec<EventRecord>,
    /// Lines of `events.jsonl` this version does not replay, kept as written.
    #[serde(default)]
    pub skipped: Vec<SkippedLine>,
}

/// Answer of `GET /v1/focus` and `loom focus --json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FocusList {
    /// Focus of each context that has one, sorted by context.
    pub entries: Vec<FocusEntry>,
}

/// Focus of one context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FocusEntry {
    /// Context of the focus.
    pub context: ContextName,
    /// Task that is the focus of `context`.
    pub task: TaskSummary,
    /// Time when `task` became the focus of `context`.
    pub since: Timestamp,
    /// If `task` has a repository link in the index, its path on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<PathBuf>,
}

/// Answer of `POST /v1/tasks/{id}/go` and `loom go <id> --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GoResult {
    /// Task that is now the focus of its context, with status `active`.
    pub task: TaskDetail,
    /// If another active task was the focus of that context, that task after it moved back to
    /// `ready`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switched_from: Option<TaskSummary>,
    /// Time when `task` became the focus of its context.
    pub since: Timestamp,
    /// If `task` has a repository link in the index, its path on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<PathBuf>,
}

/// Why a task cannot be changed. Also the `details` of a `read_only` error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "reason", content = "value", rename_all = "snake_case")]
pub enum TaskReadOnly {
    /// A kind written by a newer version of loom.
    UnknownKind(String),
    /// A content format written by a newer version of loom.
    UnknownContentFormat(String),
    /// A status written by a newer version of loom.
    UnknownStatus(String),
    /// `task.md` cannot be read: why, with the line when known.
    InvalidFile(String),
    /// The task is archived in `archive/<yyyy>/`: the year.
    Archived(u16),
}

impl From<ReadOnlyReason> for TaskReadOnly {
    fn from(reason: ReadOnlyReason) -> Self {
        match reason {
            ReadOnlyReason::UnknownKind(name) => Self::UnknownKind(name),
            ReadOnlyReason::UnknownContentFormat(name) => Self::UnknownContentFormat(name),
            ReadOnlyReason::UnknownStatus(name) => Self::UnknownStatus(name),
        }
    }
}

/// The `{action}` of `POST /v1/tasks/{id}/actions/{action}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskActionName {
    /// Move to `inbox`.
    Inbox,
    /// Move to `ready`.
    Ready,
    /// Move to `active`.
    Active,
    /// Move to `waiting`, with `on` and `until`.
    Waiting,
    /// Move to `done`.
    Done,
    /// Move to `cancelled`.
    Cancelled,
    /// Pin.
    Pin,
    /// Unpin.
    Unpin,
    /// Log a note, with `text`.
    LogNote,
}

impl TaskActionName {
    /// The action of [`Action`], as named in the path: a status name, `pin`, `unpin` or
    /// `log_note`. `None` for [`Action::Edit`], which is `PATCH /v1/tasks/{id}`.
    #[must_use]
    pub const fn of(action: Action) -> Option<Self> {
        match action {
            Action::SetStatus(status) => Some(match status {
                StatusName::Inbox => Self::Inbox,
                StatusName::Ready => Self::Ready,
                StatusName::Active => Self::Active,
                StatusName::Waiting => Self::Waiting,
                StatusName::Done => Self::Done,
                StatusName::Cancelled => Self::Cancelled,
            }),
            Action::Pin => Some(Self::Pin),
            Action::Unpin => Some(Self::Unpin),
            Action::LogNote => Some(Self::LogNote),
            Action::Edit => None,
        }
    }
}

/// Body of `POST /v1/tasks/{id}/actions/{action}`, optional. Each field belongs to one action.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskActionRequest {
    /// `waiting` only: what or whom the task waits on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    /// `waiting` only: the priority list does not show the task until this date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Date>,
    /// `log_note` only, required: the note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `ready` only, optional: new kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    /// `ready` only, optional: new importance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub importance: Option<Importance>,
    /// `ready` only, optional: new due date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<Date>,
}

/// `details` of an `ambiguous_prefix` error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AmbiguousPrefix {
    /// The prefix, normalized (lowercase, `i` and `l` read as `1`, `o` as `0`).
    pub prefix: String,
    /// Every task whose ID starts with it, sorted by ID.
    pub candidates: Vec<TaskCandidate>,
}

/// A task an ambiguous prefix matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskCandidate {
    /// ID.
    pub id: TaskId,
    /// Context of its store.
    pub context: ContextName,
    /// Title. Empty when its `task.md` is invalid.
    pub title: String,
}

/// `details` of an `illegal_transition` error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IllegalTransition {
    /// Current status.
    pub from: StatusName,
    /// Requested status.
    pub to: StatusName,
}

/// `details` of a `conflict` error: an undo whose field changed since.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UndoConflict {
    /// The field that no longer has the value the undo expects.
    pub field: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_only_reasons_wrap_the_domain_ones() {
        let reason: TaskReadOnly = ReadOnlyReason::UnknownStatus("blocked".to_owned()).into();
        assert_eq!(
            serde_json::to_value(&reason).unwrap(),
            json!({ "reason": "unknown_status", "value": "blocked" })
        );
        assert_eq!(
            serde_json::to_value(TaskReadOnly::InvalidFile("line 2: x".to_owned())).unwrap(),
            json!({ "reason": "invalid_file", "value": "line 2: x" })
        );
        assert_eq!(
            serde_json::to_value(TaskReadOnly::Archived(2026)).unwrap(),
            json!({ "reason": "archived", "value": 2026 })
        );
    }

    #[test]
    fn action_names_follow_the_actions_of_the_domain() {
        assert_eq!(
            TaskActionName::of(Action::SetStatus(StatusName::Done)),
            Some(TaskActionName::Done)
        );
        assert_eq!(TaskActionName::of(Action::Edit), None);
        assert_eq!(
            serde_json::to_value(TaskActionName::LogNote).unwrap(),
            json!("log_note")
        );
        for status in StatusName::ALL {
            let name = TaskActionName::of(Action::SetStatus(status)).unwrap();
            assert_eq!(
                serde_json::to_value(name).unwrap(),
                serde_json::to_value(status).unwrap()
            );
        }
    }

    #[test]
    fn action_requests_reject_unknown_fields() {
        let parse = |value| serde_json::from_value::<TaskActionRequest>(value);
        assert_eq!(parse(json!({})).unwrap(), TaskActionRequest::default());
        assert_eq!(
            parse(json!({ "on": "Alice", "until": "2026-10-20" })).unwrap(),
            TaskActionRequest {
                on: Some("Alice".to_owned()),
                until: Some("2026-10-20".parse().unwrap()),
                text: None,
                ..Default::default()
            }
        );
        assert!(parse(json!({ "note": "x" })).is_err());
        assert!(parse(json!({ "until": "tomorrow" })).is_err());
    }
}
