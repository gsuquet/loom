//! Editing a task file in the editor: the steps that `loom edit` and the TUI share.
//!
//! The steps are `begin` (read the task and the file), `run_editor` (blocking), and `finish`
//! (check the file, then wait for loomd to record it). None of them writes to the terminal: the
//! caller shows the `Outcome` or the `EditError`.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

use loom_api::{Actor, ContextName, EventRecord, TaskDetail, TaskEvents, TaskId, TaskReadOnly};
use thiserror::Error;

use crate::client::Client;
use crate::task::{changed_after_save, changed_meanwhile, editor, holds_saved, render_change};

/// Longest wait for loomd to record an edit: its watcher reads the file at once.
pub const RECORD_TIMEOUT: Duration = Duration::from_secs(5);
/// Interval between two reads of the edited task.
pub const POLL: Duration = Duration::from_millis(50);

/// Why an edit stops.
#[derive(Debug, Error)]
pub enum EditError {
    /// loomd refuses a call or does not answer.
    #[error("{0}")]
    Api(String),
    /// The task is in the archive.
    #[error(
        "task {prefix} is archived (archive/{year}): an archived task is read-only. To reopen it, set its status in archive/{year}/{id}/task.md, then move its directory back to tasks/."
    )]
    Archived {
        /// Prefix of the task.
        prefix: String,
        /// Year of the archive.
        year: u16,
        /// ID of the task.
        id: TaskId,
    },
    /// The context of the task has no store in the configuration.
    #[error("no store for context {0} in config.toml")]
    NoStore(ContextName),
    /// The task file cannot be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The file.
        path: String,
        /// The cause.
        source: io::Error,
    },
    /// The editor cannot start.
    #[error("cannot run the editor `{command}`: {source}")]
    EditorStart {
        /// The editor command.
        command: String,
        /// The cause.
        source: io::Error,
    },
    /// The editor ends with an error.
    #[error("the editor `{command}` exited with {status}: {path} is left as it is now")]
    EditorFailed {
        /// The editor command.
        command: String,
        /// How it ended.
        status: ExitStatus,
        /// The file.
        path: String,
    },
    /// The saved file is not a valid task.
    #[error(
        "{path}: {reason}\nThe file is kept as saved: loomd shows the task read-only until it is fixed (`loom edit {prefix}` again)."
    )]
    Invalid {
        /// The file.
        path: String,
        /// What is wrong.
        reason: String,
        /// Prefix of the task.
        prefix: String,
    },
}

/// A task file open for editing.
#[derive(Debug, Clone)]
pub struct Session {
    /// Full ID of the task: a task created while the editor is open may share the prefix.
    pub id: String,
    /// Prefix of the task, for messages.
    pub prefix: String,
    /// The task file.
    pub path: PathBuf,
    original: String,
    logged: usize,
}

/// What an edit tells the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The result line, or lines.
    pub text: String,
    /// Warnings, one per entry.
    pub warnings: Vec<String>,
    /// The file was saved as it was: nothing changed.
    pub unchanged: bool,
    /// loomd recorded the save. `false` when the wait for it ran out: the text says so, and a
    /// caller must not present the edit as a success.
    pub recorded: bool,
}

fn api(error: &impl std::fmt::Display) -> EditError {
    EditError::Api(error.to_string())
}

async fn events(client: &Client, id: &str) -> Result<Vec<EventRecord>, EditError> {
    let log: TaskEvents = client
        .get(&format!(
            "/v1/tasks/{}/events",
            crate::repo::encode_segment(id)
        ))
        .await
        .map_err(|error| api(&error))?;
    Ok(log.events)
}

async fn task(client: &Client, id: &str) -> Result<TaskDetail, EditError> {
    client
        .get(&format!("/v1/tasks/{}", crate::repo::encode_segment(id)))
        .await
        .map_err(|error| api(&error))
}

fn read(path: &Path) -> Result<String, EditError> {
    std::fs::read_to_string(path).map_err(|source| EditError::Read {
        path: path.display().to_string(),
        source,
    })
}

/// Reads the task and its file. `stores` maps each context to its store.
///
/// # Errors
///
/// [`EditError`] when the task is archived, has no store, or cannot be read.
pub async fn begin(
    client: &Client,
    id: &str,
    stores: &BTreeMap<ContextName, PathBuf>,
) -> Result<Session, EditError> {
    let before = task(client, id).await?;
    let summary = &before.summary;
    // A hand edit of an archived file would be recorded by loomd: the archive stays read-only,
    // as it is for the API.
    if let Some(TaskReadOnly::Archived(year)) = summary.read_only {
        return Err(EditError::Archived {
            prefix: summary.prefix.clone(),
            year,
            id: summary.id.clone(),
        });
    }
    let id = summary.id.to_string();
    let logged = events(client, &id).await?.len();
    let path = stores
        .get(&summary.context)
        .ok_or_else(|| EditError::NoStore(summary.context.clone()))?
        .join("tasks")
        .join(summary.id.as_str())
        .join("task.md");
    let original = read(&path)?;
    Ok(Session {
        id,
        prefix: summary.prefix.clone(),
        path,
        original,
        logged,
    })
}

/// Runs the editor on the task file and waits for it. The editor is `visual`, then `editor_var`,
/// then `vi`.
///
/// # Errors
///
/// [`EditError`] when the editor cannot start or ends with an error.
pub fn run_editor(
    session: &Session,
    visual: Option<&str>,
    editor_var: Option<&str>,
) -> Result<(), EditError> {
    let command = editor(visual, editor_var);
    // Through the shell, so that an editor with arguments (`code --wait`) works; the path is
    // passed as `$1`, never spliced into the command.
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{command} \"$1\""))
        .arg("sh")
        .arg(&session.path)
        .status()
        .map_err(|source| EditError::EditorStart {
            command: command.clone(),
            source,
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(EditError::EditorFailed {
            command,
            status,
            path: session.path.display().to_string(),
        })
    }
}

/// Checks the saved file, then waits for loomd to record it. An invalid file is reported and
/// kept as saved: loomd shows the task read-only until it is fixed.
///
/// # Errors
///
/// [`EditError`] when the file cannot be read or is not a valid task, or loomd fails.
pub async fn finish(client: &Client, session: &Session) -> Result<Outcome, EditError> {
    let saved = read(&session.path)?;
    if saved == session.original {
        return Ok(Outcome {
            text: format!("{} unchanged\n", session.prefix),
            warnings: Vec::new(),
            unchanged: true,
            recorded: true,
        });
    }
    let document = loom_core::parse_task_md(&saved).map_err(|error| EditError::Invalid {
        path: session.path.display().to_string(),
        reason: error.to_string(),
        prefix: session.prefix.clone(),
    })?;
    let saved_task = document.task(loom_api::Timestamp::EPOCH);
    let saved_notes = document.body().to_owned();
    let deadline = Instant::now() + RECORD_TIMEOUT;
    loop {
        let after = task(client, &session.id).await?;
        if holds_saved(&after, &saved_task, &saved_notes) {
            // Read once the save is indexed: its hand-edit event and what came before it.
            let log = events(client, &session.id).await?;
            // The log is append-only, in the order loomd writes it: the events after the first
            // `logged` arrived while the editor was open. (A merge of logs from several
            // machines that reorders them would break this.)
            let appended = log.get(session.logged..).unwrap_or_default();
            let others = changed_meanwhile(appended);
            let warnings = if others.is_empty() {
                Vec::new()
            } else {
                vec![format!(
                    "the task changed while it was open in the editor (by {}): your save replaced the file and loomd recorded it as an edit; `loom undo {}` reverts it",
                    others.join(", "),
                    session.prefix
                )]
            };
            return Ok(Outcome {
                text: render_change(&after),
                warnings,
                unchanged: false,
                recorded: true,
            });
        }
        if Instant::now() >= deadline {
            let log = events(client, &session.id).await?;
            return Ok(timed_out(
                log.get(session.logged..).unwrap_or_default(),
                &session.prefix,
                &after,
            ));
        }
        tokio::time::sleep(POLL).await;
    }
}

/// What an edit tells when the index did not hold the save in time. A hand-edit event in
/// `appended` means loomd recorded the save and the task changed again after it (a `loom done`
/// elsewhere, a schedule, a sync); that is told, with the actors.
fn timed_out(appended: &[EventRecord], prefix: &str, after: &TaskDetail) -> Outcome {
    if !appended.iter().any(|record| record.actor == Actor::File) {
        return Outcome {
            text: format!(
                "{prefix} saved; loomd has not recorded it within {} s\n",
                RECORD_TIMEOUT.as_secs()
            ),
            warnings: Vec::new(),
            unchanged: false,
            recorded: false,
        };
    }
    let after_save = changed_after_save(appended);
    let by = if after_save.is_empty() {
        String::new()
    } else {
        format!(" (by {})", after_save.join(", "))
    };
    Outcome {
        text: format!(
            "{prefix} saved; the task changed again after the save\n{}",
            render_change(after)
        ),
        warnings: vec![format!(
            "the task changed again after the save{by}: `loom show {prefix}` shows it"
        )],
        unchanged: false,
        recorded: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::render_change;
    use loom_api::{TaskDetail, TaskSummary};

    fn detail() -> TaskDetail {
        let id: TaskId = "k3q7zm2a".parse().unwrap();
        TaskDetail {
            summary: TaskSummary {
                id,
                prefix: "k3q7".to_owned(),
                context: "work".parse().unwrap(),
                task: None,
                read_only: None,
                rank: None,
            },
            body: String::new(),
            actions: Vec::new(),
        }
    }

    #[test]
    fn a_save_that_loomd_has_not_recorded_is_not_a_success() {
        let outcome = timed_out(&[], "k3q7", &detail());
        assert!(!outcome.recorded);
        assert!(
            outcome.text.contains("not recorded it within"),
            "{outcome:?}"
        );
        // The CLI prints this text on standard output; nothing is added to the warnings.
        assert_eq!(outcome.warnings, Vec::<String>::new());
    }

    #[test]
    fn a_recorded_save_that_changed_again_is_recorded() {
        let record = |actor| {
            let origin = loom_core::Origin {
                actor,
                machine_id: loom_api::MachineId("desk".to_owned()),
            };
            EventRecord::new(
                "aaaaaaaa".parse().unwrap(),
                loom_api::Event::NoteLogged {
                    text: "x".to_owned(),
                },
                loom_api::Timestamp::from_unix_secs(1_791_792_000).unwrap(),
                &origin,
            )
        };
        let appended = [record(Actor::File), record(Actor::User)];
        let outcome = timed_out(&appended, "k3q7", &detail());
        assert!(outcome.recorded);
        assert_eq!(outcome.warnings.len(), 1);
        assert!(outcome.text.contains(&render_change(&detail())));
    }
}
