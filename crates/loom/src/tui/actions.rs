//! Task actions that the TUI sends to `loomd`: accept, snooze, cancel, done, go, undo and create.
//!
//! The state reducer only names an [`AppEffect`]; this module turns it into API calls.

use std::path::Path;

use loom_api::{
    ContextName, Date, GoResult, Importance, Kind, TaskActionRequest, TaskDetail, TaskId,
};

use crate::client::Client;
use crate::task::{AddRequest, new_task, write_cd_file};

use super::state::AppEffect;

/// One call to `loomd`, built from an [`AppEffect`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::option_option)]
pub enum ActionRequest {
    /// `POST /v1/tasks/{id}/actions/accept`.
    Accept {
        /// Task to accept.
        id: TaskId,
        /// New kind.
        kind: Option<Kind>,
        /// New importance.
        importance: Option<Importance>,
        /// New due date, `Some(None)` removes it.
        due: Option<Option<Date>>,
    },
    /// `POST /v1/tasks/{id}/actions/waiting`.
    Snooze {
        /// Task to snooze.
        id: TaskId,
        /// Date when the task shows again.
        until: Date,
        /// What the task waits on.
        on: Option<String>,
    },
    /// `POST /v1/tasks/{id}/actions/cancelled`.
    Cancel(TaskId),
    /// `POST /v1/tasks/{id}/actions/done`.
    Done(TaskId),
    /// `POST /v1/tasks/{id}/go`.
    Go(TaskId),
    /// `POST /v1/tasks/{id}/undo`.
    Undo(TaskId),
    /// `POST /v1/tasks`.
    Create {
        /// Title of the new task.
        title: String,
        /// Context of the new task.
        context: ContextName,
    },
}

impl ActionRequest {
    /// The call for an effect, or the effect itself when it is not a task action.
    ///
    /// # Errors
    ///
    /// Returns `effect` when it is not a task action.
    pub fn from_effect(effect: AppEffect) -> Result<Self, AppEffect> {
        match effect {
            AppEffect::AcceptTask {
                id,
                kind,
                importance,
                due,
            } => Ok(Self::Accept {
                id,
                kind,
                importance,
                due,
            }),
            AppEffect::SnoozeTask { id, until, on } => Ok(Self::Snooze { id, until, on }),
            AppEffect::CancelTask(id) => Ok(Self::Cancel(id)),
            AppEffect::DoneTask(id) => Ok(Self::Done(id)),
            AppEffect::GoTask(id) => Ok(Self::Go(id)),
            AppEffect::UndoTask(id) => Ok(Self::Undo(id)),
            AppEffect::CreateTask { title, context } => Ok(Self::Create { title, context }),
            effect @ (AppEffect::None
            | AppEffect::Refresh
            | AppEffect::FetchTask(_)
            | AppEffect::Search(_)) => Err(effect),
        }
    }
}

/// What the TUI does after an action succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionOutcome {
    /// Task that `u` undoes next, when the action changed one.
    pub task_id: Option<TaskId>,
    /// The action was an undo: nothing is left to undo.
    pub undone: bool,
    /// Reload the queues.
    pub should_refresh: bool,
    /// Leave the TUI.
    pub should_quit: bool,
    /// Text for the status bar.
    pub message: Option<String>,
}

/// Result of an action: the outcome, or the text of the error.
pub type ActionResponse = Result<ActionOutcome, String>;

const fn changed(id: TaskId, message: String) -> ActionOutcome {
    ActionOutcome {
        task_id: Some(id),
        undone: false,
        should_refresh: true,
        should_quit: false,
        message: Some(message),
    }
}

/// Sends `request` to `loomd`. `go` writes the repository path to `cd_file`, if any.
///
/// # Errors
///
/// Returns the text of the error when `loomd` refuses the call or does not answer.
pub async fn execute_action(
    client: &Client,
    request: ActionRequest,
    cd_file: Option<&Path>,
) -> ActionResponse {
    match request {
        ActionRequest::Accept {
            id,
            kind,
            importance,
            due,
        } => {
            let body = TaskActionRequest {
                kind,
                importance,
                due,
                ..Default::default()
            };
            post_action(client, &id, "accept", Some(&body)).await?;
            let message = format!("task {id} accepted");
            Ok(changed(id, message))
        }
        ActionRequest::Snooze { id, until, on } => {
            let body = TaskActionRequest {
                on,
                until: Some(until),
                ..Default::default()
            };
            post_action(client, &id, "waiting", Some(&body)).await?;
            let message = format!("task {id} snoozed until {until}");
            Ok(changed(id, message))
        }
        ActionRequest::Cancel(id) => {
            post_action(client, &id, "cancelled", None).await?;
            let message = format!("task {id} cancelled");
            Ok(changed(id, message))
        }
        ActionRequest::Done(id) => {
            post_action(client, &id, "done", None).await?;
            let message = format!("task {id} marked done");
            Ok(changed(id, message))
        }
        ActionRequest::Go(id) => {
            let result: GoResult = client
                .post(&format!("/v1/tasks/{id}/go"), None::<&()>)
                .await
                .map_err(|error| error.to_string())?;
            write_cd_file(cd_file, result.repo_path.as_deref())
                .map_err(|error| format!("cannot write the repository path: {error}"))?;
            Ok(ActionOutcome {
                task_id: None,
                undone: false,
                should_refresh: false,
                should_quit: true,
                message: None,
            })
        }
        ActionRequest::Undo(id) => {
            let _detail: TaskDetail = client
                .post(&format!("/v1/tasks/{id}/undo"), None::<&()>)
                .await
                .map_err(|error| error.to_string())?;
            Ok(ActionOutcome {
                task_id: None,
                undone: true,
                should_refresh: true,
                should_quit: false,
                message: Some(format!("undid last change on {id}")),
            })
        }
        ActionRequest::Create { title, context } => {
            let body = new_task(AddRequest {
                title,
                context,
                kind: Kind::Chore,
                importance: None,
                due: None,
                parent: None,
                repo: None,
                ready: false,
            });
            let _detail: TaskDetail = client
                .post("/v1/tasks", Some(&body))
                .await
                .map_err(|error| error.to_string())?;
            Ok(ActionOutcome {
                task_id: None,
                undone: false,
                should_refresh: true,
                should_quit: false,
                message: Some("task created in inbox".to_owned()),
            })
        }
    }
}

async fn post_action(
    client: &Client,
    id: &TaskId,
    name: &str,
    body: Option<&TaskActionRequest>,
) -> Result<(), String> {
    let _detail: TaskDetail = client
        .post(&format!("/v1/tasks/{id}/actions/{name}"), body)
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}
