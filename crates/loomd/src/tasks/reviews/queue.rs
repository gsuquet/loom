//! The queue entries of a poll, written on the task thread: the decisions are in
//! `loomd::review_queue`. A file is written only when its bytes change.

use std::fs;
use std::io;
use std::path::Path;

use axum::http::StatusCode;
use loom_api::{
    ApiError, ContextName, ErrorCode, ForgeKind, ReviewQueue, ReviewSeenResult, StatusName, TaskId,
    Timestamp,
};
use loom_forge::model::{Mr, MrForgeIds, MrKey, MrState, Sha};
use loom_store::files::ARCHIVE_DIR;
use loom_store::port::TaskLocation;
use loom_store::{IndexedTask, MrApproval};
use loomd::api::{internal, invalid_request};
use loomd::hooks;
use loomd::review_queue::{
    APPROVAL_EXPIRY, ApprovalStep, EntryDecision, EntryInput, QueueEntry, ReasonsRead, StartTarget,
    active_reviews_of, approval_key, approval_step, decide_entry, entry_bytes, entry_name,
    entry_of, entry_reasons, entry_seen, mark_seen, queue_dir, queue_message, read_entry_bytes,
    start_target,
};
use loomd::review_sync::{MrBatch, MrUpdate, SeenVerdict, find_review, mirror_dir, url_host};
use tracing::{info, warn};

use super::{Previous, ReviewsApplied, Worker, can_create, read_previous, write_mirror};
use crate::repos::Failure;
use crate::reviews::hooked_duration;
use crate::tasks::{log_commit, now};

impl Worker {
    /// The queue entry of one MR, after its Review task: adds, keeps or removes it. A failed
    /// read comes with no update, so it changes no entry.
    pub(super) fn sync_entry(
        &mut self,
        batch: &MrBatch,
        rows: &[IndexedTask],
        update: &MrUpdate,
        applied: &mut ReviewsApplied,
        updated: &mut usize,
    ) -> Result<(), String> {
        let context = &batch.context;
        let root = self
            .stores
            .get(context)
            .ok_or_else(|| format!("no store for context `{context}`"))?
            .clone();
        let review = find_review(rows, context, &batch.host, &update.key);
        // In any context: the MR stays in the context of its task.
        let tracked = update.start
            || active_reviews_of(rows, &update.key)
                .iter()
                .any(|(_, _, host)| host.as_deref() == Some(batch.host.as_str()));
        let stored = read_entry(&root, &update.key);
        // Two forge hosts can have the same key, and their entries the same file name.
        let foreign = stored
            .as_ref()
            .is_some_and(|entry| url_host(&entry.web_url).as_deref() != Some(batch.host.as_str()));
        let stored = stored.filter(|_| !foreign);
        let verdict = self.approval_head(batch, update, tracked)?;
        if let Some(target) = &update.moved_to {
            return self.move_entry(batch, update, stored.as_ref(), target, tracked, applied);
        }
        if tracked && stored.is_none() {
            return Ok(());
        }
        let archived = matches!(review, Some((_, _, TaskLocation::Archived(_))));
        let previous = archived_mirror(&root, &update.key, review.as_ref());
        let read = update.mr.as_ref();
        let known = read.or(previous.mr.as_ref());
        // A 404: as for a closed MR, no reason holds.
        let reasons_read = if update.gone {
            ReasonsRead::NONE_HOLD
        } else {
            update.reasons
        };
        // A 304, and neither an entry nor a mirror: nothing changed.
        let Some(seen) = entry_seen(&EntryInput {
            read,
            stored: stored.as_ref(),
            archived_mr: previous.mr.as_ref(),
            archived_state: previous.state.as_ref(),
            archived,
            verdict: verdict.as_ref(),
            requested: update.requested,
            tracked,
            own: update.own,
            requested_after_verdict: update.requested_after_verdict,
            reasons: &reasons_read,
        }) else {
            return Ok(());
        };
        let reasons = entry_reasons(&seen);
        // Follows the MR when this poll read it, otherwise only the reasons change.
        let write = |mr: Option<&Mr>| -> Result<bool, String> {
            let entry = match (mr, stored.as_ref()) {
                (Some(mr), stored) => {
                    can_create(&root, rows, batch, mr)?;
                    entry_of(mr, stored, reasons.clone())
                }
                (None, Some(stored)) => QueueEntry {
                    reasons: reasons.clone(),
                    ..stored.clone()
                },
                (None, None) => return Err("no read of the MR for its entry".to_owned()),
            };
            write_entry(&root, &entry)
        };
        match decide_entry(&seen) {
            EntryDecision::Absent => {}
            EntryDecision::Add => {
                if foreign {
                    return Err(format!(
                        "the queue has an entry of this key on another forge host than {}: no entry",
                        batch.host
                    ));
                }
                write(known)?;
                info!(%context, key = %update.key, "MR added to the queue");
                applied.added.push(update.key.to_string());
            }
            EntryDecision::Keep => {
                if write(read)? {
                    *updated += 1;
                }
            }
            EntryDecision::Park => {
                write(read)?;
                info!(%context, key = %update.key, "MR removed from the queue, its file keeps seen_at");
                applied.removed.push(update.key.to_string());
            }
            EntryDecision::DropParked => {
                remove_entry(&root, &update.key)?;
                *updated += 1;
            }
            EntryDecision::Remove => {
                remove_entry(&root, &update.key)?;
                info!(%context, key = %update.key, "MR removed from the queue");
                applied.removed.push(update.key.to_string());
                applied.entry_gone |= update.gone;
            }
        }
        Ok(())
    }

    /// The verdict of `update`, with the head that my GitLab approval counts for: the head of the
    /// poll that first showed it, kept in the index (machine-local, no store commit).
    #[expect(
        clippy::option_option,
        reason = "as `MrUpdate::verdict`: not read, no verdict, or a verdict"
    )]
    fn approval_head(
        &mut self,
        batch: &MrBatch,
        update: &MrUpdate,
        tracked: bool,
    ) -> Result<Option<Option<SeenVerdict>>, String> {
        if update.key.forge() != ForgeKind::Gitlab {
            return Ok(update.verdict.clone());
        }
        let cannot = |error: &dyn std::fmt::Display| format!("cannot use the index: {error}");
        let index = self.tasks.index();
        let key = approval_key(&update.key);
        let row = index
            .mr_approval_head(&batch.context, &batch.host, &key)
            .map_err(|error| cannot(&error))?
            .and_then(|head| Sha::try_from(head).ok());
        if update.gone {
            let expiry = hooked_duration(hooks::APPROVAL_EXPIRY_ENV, APPROVAL_EXPIRY);
            let cutoff = now().unix_secs().saturating_sub(expiry.as_secs());
            Timestamp::from_unix_secs(cutoff)
                .map(|cutoff| {
                    index.drop_mr_approval_before(&batch.context, &batch.host, &key, cutoff)
                })
                .transpose()
                .map_err(|error| cannot(&error))?;
        }
        // A 404 is not leaving: a lost access answers it too, and the head of my approval stays.
        let left = (update.requested == Some(false) && !update.gone)
            || update.moved_to.is_some()
            || update
                .mr
                .as_ref()
                .is_some_and(|mr| mr.state != MrState::Open);
        let shown = update.verdict.as_ref().map(Option::as_ref);
        let (step, head) = approval_step(row.as_ref(), tracked, left, shown);
        match step {
            ApprovalStep::Keep => {}
            ApprovalStep::Drop => index
                .drop_mr_approval(&batch.context, &batch.host, &key)
                .map_err(|error| cannot(&error))?,
            ApprovalStep::Record(head) => {
                // A verdict that the poll read comes with the MR, unless the MR has a task.
                if let Some(MrForgeIds::Gitlab { project_id, .. }) =
                    update.mr.as_ref().map(|mr| &mr.forge_ids)
                {
                    let approval = MrApproval {
                        key,
                        head_sha: head.to_string(),
                        project_id: *project_id,
                    };
                    index
                        .set_mr_approval(&batch.context, &batch.host, &approval, now())
                        .map_err(|error| cannot(&error))?;
                }
            }
        }
        Ok(update.verdict.clone().map(|verdict| {
            verdict.map(|verdict| SeenVerdict {
                sha: head.unwrap_or_else(|| verdict.sha.clone()),
                ..verdict
            })
        }))
    }

    /// Moves the entry of an MR of another context to the store of that context, and commits it
    /// there: the poll of that context does not list an unchanged MR. The poller checked that
    /// this machine polls that context with the same account.
    fn move_entry(
        &mut self,
        batch: &MrBatch,
        update: &MrUpdate,
        stored: Option<&QueueEntry>,
        target: &ContextName,
        tracked: bool,
        applied: &mut ReviewsApplied,
    ) -> Result<(), String> {
        let Some(entry) = stored else {
            return Ok(());
        };
        let root = self
            .stores
            .get(&batch.context)
            .ok_or_else(|| format!("no store for context `{}`", batch.context))?
            .clone();
        let target_root = self
            .stores
            .get(target)
            .ok_or_else(|| format!("no store for context `{target}`"))?
            .clone();
        // A task means no entry anywhere; an entry there is already this one, or is of another
        // host, which is reported.
        if !tracked {
            match read_entry(&target_root, &update.key) {
                Some(other) if other.web_url != entry.web_url => {
                    return Err(format!(
                        "the queue of context `{target}` has an entry of this key on another forge host: the entry stays in this context"
                    ));
                }
                Some(_) => {}
                None => {
                    let bytes = entry_bytes(entry).map_err(|error| error.to_string())?;
                    let dir = queue_dir(&target_root);
                    let name = entry_name(&update.key);
                    write_mirror(&dir, &[(name.as_str(), bytes.as_slice())]).map_err(|error| {
                        format!("cannot write the entry {}: {error}", dir.display())
                    })?;
                    let result = self.commits.commit_queue(target, &queue_message(1, 0, 0));
                    log_commit(target, None, result);
                }
            }
        }
        remove_entry(&root, &update.key)?;
        info!(context = %batch.context, %target, key = %update.key, "MR moved to the queue of its context");
        applied.removed.push(update.key.to_string());
        Ok(())
    }

    /// The entries of the queue of `context`, or of every context, sorted. A file with no reason
    /// only keeps a `seen_at`: it is not in the queue.
    pub(in crate::tasks) fn review_queue(&self, context: Option<&ContextName>) -> ReviewQueue {
        let mut entries = Vec::new();
        for (name, root) in &self.stores {
            if context.is_none_or(|context| context == name) {
                entries.extend(
                    read_queue(root)
                        .iter()
                        .filter(|entry| !entry.reasons.is_empty())
                        .map(|entry| entry.to_api(name)),
                );
            }
        }
        ReviewQueue { entries }
    }

    /// I opened the entry of `key`: writes `seen_at` and removes `own_new_comments`, in the file
    /// of the store, which a rebuild of `index.db` does not touch. No forge call.
    pub(in crate::tasks) fn review_seen(
        &mut self,
        context: Option<&ContextName>,
        key: &MrKey,
        polled: &crate::reviews::Polled,
    ) -> Result<ReviewSeenResult, Failure> {
        let found: Vec<(ContextName, QueueEntry)> = self
            .stores
            .iter()
            .filter_map(|(name, root)| Some((name.clone(), read_entry(root, key)?)))
            .collect();
        // An entry wins over a file with no reason, which only keeps a `seen_at`.
        let found = if found.iter().any(|(_, entry)| !entry.reasons.is_empty()) {
            found
                .into_iter()
                .filter(|(_, entry)| !entry.reasons.is_empty())
                .collect()
        } else {
            found
        };
        let hosts: Vec<(ContextName, Option<String>)> = found
            .iter()
            .map(|(name, entry)| (name.clone(), url_host(&entry.web_url)))
            .collect();
        let name = match start_target(context, &[], &hosts) {
            StartTarget::Entry(name) => name,
            StartTarget::Task(_) | StartTarget::NotFound => {
                return Err((
                    StatusCode::NOT_FOUND,
                    ApiError::new(
                        ErrorCode::NotFound,
                        format!("MR `{key}` has no queue entry"),
                    ),
                ));
            }
            StartTarget::Ambiguous(contexts) => {
                let contexts: Vec<&str> = contexts.iter().map(ContextName::as_str).collect();
                return Err((
                    StatusCode::BAD_REQUEST,
                    invalid_request(format!(
                        "MR `{key}` is in several contexts ({}): name one",
                        contexts.join(", ")
                    )),
                ));
            }
        };
        let cannot = |error: String| (StatusCode::INTERNAL_SERVER_ERROR, internal(error));
        let root = self
            .stores
            .get(&name)
            .ok_or_else(|| cannot(format!("no store for context `{name}`")))?;
        let entry = found
            .iter()
            .find(|(other, _)| *other == name)
            .map(|(_, entry)| entry)
            .ok_or_else(|| cannot(format!("no entry of MR `{key}` in context `{name}`")))?;
        // As for a start: this machine must poll what it writes on.
        let host = url_host(&entry.web_url).unwrap_or_default();
        if !polled.contains(&(name.clone(), host.clone())) {
            return Err((
                StatusCode::BAD_REQUEST,
                invalid_request(format!(
                    "this machine does not poll {host} for context `{name}`: open MR `{key}` on the first machine of the context, or add a credential for the host if it was removed"
                )),
            ));
        }
        let at = now();
        let opened = mark_seen(entry, at);
        let wrote = write_entry(root, &opened).map_err(cannot)?;
        if wrote {
            let result = self.commits.commit_queue(&name, &queue_message(0, 1, 0));
            log_commit(&name, None, result);
        }
        Ok(ReviewSeenResult {
            context: name,
            key: key.to_string(),
            seen_at: at,
            reasons: opened.reasons,
        })
    }
}

/// The mirror of the archived Review task of `key`, if `review` is one.
fn archived_mirror(
    root: &Path,
    key: &MrKey,
    review: Option<&(TaskId, Option<StatusName>, TaskLocation)>,
) -> Previous {
    match review {
        Some((id, _, TaskLocation::Archived(year))) => read_previous(&mirror_dir(
            &root
                .join(ARCHIVE_DIR)
                .join(format!("{year:04}"))
                .join(id.as_str()),
            key,
        )),
        Some((_, _, TaskLocation::Active)) | None => Previous::default(),
    }
}

/// Writes `entry` in the store at `root`, when its bytes change. `true` when it wrote.
fn write_entry(root: &Path, entry: &QueueEntry) -> Result<bool, String> {
    let bytes = entry_bytes(entry).map_err(|error| error.to_string())?;
    let name = entry_name(&entry.key);
    let dir = queue_dir(root);
    write_mirror(&dir, &[(name.as_str(), bytes.as_slice())])
        .map_err(|error| format!("cannot write the entry {}: {error}", dir.display()))
}

/// Deletes the entry of `key` in the store at `root`; already gone is not a failure.
fn remove_entry(root: &Path, key: &MrKey) -> Result<(), String> {
    let name = entry_name(key);
    match fs::remove_file(queue_dir(root).join(&name)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot delete the entry {name}: {error}")),
    }
}

/// The entry of `key` in the store at `root`; what cannot be read is missing.
#[must_use]
pub fn read_entry(root: &Path, key: &MrKey) -> Option<QueueEntry> {
    read_entry_bytes(&fs::read(queue_dir(root).join(entry_name(key))).ok()?)
}

/// The entries of the store at `root`, sorted by key. A file that cannot be read is logged and
/// left out.
#[must_use]
pub fn read_queue(root: &Path) -> Vec<QueueEntry> {
    let dir = queue_dir(root);
    let Ok(files) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut entries: Vec<QueueEntry> = files
        .flatten()
        .filter(|file| file.file_name().to_string_lossy().ends_with(".json"))
        .filter_map(|file| {
            let entry = fs::read(file.path())
                .ok()
                .and_then(|bytes| read_entry_bytes(&bytes));
            if entry.is_none() {
                warn!(path = %file.path().display(), "cannot read a queue entry, left out");
            }
            entry
        })
        .collect();
    entries.sort_by_key(|entry| entry.key.to_string());
    entries
}
