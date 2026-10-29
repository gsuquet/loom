//! The Review tasks and MR mirrors of a poll, written on the task thread (ADR-0016, decisions 1
//! and 2): the decisions are in `loomd::review_sync`.
//!
//! A mirror file is written (atomically) only when its bytes change, and a task whose status
//! or mirror changed is committed once: a poll that sees no change writes and commits nothing.

use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

use loom_api::{ReviewSyncFailure, Status, StatusName, TaskId};
use loom_core::{Actor, Origin};
use loom_forge::model::{Mr, MrKey};
use loom_store::IndexedTask;
use loom_store::atomic::write_file;
use loom_store::files::{TASK_DIR_MODE, TASK_FILE_MODE, TASKS_DIR};
use loom_store::port::TaskLocation;
use loomd::backup::commit_message;
use loomd::review_sync::{
    DESCRIPTION_FILE, Decision, MR_FILE, MrBatch, MrUpdate, Observation, SYNC_FILE, SyncState,
    VERSIONS_FILE, VersionRecord, decide, find_review, merge_versions, mirror, mirror_dir,
    new_review_task, next_verdict, read_mr,
};
use tracing::{info, info_span, warn};

use super::{Worker, log_commit, now};

/// What the task thread did with a batch.
#[derive(Debug, Default)]
pub struct ReviewsApplied {
    /// The Review tasks created.
    pub created: Vec<TaskId>,
    /// The Review tasks whose status or mirror changed.
    pub changed: Vec<TaskId>,
    /// The MRs whose task or mirror could not be written.
    pub failed: Vec<ReviewSyncFailure>,
}

/// The mirror of an MR as the last poll left it.
#[derive(Default)]
struct Previous {
    mr: Option<Mr>,
    versions: Vec<VersionRecord>,
    state: Option<SyncState>,
}

impl Worker {
    /// Applies the poll `batch`: creates, reopens and closes Review tasks, and writes their MR
    /// mirrors. Events are of the actor `daemon`.
    pub(super) fn sync_reviews(&mut self, batch: &MrBatch) -> ReviewsApplied {
        let _span =
            info_span!("tasks.reviews", context = %batch.context, host = %batch.host).entered();
        let mut applied = ReviewsApplied::default();
        let rows = match self.tasks.index().tasks() {
            Ok(rows) => rows,
            Err(error) => {
                warn!(context = %batch.context, %error, "cannot read the tasks, MRs not synced");
                applied.failed = batch
                    .updates
                    .iter()
                    .map(|update| ReviewSyncFailure {
                        key: update.key.to_string(),
                        error: format!("cannot read the tasks: {error}"),
                    })
                    .collect();
                return applied;
            }
        };
        for update in &batch.updates {
            match self.sync_review(batch, &rows, update) {
                Ok(Some((id, created))) => {
                    if created {
                        applied.created.push(id);
                    } else {
                        applied.changed.push(id);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    warn!(context = %batch.context, key = %update.key, %error, "cannot sync a Review task");
                    applied.failed.push(ReviewSyncFailure {
                        key: update.key.to_string(),
                        error,
                    });
                }
            }
        }
        applied
    }

    /// The task of one MR: `Some((id, created))` when it was created or changed.
    fn sync_review(
        &mut self,
        batch: &MrBatch,
        rows: &[IndexedTask],
        update: &MrUpdate,
    ) -> Result<Option<(TaskId, bool)>, String> {
        let context = &batch.context;
        let root = self
            .stores
            .get(context)
            .ok_or_else(|| format!("no store for context `{context}`"))?
            .clone();
        let found = find_review(rows, context, &batch.host, &update.key);
        let previous = match &found {
            Some((id, _)) => read_previous(&mirror_dir(&task_dir(&root, id), &update.key)),
            None => Previous::default(),
        };
        let Some(mr) = update.mr.clone().or_else(|| previous.mr.clone()) else {
            // A 304 without a mirror: nothing to compare with until the next full read.
            return match found {
                Some(_) => Err(
                    "the mirror cannot be read: the MR is read in full at the next poll".to_owned(),
                ),
                None => Ok(None),
            };
        };
        let (observation, state) = observe(&mr, &previous, update);
        let decision = decide(found.as_ref().map(|(_, status)| *status), &observation);
        let daemon = Origin {
            actor: Actor::Daemon,
            machine_id: self.user.machine_id.clone(),
        };
        let (id, created, before, decision) = match (decision, found) {
            // `decide` creates when there is no task.
            (Decision::Ignore, _) | (Decision::Keep | Decision::SetStatus { .. }, None) => {
                return Ok(None);
            }
            (Decision::Create, _) => {
                let task = self
                    .tasks
                    .create(
                        new_review_task(&mr, context),
                        now(),
                        &daemon,
                        &mut self.random,
                    )
                    .map_err(|error| error.to_string())?;
                info!(%context, key = %mr.key, id = %task.id, "Review task created");
                (task.id, true, 0, Decision::Keep)
            }
            (decision @ (Decision::Keep | Decision::SetStatus { .. }), Some((id, _))) => {
                let before = self.event_count(&id);
                (id, false, before, decision)
            }
        };
        let dir = mirror_dir(&task_dir(&root, &id), &update.key);
        // The events written are committed even when a later step fails.
        let written = (|| {
            if let Decision::SetStatus { status, reason } = &decision {
                self.set_review_status(&id, *status, &reason.note(&update.key), &daemon)?;
                info!(%context, key = %update.key, %id, %status, ?reason, "Review task status changed");
            }
            let versions = merge_versions(&previous.versions, &update.versions, now());
            let files = mirror(&mr, &versions, update.threads.as_deref(), &state)
                .map_err(|error| error.to_string())?;
            write_mirror(&dir, &files.files())
                .map_err(|error| format!("cannot write the mirror {}: {error}", dir.display()))
        })();
        let events = self
            .tasks
            .events(&id)
            .map(|log| log.events)
            .unwrap_or_default();
        let new: Vec<&loom_core::Event> = events
            .iter()
            .skip(before)
            .map(|record| &record.event)
            .collect();
        let wrote = *written.as_ref().unwrap_or(&false);
        if !new.is_empty() || wrote {
            let message =
                commit_message(&id, &new).unwrap_or_else(|| mirror_message(&id, &update.key));
            let result = self
                .commits
                .commit_task(context, &id, TaskLocation::Active, &message);
            log_commit(context, Some(&id), result);
            self.changed(context, &id);
        }
        written?;
        Ok((!new.is_empty() || wrote).then_some((id, created)))
    }

    /// Logs `note` on task `id`, then sets its status.
    fn set_review_status(
        &mut self,
        id: &TaskId,
        status: StatusName,
        note: &str,
        daemon: &Origin,
    ) -> Result<(), String> {
        let status = match status {
            StatusName::Ready => Status::Ready,
            StatusName::Done => Status::Done,
            StatusName::Cancelled => Status::Cancelled,
            // `decide` sets no other status.
            StatusName::Inbox | StatusName::Active | StatusName::Waiting => {
                return Err(format!("unexpected status {status}"));
            }
        };
        for command in [
            loom_core::Command::LogNote {
                text: note.to_owned(),
            },
            loom_core::Command::SetStatus { status },
        ] {
            self.tasks
                .apply(id, command, now(), daemon, &mut self.random)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

/// What the poll saw of `mr` against the `previous` mirror, and the sync state to record.
fn observe(mr: &Mr, previous: &Previous, update: &MrUpdate) -> (Observation, SyncState) {
    let state = previous.state.as_ref();
    let recorded = state.and_then(|state| state.verdict.as_ref());
    let shown = state.is_some_and(|state| state.shown);
    let (verdict, new_verdict, shown) = update.verdict.clone().map_or_else(
        || (recorded.cloned(), false, shown),
        |latest| {
            let now_shown = latest.is_some();
            let (verdict, new) = next_verdict(recorded, shown, latest);
            (verdict, new, now_shown)
        },
    );
    let was_requested = state.map(|state| state.requested);
    // Unknown (an incomplete search): as at the last poll.
    let requested = update.requested.or(was_requested).unwrap_or(false);
    let observation = Observation {
        state: mr.state,
        head: mr.version.head_sha.clone(),
        previous_head: previous.mr.as_ref().map(|mr| mr.version.head_sha.clone()),
        previous_state: previous.mr.as_ref().map(|mr| mr.state),
        requested,
        was_requested,
        verdict: verdict.as_ref().map(|verdict| verdict.sha.clone()),
        new_verdict: new_verdict
            && verdict.as_ref().map(|verdict| &verdict.sha) == Some(&mr.version.head_sha),
    };
    let state = SyncState {
        v: SyncState::VERSION,
        requested,
        verdict,
        shown,
    };
    (observation, state)
}

/// `mr(<id>): <key> mirror updated`: the message of a commit of a mirror alone.
fn mirror_message(id: &TaskId, key: &MrKey) -> String {
    format!("mr({id}): {key} mirror updated")
}

fn task_dir(root: &Path, id: &TaskId) -> PathBuf {
    root.join(TASKS_DIR).join(id.as_str())
}

/// The mirror in `dir`; what cannot be read is missing, and the next full read replaces it.
fn read_previous(dir: &Path) -> Previous {
    let read = |name: &str| fs::read(dir.join(name)).ok();
    let description = read(DESCRIPTION_FILE)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default();
    Previous {
        mr: read(MR_FILE).and_then(|bytes| read_mr(&bytes, &description)),
        versions: read(VERSIONS_FILE)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default(),
        state: read(SYNC_FILE).and_then(|bytes| serde_json::from_slice(&bytes).ok()),
    }
}

/// Writes each of `files` in `dir` whose bytes differ, atomically. `true` when one was written.
fn write_mirror(dir: &Path, files: &[(&str, &[u8])]) -> io::Result<bool> {
    let mut wrote = false;
    for (name, bytes) in files {
        let path = dir.join(name);
        if fs::read(&path).is_ok_and(|current| current == *bytes) {
            continue;
        }
        DirBuilder::new()
            .recursive(true)
            .mode(TASK_DIR_MODE)
            .create(dir)?;
        write_file(&path, bytes, TASK_FILE_MODE)?;
        wrote = true;
    }
    Ok(wrote)
}
