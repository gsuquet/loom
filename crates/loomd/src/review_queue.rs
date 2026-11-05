//! The queue of MRs that wait on me (`queue/mrs/<provider>-<key>.json`): an MR is an entry, not
//! a task, until I start on it. Pure: the poller (`reviews.rs`) calls the forge, the task thread
//! writes the files.
//!
//! - [`decide_entry`]: add, keep or remove the entry of one MR, after one poll.
//! - [`start_target`]: what `loom review start <key>` acts on.
//! - [`can_start`]: whether the MR that the start read can get a Review task.
//! - [`approval_step`]: the head of my GitLab approval of an MR without a task.
//! - [`entry_seen`]: what a poll knows of an MR, for [`decide_entry`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use loom_api::{ContextName, QueueReason, ReviewQueueEntry, TaskId, Timestamp};
use loom_forge::model::{Mr, MrForgeIds, MrKey, MrState, Sha};
use loom_store::IndexedTask;
use loom_store::files::QUEUE_DIR;
use loom_store::port::TaskLocation;
use loom_store::tasks::IndexedState;
use serde::{Deserialize, Serialize};

use crate::review_sync::{SeenVerdict, SyncState, next_verdict, review_key};

/// The directory of the MR entries in the queue directory of a store.
pub const QUEUE_MRS_DIR: &str = "mrs";

/// The entry of an MR (`queue/mrs/<provider>-<key>.json`, daemon-owned).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueEntry {
    /// Version of the file format, [`QueueEntry::VERSION`].
    pub v: u32,
    /// The key of the MR.
    pub key: MrKey,
    /// The identifiers of the forge: the poll of a GitLab entry that the list does not show
    /// needs the project ID.
    pub forge_ids: MrForgeIds,
    /// The title.
    pub title: String,
    /// The login of the author.
    pub author: String,
    /// The page of the MR on the forge: its host is the forge host of the entry.
    pub web_url: String,
    /// The head SHA at the last read of the MR.
    pub head_sha: Sha,
    /// The time of the last change of the MR on the forge.
    pub updated_at: Timestamp,
    /// Why the MR waits on me.
    pub reasons: Vec<QueueReason>,
    /// When I last opened the entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_at: Option<Timestamp>,
}

impl QueueEntry {
    /// The version of the format.
    pub const VERSION: u32 = 1;

    /// The row of the API, for the store of `context`.
    #[must_use]
    pub fn to_api(&self, context: &ContextName) -> ReviewQueueEntry {
        ReviewQueueEntry {
            context: context.clone(),
            key: self.key.to_string(),
            title: self.title.clone(),
            author: self.author.clone(),
            web_url: self.web_url.clone(),
            head_sha: self.head_sha.to_string(),
            updated_at: self.updated_at,
            reasons: self.reasons.clone(),
            seen_at: self.seen_at,
        }
    }
}

/// The directory of the MR entries of the store at `root`.
#[must_use]
pub fn queue_dir(root: &Path) -> PathBuf {
    root.join(QUEUE_DIR).join(QUEUE_MRS_DIR)
}

/// The name of the entry file of `key`: `<provider>-<key>.json`, as the mirror directory.
#[must_use]
pub fn entry_name(key: &MrKey) -> String {
    format!("{}.json", key.dir_name())
}

/// The entry of `mr`, for the `review_requested` reason. A `stored` entry keeps what the poll
/// does not read: `seen_at`, and its other reasons. Pure.
#[must_use]
pub fn entry_of(mr: &Mr, stored: Option<&QueueEntry>) -> QueueEntry {
    let mut reasons = stored.map_or_else(Vec::new, |stored| stored.reasons.clone());
    if !reasons.contains(&QueueReason::ReviewRequested) {
        reasons.push(QueueReason::ReviewRequested);
    }
    QueueEntry {
        v: QueueEntry::VERSION,
        key: mr.key.clone(),
        forge_ids: mr.forge_ids.clone(),
        title: mr.title.clone(),
        author: mr.author.clone(),
        web_url: mr.web_url.clone(),
        head_sha: mr.version.head_sha.clone(),
        updated_at: mr.updated_at,
        reasons,
        seen_at: stored.and_then(|stored| stored.seen_at),
    }
}

/// The bytes of the entry file: the same entry gives the same bytes, so an unchanged MR writes
/// nothing.
///
/// # Errors
///
/// The serialization error, which these types never give.
pub fn entry_bytes(entry: &QueueEntry) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = serde_json::to_vec_pretty(entry)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// The entry of the bytes of an entry file. `None` when it cannot be read. Pure.
#[must_use]
pub fn read_entry_bytes(bytes: &[u8]) -> Option<QueueEntry> {
    serde_json::from_slice(bytes)
        .ok()
        .filter(|entry: &QueueEntry| entry.v == QueueEntry::VERSION)
}

/// What the mirror of an archived Review task of the MR tells: what the last poll saw before
/// the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedSeen {
    /// The head SHA of the mirror, `None` when it cannot be read.
    pub head: Option<Sha>,
    /// I was a requested reviewer at that poll, `None` without a `sync.json`.
    pub requested: Option<bool>,
}

/// What a poll knows of one MR, for its entry.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the facts of a decision table: each row names the ones it sets"
)]
pub struct EntrySeen {
    /// The MR has a Review task that is not archived, in any status.
    pub tracked: bool,
    /// The MR has an entry.
    pub has_entry: bool,
    /// The state of the MR.
    pub state: MrState,
    /// The head SHA now.
    pub head: Sha,
    /// I am a requested reviewer now.
    pub requested: bool,
    /// My last verdict is on the current head.
    pub verdict_on_head: bool,
    /// A request is known to be newer than my verdict: GitHub removes me from the reviewers when
    /// I submit, so being listed is newer; GitLab: my reviewer state is `unreviewed`.
    pub requested_after_verdict: bool,
    /// I am the author of the MR.
    pub own: bool,
    /// The last archived Review task of the MR.
    pub archived: Option<ArchivedSeen>,
}

/// What to do with the entry of an MR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryDecision {
    /// No entry, and none to write.
    Absent,
    /// Write a new entry.
    Add,
    /// The entry stays; its fields follow the MR.
    Keep,
    /// Delete the entry. No event: an entry is not a task.
    Remove,
}

/// What a poll does with the entry of an MR. Pure.
///
/// An MR waits on me when it is open, I am a requested reviewer, my verdict is not on its
/// current head or a request is newer than it, I am not its author, and it has no Review task.
/// After an archived Review task,
/// a new entry needs a new head or a new request, against what the mirror of that task saw.
#[must_use]
pub fn decide_entry(seen: &EntrySeen) -> EntryDecision {
    let waits = !seen.tracked
        && seen.state == MrState::Open
        && seen.requested
        && (!seen.verdict_on_head || seen.requested_after_verdict)
        && !seen.own;
    match (seen.has_entry, waits) {
        (true, true) => EntryDecision::Keep,
        (true, false) => EntryDecision::Remove,
        (false, false) => EntryDecision::Absent,
        (false, true) => {
            let new = seen.archived.as_ref().is_none_or(|archived| {
                archived.head.as_ref() != Some(&seen.head) || archived.requested == Some(false)
            });
            if new {
                EntryDecision::Add
            } else {
                EntryDecision::Absent
            }
        }
    }
}

/// The context and the task or entry that a start names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartTarget {
    /// The MR has a Review task: a start answers with it and creates nothing.
    Task(TaskId),
    /// The MR has an entry in this context, and no Review task in any context.
    Entry(ContextName),
    /// The MR has no task and no entry.
    NotFound,
    /// Two contexts have this key, with an entry or with a task: the request must name one.
    Ambiguous(Vec<ContextName>),
}

/// What a start of an MR acts on. Pure.
///
/// `tasks` are the Review tasks of the MR that are not archived, with their context and forge
/// host; `entries` are its entries, with their context and forge host; `context` is the context
/// that the request names. A task wins over an entry, in any context: a task stays in its context
/// and an MR never gets a second one. A task of another forge host with this key is not of this
/// MR.
#[must_use]
pub fn start_target(
    context: Option<&ContextName>,
    tasks: &[(ContextName, TaskId, Option<String>)],
    entries: &[(ContextName, Option<String>)],
) -> StartTarget {
    let named = |candidate: &ContextName| context.is_none_or(|context| context == candidate);
    let entries: Vec<&(ContextName, Option<String>)> =
        entries.iter().filter(|(other, _)| named(other)).collect();
    let tasks: Vec<&(ContextName, TaskId, Option<String>)> = tasks
        .iter()
        .filter(|(_, _, host)| {
            entries.is_empty() || entries.iter().any(|(_, entry_host)| entry_host == host)
        })
        .collect();
    let contexts = |contexts: Vec<&ContextName>| {
        StartTarget::Ambiguous(
            contexts
                .into_iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        )
    };
    if let Some((_, id, _)) = tasks.iter().find(|(other, _, _)| context == Some(other)) {
        return StartTarget::Task(id.clone());
    }
    match (tasks.as_slice(), entries.as_slice()) {
        ([(_, id, _)], _) => StartTarget::Task(id.clone()),
        ([], []) => StartTarget::NotFound,
        ([], [(name, _)]) => StartTarget::Entry(name.clone()),
        ([], many) => contexts(many.iter().map(|(name, _)| name).collect()),
        (many, _) => contexts(many.iter().map(|(name, _, _)| name).collect()),
    }
}

/// Why a start refuses the MR that it read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartRefusal {
    /// The MR is merged or closed.
    NotOpen(MrState),
    /// I am the author: my own MRs never get a Review task.
    Own,
    /// The read shows that my direct request is gone.
    RequestGone,
}

/// Whether the MR that a start read can get a Review task. `request_gone`: the read shows that a
/// direct request is gone; a read does not show a request through a team. Pure.
///
/// # Errors
///
/// The reason of the refusal.
pub const fn can_start(state: MrState, own: bool, request_gone: bool) -> Result<(), StartRefusal> {
    match state {
        MrState::Merged | MrState::Closed => Err(StartRefusal::NotOpen(state)),
        MrState::Open if own => Err(StartRefusal::Own),
        MrState::Open if request_gone => Err(StartRefusal::RequestGone),
        MrState::Open => Ok(()),
    }
}

/// What to do with the head of my GitLab approval of an MR without a Review task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalStep {
    /// Nothing to write.
    Keep,
    /// Record this head: this poll first shows the approval.
    Record(Sha),
    /// Forget the head: no approval any more, or the MR left.
    Drop,
}

/// The step to write, and the head that my approval counts for. Pure.
///
/// `row`: the recorded head. `shown`: the verdict of the poll (`None`: not read; `Some(None)`: no
/// approval). `left`: the MR is merged or closed, or I am no longer requested. `tracked`: it has a
/// Review task, whose `sync.json` takes over.
#[must_use]
pub fn approval_step(
    row: Option<&Sha>,
    tracked: bool,
    left: bool,
    shown: Option<Option<&SeenVerdict>>,
) -> (ApprovalStep, Option<Sha>) {
    let forget = |row: Option<&Sha>| {
        if row.is_some() {
            ApprovalStep::Drop
        } else {
            ApprovalStep::Keep
        }
    };
    if tracked || left {
        return (forget(row), None);
    }
    match (shown, row) {
        (None, _) => (ApprovalStep::Keep, row.cloned()),
        (Some(None), _) => (forget(row), None),
        (Some(Some(_)), Some(row)) => (ApprovalStep::Keep, Some(row.clone())),
        (Some(Some(verdict)), None) => (
            ApprovalStep::Record(verdict.sha.clone()),
            Some(verdict.sha.clone()),
        ),
    }
}

/// What a poll holds of an MR, for [`entry_seen`].
#[derive(Debug, Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the facts of a decision table: each row names the ones it sets"
)]
pub struct EntryInput<'a> {
    /// The MR, when this poll read it in full.
    pub read: Option<&'a Mr>,
    /// The entry of the MR.
    pub stored: Option<&'a QueueEntry>,
    /// The MR in the mirror of its archived Review task.
    pub archived_mr: Option<&'a Mr>,
    /// The sync state in the mirror of its archived Review task.
    pub archived_state: Option<&'a SyncState>,
    /// The last Review task of the MR is archived.
    pub archived: bool,
    /// My verdict as the poll shows it, `None` when it did not read it.
    pub verdict: Option<&'a Option<SeenVerdict>>,
    /// I am a requested reviewer, `None` when the poll cannot tell (an incomplete search).
    pub requested: Option<bool>,
    /// The MR has a Review task that is not archived, in any context.
    pub tracked: bool,
    /// I am the author.
    pub own: bool,
    /// A request is known to be newer than my verdict.
    pub requested_after_verdict: bool,
}

/// The facts of [`decide_entry`]: the state and head come from the read, else from the entry,
/// else from the mirror of an archived task. `None` without any of them: nothing changed. Pure.
#[must_use]
pub fn entry_seen(input: &EntryInput<'_>) -> Option<EntrySeen> {
    let of_mr = |mr: &Mr| (mr.state, mr.version.head_sha.clone());
    let (state, head) = input
        .read
        .map(of_mr)
        // An entry is of an MR that was open at its last read.
        .or_else(|| {
            input
                .stored
                .map(|entry| (MrState::Open, entry.head_sha.clone()))
        })
        .or_else(|| input.archived_mr.map(of_mr))?;
    let recorded = input.archived_state;
    let verdict = input.verdict.map_or_else(
        || recorded.and_then(|state| state.verdict.clone()),
        |latest| {
            let shown = recorded.is_some_and(|state| state.shown);
            let known = recorded.and_then(|state| state.verdict.as_ref());
            next_verdict(known, shown, latest.clone()).0
        },
    );
    // Unknown (an incomplete search): as at the last poll.
    let requested = input
        .requested
        .or_else(|| input.stored.map(|_| true))
        .or_else(|| recorded.map(|state| state.requested))
        .unwrap_or(false);
    Some(EntrySeen {
        tracked: input.tracked,
        has_entry: input.stored.is_some(),
        state,
        verdict_on_head: verdict.is_some_and(|verdict| verdict.sha == head),
        requested_after_verdict: input.requested_after_verdict,
        head,
        requested,
        own: input.own,
        archived: input.archived.then(|| ArchivedSeen {
            head: input.archived_mr.map(|mr| mr.version.head_sha.clone()),
            requested: recorded.map(|state| state.requested),
        }),
    })
}

/// The key of an MR in the approvals of the index: MR keys ignore the ASCII case of the project.
#[must_use]
pub fn approval_key(key: &MrKey) -> String {
    key.to_string().to_ascii_lowercase()
}

/// The Review tasks of `key` that are not archived, in every context, with their context and
/// the host of their link. Pure.
#[must_use]
pub fn active_reviews_of(
    rows: &[IndexedTask],
    key: &MrKey,
) -> Vec<(ContextName, TaskId, Option<String>)> {
    rows.iter()
        .filter(|row| row.location == TaskLocation::Active)
        .filter_map(|row| match &row.state {
            IndexedState::Valid { task, .. } => {
                let (found, host) = review_key(&task.kind, &task.links)?;
                (found == *key).then(|| (row.context.clone(), row.id.clone(), host))
            }
            IndexedState::Invalid { .. } => None,
        })
        .collect()
}

/// A 404 drops an approval row older than this: the MR may be deleted for good.
pub const APPROVAL_EXPIRY: Duration = Duration::from_hours(30 * 24);

/// The message of the commit of the queue after one poll. Pure.
#[must_use]
pub fn queue_message(added: usize, updated: usize, removed: usize) -> String {
    format!("queue: {added} added, {updated} updated, {removed} removed")
}

#[cfg(test)]
mod tests;
