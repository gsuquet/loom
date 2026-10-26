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
    /// Task whose last change the action undid.
    pub undone: Option<TaskId>,
    /// Reload the queues.
    pub should_refresh: bool,
    /// Leave the TUI.
    pub should_quit: bool,
    /// Text for the status bar.
    pub message: Option<String>,
}

impl ActionOutcome {
    /// The task that `u` undoes next, once this outcome is known.
    ///
    /// A changed task becomes the target. An undo clears the target only when it undid that task:
    /// an undo that ran after a newer change leaves the newer target.
    #[must_use]
    pub fn next_undo_target(&self, current: Option<&TaskId>) -> Option<TaskId> {
        if let Some(changed) = &self.task_id {
            return Some(changed.clone());
        }
        match (&self.undone, current) {
            (Some(undone), Some(current)) if undone == current => None,
            (_, current) => current.cloned(),
        }
    }
}

/// What to do with an action that the user starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submission {
    /// Send it to the worker now.
    Send(ActionRequest),
    /// Another action runs: it waits in the queue slot.
    Kept,
    /// Another action runs and the slot is full: it is dropped.
    Dropped,
}

/// Decides what happens to `request`. While an action runs, one request waits in `queued`.
pub fn submit(
    in_flight: bool,
    queued: &mut Option<ActionRequest>,
    request: ActionRequest,
) -> Submission {
    if !in_flight {
        Submission::Send(request)
    } else if queued.is_some() {
        Submission::Dropped
    } else {
        *queued = Some(request);
        Submission::Kept
    }
}

/// Result of an action: the outcome, or the text of the error.
pub type ActionResponse = Result<ActionOutcome, String>;

const fn changed(id: TaskId, message: String) -> ActionOutcome {
    ActionOutcome {
        task_id: Some(id),
        undone: None,
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
                undone: None,
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
                undone: Some(id.clone()),
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
                undone: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> TaskId {
        text.parse().unwrap()
    }

    fn done(text: &str) -> ActionRequest {
        ActionRequest::Done(id(text))
    }

    fn outcome(task_id: Option<TaskId>, undone: Option<TaskId>) -> ActionOutcome {
        ActionOutcome {
            task_id,
            undone,
            should_refresh: true,
            should_quit: false,
            message: None,
        }
    }

    #[test]
    fn an_idle_worker_gets_the_action_at_once() {
        let mut queued = None;
        assert_eq!(
            submit(false, &mut queued, done("k3q7zm2a")),
            Submission::Send(done("k3q7zm2a"))
        );
        assert_eq!(queued, None);
    }

    #[test]
    fn one_action_waits_and_a_third_is_dropped() {
        let mut queued = None;
        assert_eq!(
            submit(true, &mut queued, done("k3q7zm2a")),
            Submission::Kept
        );
        assert_eq!(queued, Some(done("k3q7zm2a")));
        assert_eq!(
            submit(true, &mut queued, done("m9x2rt4b")),
            Submission::Dropped
        );
        assert_eq!(queued, Some(done("k3q7zm2a")));
    }

    #[test]
    fn the_kept_action_goes_out_when_the_running_one_ends() {
        let mut queued = None;
        submit(true, &mut queued, done("k3q7zm2a"));
        // The runtime takes the slot after a response, then submits it with nothing running.
        let next = queued.take().unwrap();
        assert_eq!(
            submit(false, &mut queued, next),
            Submission::Send(done("k3q7zm2a"))
        );
        assert_eq!(queued, None);
    }

    #[test]
    fn a_changed_task_becomes_the_undo_target() {
        let outcome = outcome(Some(id("k3q7zm2a")), None);
        assert_eq!(
            outcome.next_undo_target(Some(&id("m9x2rt4b"))),
            Some(id("k3q7zm2a"))
        );
    }

    #[test]
    fn an_undo_clears_the_target_it_undid() {
        let outcome = outcome(None, Some(id("k3q7zm2a")));
        assert_eq!(outcome.next_undo_target(Some(&id("k3q7zm2a"))), None);
    }

    #[test]
    fn an_undo_keeps_a_newer_target() {
        // Undo of A ran after the user changed B: `u` must still reach B.
        let outcome = outcome(None, Some(id("k3q7zm2a")));
        assert_eq!(
            outcome.next_undo_target(Some(&id("m9x2rt4b"))),
            Some(id("m9x2rt4b"))
        );
    }

    #[test]
    fn other_actions_keep_the_target() {
        let outcome = outcome(None, None);
        assert_eq!(
            outcome.next_undo_target(Some(&id("m9x2rt4b"))),
            Some(id("m9x2rt4b"))
        );
        assert_eq!(outcome.next_undo_target(None), None);
    }
}
