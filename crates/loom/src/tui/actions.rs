//! Actions that the TUI sends to `loomd`: accept, snooze, cancel, done, go, undo and create a
//! task, and go to a repository.
//!
//! The state reducer only names an [`AppEffect`]; this module turns it into API calls.

use std::path::{Path, PathBuf};

use loom_api::{
    ContextName, Date, GoResult, Importance, Kind, RegisterRequest, RepoSummary, StatusName,
    TaskActionRequest, TaskDetail, TaskEdit, TaskId, TaskMoveRequest,
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
    /// `POST /v1/tasks/{id}/actions/{status}`, for a status other than `waiting`.
    SetStatus {
        /// Task to move.
        id: TaskId,
        /// The status.
        status: StatusName,
    },
    /// `POST /v1/tasks/{id}/move`.
    Move {
        /// Task to move.
        id: TaskId,
        /// The context to move it to.
        context: ContextName,
    },
    /// `PATCH /v1/tasks/{id}`.
    Edit {
        /// Task to edit.
        id: TaskId,
        /// New kind.
        kind: Option<Kind>,
        /// New importance.
        importance: Option<Importance>,
        /// New due date, `Some(None)` removes it.
        due: Option<Option<Date>>,
    },
    /// `POST /v1/tasks/{id}/actions/cancelled`.
    Cancel(TaskId),
    /// `POST /v1/tasks/{id}/actions/done`.
    Done(TaskId),
    /// `POST /v1/tasks/{id}/go`.
    Go(TaskId),
    /// `POST /v1/repos/register` on use, for the repository at this path.
    GoRepo(PathBuf),
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
            AppEffect::EditTask {
                id,
                kind,
                importance,
                due,
            } => Ok(Self::Edit {
                id,
                kind,
                importance,
                due,
            }),
            AppEffect::SetTaskStatus { id, status } => Ok(Self::SetStatus { id, status }),
            AppEffect::MoveTask { id, context } => Ok(Self::Move { id, context }),
            AppEffect::CancelTask(id) => Ok(Self::Cancel(id)),
            AppEffect::DoneTask(id) => Ok(Self::Done(id)),
            AppEffect::GoTask(id) => Ok(Self::Go(id)),
            AppEffect::GoRepo(path) => Ok(Self::GoRepo(path)),
            AppEffect::UndoTask(id) => Ok(Self::Undo(id)),
            AppEffect::CreateTask { title, context } => Ok(Self::Create { title, context }),
            effect @ (AppEffect::None
            | AppEffect::Refresh
            | AppEffect::ReloadPage
            | AppEffect::FetchTask(_)
            | AppEffect::FetchEvents(_)
            | AppEffect::FetchRelations(_)
            | AppEffect::EditBody(_)
            | AppEffect::Search(_)) => Err(effect),
        }
    }
}

/// What the TUI does after an action succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionOutcome {
    /// Task that `u` undoes next, when the action changed one.
    pub task_id: Option<TaskId>,
    /// Task whose last change the action undid, or that the action made impossible to undo (a
    /// move).
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

/// What to do with the kept action when the running one has ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// No action waits.
    Nothing,
    /// Send this action now.
    Send(ActionRequest),
    /// The kept action is dropped, for this reason.
    Dropped(&'static str),
}

/// Takes the kept action out of `queued` and decides if it still runs.
///
/// A failure drops it: it was typed for a screen that the failed action may have changed.
/// A kept undo runs only if `undo_target` is still its task, so that two quick presses of `u`
/// do not undo an older change.
pub fn take_next(
    queued: &mut Option<ActionRequest>,
    succeeded: bool,
    undo_target: Option<&TaskId>,
) -> Next {
    let Some(request) = queued.take() else {
        return Next::Nothing;
    };
    if !succeeded {
        return Next::Dropped("the action before it failed");
    }
    match &request {
        ActionRequest::Undo(id) if undo_target != Some(id) => Next::Dropped("nothing to undo"),
        ActionRequest::Undo(_)
        | ActionRequest::Accept { .. }
        | ActionRequest::Snooze { .. }
        | ActionRequest::Edit { .. }
        | ActionRequest::SetStatus { .. }
        | ActionRequest::Move { .. }
        | ActionRequest::Cancel(_)
        | ActionRequest::Done(_)
        | ActionRequest::Go(_)
        | ActionRequest::GoRepo(_)
        | ActionRequest::Create { .. } => Next::Send(request),
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

/// Sends `request` to `loomd`. `go` writes the repository path to `cd_file`, if any. Going to a
/// repository without `cd_file` keeps the TUI open, as it cannot change the directory.
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
        ActionRequest::Edit {
            id,
            kind,
            importance,
            due,
        } => edit_task(client, id, kind, importance, due).await,
        ActionRequest::SetStatus { id, status } => {
            let message = format!("task {id} moved to {status}");
            move_task(client, id, &status.to_string(), message).await
        }
        ActionRequest::Move { id, context } => move_to_context(client, id, context).await,
        ActionRequest::Cancel(id) => {
            let message = format!("task {id} cancelled");
            move_task(client, id, "cancelled", message).await
        }
        ActionRequest::Done(id) => {
            let message = format!("task {id} marked done");
            move_task(client, id, "done", message).await
        }
        ActionRequest::Go(id) => go_task(client, &id, cd_file).await,
        ActionRequest::GoRepo(path) => go_repo(client, path, cd_file).await,
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

/// Outcome of a go: leave the TUI, so that the shell function changes directory.
const QUIT: ActionOutcome = ActionOutcome {
    task_id: None,
    undone: None,
    should_refresh: false,
    should_quit: true,
    message: None,
};

/// Focuses the task `id`, writes its repository path to `cd_file`, if any, and quits.
async fn go_task(client: &Client, id: &TaskId, cd_file: Option<&Path>) -> ActionResponse {
    let result: GoResult = client
        .post(&format!("/v1/tasks/{id}/go"), None::<&()>)
        .await
        .map_err(|error| error.to_string())?;
    write_cd_file(cd_file, result.repo_path.as_deref())
        .map_err(|error| format!("cannot write the repository path: {error}"))?;
    Ok(QUIT)
}

/// Marks the repository at `path` used, writes its path to `cd_file` and quits.
async fn go_repo(client: &Client, path: PathBuf, cd_file: Option<&Path>) -> ActionResponse {
    let Some(cd_file) = cd_file.filter(|file| !file.as_os_str().is_empty()) else {
        return Ok(ActionOutcome {
            task_id: None,
            undone: None,
            should_refresh: false,
            should_quit: false,
            message: Some(format!(
                "{}: load `loom shell-init` to change to it",
                path.display()
            )),
        });
    };
    let request = RegisterRequest { path, on_use: true };
    let repo: RepoSummary = client
        .post("/v1/repos/register", Some(&request))
        .await
        .map_err(|error| error.to_string())?;
    write_cd_file(Some(cd_file), Some(&repo.identity.path))
        .map_err(|error| format!("cannot write the repository path: {error}"))?;
    Ok(QUIT)
}

// `due` follows `TaskEdit`: `Some(None)` removes the date.
#[allow(clippy::option_option)]
async fn edit_task(
    client: &Client,
    id: TaskId,
    kind: Option<Kind>,
    importance: Option<Importance>,
    due: Option<Option<Date>>,
) -> ActionResponse {
    let edit = TaskEdit {
        kind,
        importance,
        due,
        ..Default::default()
    };
    let _detail: TaskDetail = client
        .patch(&format!("/v1/tasks/{id}"), &edit)
        .await
        .map_err(|error| error.to_string())?;
    let message = format!("task {id} updated");
    Ok(changed(id, message))
}

async fn move_to_context(client: &Client, id: TaskId, context: ContextName) -> ActionResponse {
    let message = format!("task {id} moved to context {context}");
    let _detail: TaskDetail = client
        .post(
            &format!("/v1/tasks/{id}/move"),
            Some(&TaskMoveRequest { context }),
        )
        .await
        .map_err(|error| error.to_string())?;
    // A move is not undone: it ends what `u` can undo for this task, as an undo does.
    Ok(ActionOutcome {
        task_id: None,
        undone: Some(id),
        should_refresh: true,
        should_quit: false,
        message: Some(message),
    })
}

async fn move_task(client: &Client, id: TaskId, name: &str, message: String) -> ActionResponse {
    post_action(client, &id, name, None).await?;
    Ok(changed(id, message))
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

    fn undo(text: &str) -> ActionRequest {
        ActionRequest::Undo(id(text))
    }

    #[test]
    fn nothing_is_sent_when_nothing_waits() {
        assert_eq!(take_next(&mut None, true, None), Next::Nothing);
        assert_eq!(take_next(&mut None, false, None), Next::Nothing);
    }

    #[test]
    fn a_failure_drops_the_kept_action() {
        let mut queued = Some(done("k3q7zm2a"));
        assert!(matches!(
            take_next(&mut queued, false, None),
            Next::Dropped(_)
        ));
        assert_eq!(queued, None);
    }

    #[test]
    fn a_success_sends_the_kept_action() {
        let mut queued = Some(done("k3q7zm2a"));
        assert_eq!(
            take_next(&mut queued, true, None),
            Next::Send(done("k3q7zm2a"))
        );
        assert_eq!(queued, None);
    }

    #[test]
    fn a_kept_undo_runs_only_while_its_task_is_the_target() {
        let target = id("k3q7zm2a");
        let mut queued = Some(undo("k3q7zm2a"));
        assert_eq!(
            take_next(&mut queued, true, Some(&target)),
            Next::Send(undo("k3q7zm2a"))
        );
        // The first undo cleared the target: a second press of `u` must not undo an older change.
        let mut queued = Some(undo("k3q7zm2a"));
        assert_eq!(
            take_next(&mut queued, true, None),
            Next::Dropped("nothing to undo")
        );
        let mut queued = Some(undo("k3q7zm2a"));
        assert_eq!(
            take_next(&mut queued, true, Some(&id("m9x2rt4b"))),
            Next::Dropped("nothing to undo")
        );
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

    #[test]
    fn an_edit_effect_becomes_an_edit_call() {
        let effect = AppEffect::EditTask {
            id: id("k3q7zm2a"),
            kind: None,
            importance: Some(loom_core::Importance::P1),
            due: None,
        };
        assert_eq!(
            ActionRequest::from_effect(effect),
            Ok(ActionRequest::Edit {
                id: id("k3q7zm2a"),
                kind: None,
                importance: Some(loom_core::Importance::P1),
                due: None,
            })
        );
    }

    #[test]
    fn a_move_effect_becomes_a_move_call() {
        let effect = AppEffect::MoveTask {
            id: id("k3q7zm2a"),
            context: "home".parse().unwrap(),
        };
        assert_eq!(
            ActionRequest::from_effect(effect),
            Ok(ActionRequest::Move {
                id: id("k3q7zm2a"),
                context: "home".parse().unwrap(),
            })
        );
    }

    #[test]
    fn a_status_effect_becomes_a_status_call() {
        let effect = AppEffect::SetTaskStatus {
            id: id("k3q7zm2a"),
            status: StatusName::Done,
        };
        assert_eq!(
            ActionRequest::from_effect(effect),
            Ok(ActionRequest::SetStatus {
                id: id("k3q7zm2a"),
                status: StatusName::Done,
            })
        );
    }
}
