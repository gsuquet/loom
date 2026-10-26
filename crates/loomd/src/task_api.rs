//! Decisions of the task endpoints, without I/O: query and body checks, prefix resolution,
//! the list and its ranking, the response of a task, and the problem of each refusal.

use std::collections::HashMap;
use std::path::PathBuf;

use axum::http::StatusCode;
use loom_api::{
    AmbiguousPrefix, ApiError, ContextName, Date, ErrorCode, FocusEntry, FocusList, GoResult,
    IllegalTransition, Link, NewTask, Status, StatusName, Task, TaskActionName, TaskActionRequest,
    TaskCandidate, TaskDetail, TaskEdit, TaskId, TaskList, TaskRank, TaskReadOnly, TaskSummary,
    Timestamp, UndoConflict,
};
use loom_core::shortest_unique_prefixes;
use loom_core::{Command, DomainError, PrefixError, available_actions, rank, resolve_prefix};
use loom_store::port::TaskLocation;
use loom_store::tasks::{FocusRow, IndexedState, IndexedTask, TaskError};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::api::{internal, invalid_request};

/// `YYYY-MM-DD`: the date of the priority list in a debug build, instead of the local date
/// (tests).
pub const TEST_TODAY_ENV: &str = "LOOM_TEST_TODAY";

/// A refused request: its HTTP status and its body.
pub type Problem = (StatusCode, ApiError);

/// Keys of a [`NewTask`] body: `kind` and `format` of its kind, `status`, `waiting_on` and
/// `waiting_until` of its status are flattened, so serde cannot reject the other keys itself.
pub const NEW_TASK_FIELDS: [&str; 12] = [
    "title",
    "kind",
    "format",
    "context",
    "status",
    "waiting_on",
    "waiting_until",
    "importance",
    "due",
    "pinned",
    "links",
    "relations",
];

/// Keys of a [`TaskEdit`] body.
pub const TASK_EDIT_FIELDS: [&str; 6] =
    ["title", "kind", "importance", "due", "links", "relations"];

/// Query parameters of `GET /v1/tasks`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskListQuery {
    /// Only the tasks of this context; ignored when empty.
    pub context: Option<String>,
    /// Only the tasks with this status; ignored when empty.
    pub status: Option<String>,
    /// `true`: the priority list.
    pub ranked: Option<bool>,
    /// `true`: the archived tasks instead of the others; `any`: both.
    pub archived: Option<ArchivedFilter>,
    /// Only the tasks whose parent is one of these tasks (full IDs, separated by commas);
    /// ignored when empty.
    pub parent: Option<String>,
}

/// Which locations `GET /v1/tasks` lists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum ArchivedFilter {
    /// The tasks of `tasks/` only.
    #[default]
    #[serde(rename = "false")]
    Active,
    /// The archived tasks only.
    #[serde(rename = "true")]
    Archived,
    /// Both.
    #[serde(rename = "any")]
    Any,
}

/// Query parameters of `GET /v1/focus`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FocusQuery {
    /// Only the focus of this context; ignored when empty.
    pub context: Option<String>,
}

/// The context filter of `GET /v1/focus`.
///
/// # Errors
///
/// `invalid_request` when `context` is not a context name.
pub fn focus_context(query: FocusQuery) -> Result<Option<ContextName>, ApiError> {
    query
        .context
        .filter(|context| !context.is_empty())
        .map_or(Ok(None), |context| {
            context
                .parse::<ContextName>()
                .map(Some)
                .map_err(|error| invalid_request(format!("invalid context: {error}")))
        })
}

impl ArchivedFilter {
    /// Whether a task at `location` is listed.
    const fn keeps(self, location: TaskLocation) -> bool {
        match self {
            Self::Active => matches!(location, TaskLocation::Active),
            Self::Archived => !matches!(location, TaskLocation::Active),
            Self::Any => true,
        }
    }
}

/// The filter of a task list, checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskFilter {
    /// Only this context.
    pub context: Option<ContextName>,
    /// Only this status.
    pub status: Option<StatusName>,
    /// The priority list: the tasks [`loom_core::rank`] lists, in its order.
    pub ranked: bool,
    /// Which locations to list.
    pub archived: ArchivedFilter,
    /// Only the children of these tasks; empty keeps every task.
    pub parent: Vec<TaskId>,
}

/// The filter of `query`.
///
/// # Errors
///
/// `invalid_request` when `context` is not a context name or `status` not a status name.
pub fn task_filter(query: TaskListQuery) -> Result<TaskFilter, ApiError> {
    let context = focus_context(FocusQuery {
        context: query.context,
    })?;
    let status = match query.status.filter(|status| !status.is_empty()) {
        None => None,
        Some(status) => Some(
            serde_json::from_value::<StatusName>(Value::String(status.clone())).map_err(|_| {
                invalid_request(format!(
                    "invalid status `{status}`: inbox, ready, active, waiting, done or cancelled"
                ))
            })?,
        ),
    };
    let parent = query
        .parent
        .iter()
        .flat_map(|parents| parents.split(','))
        .filter(|parent| !parent.is_empty())
        .map(|parent| {
            parent
                .parse::<TaskId>()
                .map_err(|error| invalid_request(format!("invalid parent `{parent}`: {error}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TaskFilter {
        context,
        status,
        ranked: query.ranked.unwrap_or(false),
        archived: query.archived.unwrap_or_default(),
        parent,
    })
}

/// The tasks of `rows` (every indexed task) that `filter` keeps: sorted by ID, or in priority
/// order on `today` with `ranked`. Prefixes are unique across every task of `rows`.
#[must_use]
pub fn task_list(rows: &[IndexedTask], filter: &TaskFilter, today: Date) -> TaskList {
    let ids: Vec<TaskId> = rows.iter().map(|row| row.id.clone()).collect();
    let prefixes: HashMap<&TaskId, &str> = ids.iter().zip(shortest_unique_prefixes(&ids)).collect();
    let prefix = |id: &TaskId| {
        prefixes
            .get(id)
            .map_or_else(|| id.to_string(), |prefix| (*prefix).to_owned())
    };
    let kept = rows.iter().filter(|row| {
        filter.archived.keeps(row.location)
            && filter
                .context
                .as_ref()
                .is_none_or(|context| row.context == *context)
            && filter.status.is_none_or(|status| {
                task_of(row).and_then(|task| task.status.name()) == Some(status)
            })
            && (filter.parent.is_empty()
                || task_of(row).is_some_and(|task| {
                    task.relations
                        .parent
                        .as_ref()
                        .is_some_and(|parent| filter.parent.contains(parent))
                }))
    });
    let mut kept: Vec<&IndexedTask> = kept.collect();
    if !filter.ranked {
        kept.sort_by(|a, b| a.id.cmp(&b.id));
        return TaskList {
            tasks: kept
                .into_iter()
                .map(|row| summary(row, &prefix(&row.id), None))
                .collect(),
        };
    }
    // An archived task is never ranked, even reopened by hand: `archived` with `ranked` lists
    // nothing.
    let tasks: Vec<Task> = kept
        .iter()
        .filter(|row| row.location == TaskLocation::Active)
        .filter_map(|row| task_of(row).cloned())
        .collect();
    let by_id: HashMap<&TaskId, &IndexedTask> = kept.iter().map(|row| (&row.id, *row)).collect();
    TaskList {
        tasks: rank(&tasks, today)
            .into_iter()
            .filter_map(|ranked| {
                by_id.get(&ranked.task_id).map(|row| {
                    summary(
                        row,
                        &prefix(&row.id),
                        Some(TaskRank {
                            position: ranked.position,
                            reason: ranked.reason,
                        }),
                    )
                })
            })
            .collect(),
    }
}

/// The task of `rows` whose ID starts with `prefix` (see [`loom_core::resolve_prefix`]).
///
/// # Errors
///
/// `invalid_request` (400) for a malformed prefix, `not_found` (404) when no task matches,
/// `ambiguous_prefix` (400) with the candidates when several do.
pub fn resolve<'a>(prefix: &str, rows: &'a [IndexedTask]) -> Result<&'a IndexedTask, Problem> {
    let ids: Vec<TaskId> = rows.iter().map(|row| row.id.clone()).collect();
    match resolve_prefix(prefix, &ids) {
        Ok(id) => rows.iter().find(|row| row.id == id).ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                ApiError::new(ErrorCode::NotFound, format!("no task `{id}`")),
            )
        }),
        Err(error @ PrefixError::Invalid(_)) => {
            Err((StatusCode::BAD_REQUEST, invalid_request(error.to_string())))
        }
        Err(error @ PrefixError::Unknown(_)) => Err((
            StatusCode::NOT_FOUND,
            ApiError::new(ErrorCode::NotFound, error.to_string()),
        )),
        Err(PrefixError::Ambiguous { prefix, candidates }) => {
            let message = format!("`{prefix}` matches {} tasks", candidates.len());
            let details = AmbiguousPrefix {
                prefix,
                candidates: candidates
                    .iter()
                    .filter_map(|id| rows.iter().find(|row| row.id == *id))
                    .map(|row| TaskCandidate {
                        id: row.id.clone(),
                        context: row.context.clone(),
                        title: row.title().to_owned(),
                    })
                    .collect(),
            };
            Err((
                StatusCode::BAD_REQUEST,
                with_details(ApiError::new(ErrorCode::AmbiguousPrefix, message), &details),
            ))
        }
    }
}

/// The response of `row`, its prefix unique among `ids` (every indexed task ID).
#[must_use]
pub fn task_detail(row: &IndexedTask, ids: &[TaskId]) -> TaskDetail {
    let prefix = unique_prefix(&row.id, ids);
    let (body, mut actions) = match &row.state {
        IndexedState::Valid { task, body } => (body.clone(), available_actions(task)),
        IndexedState::Invalid { text, .. } => (text.clone(), Vec::new()),
    };
    if row.location != TaskLocation::Active {
        actions.clear();
    }
    TaskDetail {
        summary: summary(row, prefix, None),
        body,
        actions,
    }
}

/// The summary of `row`, its prefix unique among `ids` (every indexed task ID).
#[must_use]
pub fn task_summary(row: &IndexedTask, ids: &[TaskId]) -> TaskSummary {
    summary(row, unique_prefix(&row.id, ids), None)
}

fn unique_prefix<'a>(target: &'a TaskId, ids: &'a [TaskId]) -> &'a str {
    ids.iter()
        .position(|id| id == target)
        .and_then(|index| shortest_unique_prefixes(ids).get(index).copied())
        .unwrap_or_else(|| target.as_str())
}

/// The repository slug of the first `Link::Repo` of `task`, if any.
#[must_use]
pub fn linked_repo_slug(task: &Task) -> Option<&str> {
    task.links.iter().find_map(|link| match link {
        Link::Repo { repo } => Some(repo.as_str()),
        Link::Origin(_) | Link::Related(_) => None,
    })
}

/// The response of `GET /v1/focus` from `focused` rows and `ids` (every indexed task ID).
pub fn focus_list(
    focused: &[(FocusRow, IndexedTask)],
    ids: &[TaskId],
    mut repo_path: impl FnMut(&str) -> Option<PathBuf>,
) -> FocusList {
    let prefixes: HashMap<&TaskId, &str> = ids.iter().zip(shortest_unique_prefixes(ids)).collect();
    let entries = focused
        .iter()
        .map(|(focus, row)| {
            let prefix = prefixes
                .get(&row.id)
                .map_or_else(|| row.id.as_str(), |p| *p);
            let repo_path = task_of(row)
                .and_then(linked_repo_slug)
                .and_then(&mut repo_path);
            FocusEntry {
                context: focus.context.clone(),
                task: summary(row, prefix, None),
                since: focus.since,
                repo_path,
            }
        })
        .collect();
    FocusList { entries }
}

/// The response of `POST /v1/tasks/{id}/go`.
#[must_use]
pub fn go_result(
    row: &IndexedTask,
    switched_from: Option<&IndexedTask>,
    since: Timestamp,
    ids: &[TaskId],
    repo_path: Option<PathBuf>,
) -> GoResult {
    GoResult {
        task: task_detail(row, ids),
        switched_from: switched_from.map(|prev| task_summary(prev, ids)),
        since,
        repo_path,
    }
}

fn summary(row: &IndexedTask, prefix: &str, rank: Option<TaskRank>) -> TaskSummary {
    let (task, read_only) = match &row.state {
        IndexedState::Valid { task, .. } => (
            Some((**task).clone()),
            task.read_only_reason().map(TaskReadOnly::from),
        ),
        IndexedState::Invalid { error, .. } => {
            (None, Some(TaskReadOnly::InvalidFile(error.clone())))
        }
    };
    // Archived comes first: fixing the file or upgrading loom would not make it writable.
    let read_only = match row.location {
        TaskLocation::Archived(year) => Some(TaskReadOnly::Archived(year)),
        TaskLocation::Active => read_only,
    };
    TaskSummary {
        id: row.id.clone(),
        prefix: prefix.to_owned(),
        context: row.context.clone(),
        task,
        read_only,
        rank,
    }
}

const fn task_of(row: &IndexedTask) -> Option<&Task> {
    match &row.state {
        IndexedState::Valid { task, .. } => Some(task),
        IndexedState::Invalid { .. } => None,
    }
}

/// The task `body` asks to create.
///
/// # Errors
///
/// `invalid_request` for a key that is not a field of [`NewTask`], or a body that is not one.
pub fn new_task(body: Value) -> Result<NewTask, ApiError> {
    checked_body(body, &NEW_TASK_FIELDS)
}

/// The edit `body` asks for.
///
/// # Errors
///
/// `invalid_request` for a key that is not a field of [`TaskEdit`] (the status and the pin are
/// actions), or a body that is not one.
pub fn task_edit(body: Value) -> Result<TaskEdit, ApiError> {
    checked_body(body, &TASK_EDIT_FIELDS)
}

/// `body` read as a `T`, an object whose keys are all in `fields`.
fn checked_body<T: DeserializeOwned>(body: Value, fields: &[&str]) -> Result<T, ApiError> {
    let Value::Object(object) = &body else {
        return Err(invalid_request("the body must be a JSON object"));
    };
    if let Some(unknown) = object.keys().find(|key| !fields.contains(&key.as_str())) {
        return Err(invalid_request(format!(
            "unknown field `{unknown}`, expected one of: {}",
            fields.join(", ")
        )));
    }
    serde_json::from_value(body).map_err(|error| invalid_request(error.to_string()))
}

/// The command of `action` with `request`.
///
/// # Errors
///
/// `invalid_request` when `request` has a field the action does not take, or `log_note`
/// has no `text`.
pub fn action_command(
    action: TaskActionName,
    request: TaskActionRequest,
) -> Result<Command, ApiError> {
    let TaskActionRequest {
        on,
        until,
        text,
        kind,
        importance,
        due,
    } = request;
    // The first field of `request` the action does not take, if any.
    let wait_field = if on.is_some() {
        Some("on")
    } else if until.is_some() {
        Some("until")
    } else {
        None
    };
    let text_field = text.is_some().then_some("text");
    let accept_field = if kind.is_some() {
        Some("kind")
    } else if importance.is_some() {
        Some("importance")
    } else if due.is_some() {
        Some("due")
    } else {
        None
    };
    let refuse = |field: Option<&str>| {
        field.map_or(Ok(()), |field| {
            Err(invalid_request(format!(
                "`{field}` is not a field of the action `{}`",
                action_name(action)
            )))
        })
    };
    let plain = |command: Command| {
        refuse(wait_field.or(text_field).or(accept_field))?;
        Ok(command)
    };
    let status = |status: Status| Command::SetStatus { status };
    match action {
        TaskActionName::Inbox => plain(status(Status::Inbox)),
        TaskActionName::Ready => plain(status(Status::Ready)),
        TaskActionName::Active => plain(status(Status::Active)),
        TaskActionName::Done => plain(status(Status::Done)),
        TaskActionName::Cancelled => plain(status(Status::Cancelled)),
        TaskActionName::Pin => plain(Command::Pin),
        TaskActionName::Unpin => plain(Command::Unpin),
        TaskActionName::Accept => {
            refuse(wait_field.or(text_field))?;
            Ok(Command::Accept {
                kind,
                importance,
                due,
            })
        }
        TaskActionName::Waiting => {
            refuse(text_field.or(accept_field))?;
            Ok(status(Status::Waiting { on, until }))
        }
        TaskActionName::LogNote => {
            refuse(wait_field.or(accept_field))?;
            text.map(|text| Command::LogNote { text })
                .ok_or_else(|| invalid_request("the action `log_note` needs a `text`"))
        }
    }
}

/// The body of an action: empty (whitespace only) for none.
///
/// # Errors
///
/// `invalid_request` for a body that is not a [`TaskActionRequest`].
pub fn action_request(body: &[u8]) -> Result<TaskActionRequest, ApiError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(TaskActionRequest::default());
    }
    // Through `Value`: serde reads a struct from an array too.
    match serde_json::from_slice(body) {
        Ok(value @ Value::Object(_)) => {
            serde_json::from_value(value).map_err(|error| invalid_request(error.to_string()))
        }
        Ok(_) => Err(invalid_request("the body must be a JSON object")),
        Err(error) => Err(invalid_request(error.to_string())),
    }
}

/// The action named `segment` in the path, if there is one.
#[must_use]
pub fn action_name_of(segment: &str) -> Option<TaskActionName> {
    serde_json::from_value(Value::String(segment.to_owned())).ok()
}

/// The action as named in the path.
fn action_name(action: TaskActionName) -> String {
    serde_json::to_value(action)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The problem of a refused or failed task operation.
#[must_use]
pub fn task_problem(error: &TaskError) -> Problem {
    let message = error.to_string();
    match error {
        TaskError::NotFound(_) => (
            StatusCode::NOT_FOUND,
            ApiError::new(ErrorCode::NotFound, message),
        ),
        TaskError::UnknownContext(_) => (StatusCode::BAD_REQUEST, invalid_request(message)),
        TaskError::Invalid(invalid) => (
            StatusCode::CONFLICT,
            with_details(
                ApiError::new(ErrorCode::ReadOnly, message),
                &TaskReadOnly::InvalidFile(invalid.to_string()),
            ),
        ),
        TaskError::Archived { year, .. } => (
            StatusCode::CONFLICT,
            with_details(
                ApiError::new(ErrorCode::ReadOnly, message),
                &TaskReadOnly::Archived(*year),
            ),
        ),
        TaskError::Domain(refused) => domain_problem(refused, message),
        TaskError::Files(_) | TaskError::Index(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, internal(message))
        }
    }
}

fn domain_problem(refused: &DomainError, message: String) -> Problem {
    match refused {
        DomainError::ReadOnly(reason) => (
            StatusCode::CONFLICT,
            with_details(
                ApiError::new(ErrorCode::ReadOnly, message),
                &TaskReadOnly::from(reason.clone()),
            ),
        ),
        DomainError::IllegalTransition { from, to } => (
            StatusCode::CONFLICT,
            with_details(
                ApiError::new(ErrorCode::IllegalTransition, message),
                &IllegalTransition {
                    from: *from,
                    to: *to,
                },
            ),
        ),
        DomainError::Conflict(field) => (
            StatusCode::CONFLICT,
            with_details(
                ApiError::new(ErrorCode::Conflict, message),
                &UndoConflict {
                    field: (*field).to_owned(),
                },
            ),
        ),
        DomainError::NothingToUndo => (
            StatusCode::CONFLICT,
            ApiError::new(ErrorCode::NothingToUndo, message),
        ),
        DomainError::UnknownValue(_)
        | DomainError::EmptyTitle
        | DomainError::EmptyNote
        | DomainError::InvalidParent { .. } => (StatusCode::BAD_REQUEST, invalid_request(message)),
        DomainError::Id(_) => (StatusCode::INTERNAL_SERVER_ERROR, internal(message)),
    }
}

/// `error` with `details`; without them when they cannot be serialized.
fn with_details(error: ApiError, details: &impl serde::Serialize) -> ApiError {
    ApiError {
        details: serde_json::to_value(details).ok(),
        ..error
    }
}

/// The date of the priority list: `test` (`YYYY-MM-DD`, from `LOOM_TEST_TODAY`) when
/// `allowed` (a debug build) and valid, otherwise the local date `local` reads.
pub fn today(test: Option<&str>, allowed: bool, local: impl FnOnce() -> Date) -> Date {
    test.filter(|_| allowed)
        .and_then(|text| text.parse().ok())
        .unwrap_or_else(local)
}

#[cfg(test)]
mod tests;
