//! The queue of MRs that wait on me (`queue/mrs/<provider>-<key>.json`): an MR is an entry, not
//! a task, until I start on it. Pure: the poller (`reviews.rs`) calls the forge, the task thread
//! writes the files.
//!
//! - [`decide_entry`]: add, keep or remove the entry of one MR, after one poll.
//! - [`start_target`]: what `loom review start <key>` acts on.
//! - [`can_start`]: whether the MR that the start read can get a Review task.
//! - [`approval_step`]: the head of my GitLab approval of an MR without a task.
//! - [`entry_seen`]: what a poll knows of an MR, for [`decide_entry`].
//! - [`reasons_read`], [`other_reasons`], [`entry_reasons`]: why an MR waits on me.
//! - [`mark_seen`]: what `POST /v1/reviews/queue/{key}/seen` does to an entry.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use loom_api::{ContextName, QueueReason, ReviewQueueEntry, TaskId, Timestamp};
use loom_forge::model::{
    HeadPipeline, MergeStatus, Mr, MrForgeIds, MrKey, MrSignals, MrState, MrVersion,
    PipelineCommit, PipelineOutcome, Sha, Thread,
};
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

/// The entry of `mr`, with `reasons`. A `stored` entry keeps what the poll does not read:
/// `seen_at`. Pure.
#[must_use]
pub fn entry_of(mr: &Mr, stored: Option<&QueueEntry>, reasons: Vec<QueueReason>) -> QueueEntry {
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
    /// The reasons that hold now other than `review_requested`, in the order of [`REASONS`].
    pub other_reasons: Vec<QueueReason>,
    /// The MR has a file with no reason: my own MR, kept for its `seen_at`.
    pub parked: bool,
    /// The file of the MR has a `seen_at`.
    pub has_seen_at: bool,
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
    /// The MR leaves the queue, and its file stays with no reason, for its `seen_at`.
    Park,
    /// Delete a file with no reason. The queue does not change.
    DropParked,
}

/// What a poll does with the entry of an MR. Pure.
///
/// An MR waits on me when it is open, it has no Review task, and a reason holds.
///
/// After an archived Review task, a new entry needs a new head or a new request, against what the
/// mirror of that task saw. The mirror does not hold whether I was assigned or mentioned, so these
/// need a new head; my own MR needs nothing, as a task of it is only an old one.
///
/// A file with no reason keeps the `seen_at` of my own open MR: `index.db` can be rebuilt, and
/// a note that I saw must not count as new again.
#[must_use]
pub fn decide_entry(seen: &EntrySeen) -> EntryDecision {
    let open = !seen.tracked && seen.state == MrState::Open;
    let requested = review_requested_holds(seen);
    let waits = open && (requested || !seen.other_reasons.is_empty());
    let keeps_seen_at = open && seen.own && seen.has_seen_at;
    match (seen.has_entry, waits) {
        (true, true) => EntryDecision::Keep,
        (true, false) if keeps_seen_at => EntryDecision::Park,
        (true, false) => EntryDecision::Remove,
        (false, false) if seen.parked && !keeps_seen_at => EntryDecision::DropParked,
        (false, false) => EntryDecision::Absent,
        (false, true) => {
            let new = seen.parked
                || seen.own
                || seen.archived.as_ref().is_none_or(|archived| {
                    archived.head.as_ref() != Some(&seen.head)
                        || (requested && archived.requested == Some(false))
                });
            if new {
                EntryDecision::Add
            } else {
                EntryDecision::Absent
            }
        }
    }
}

/// I am a requested reviewer of an MR that I did not write, and my verdict is not on its current
/// head or a request is newer than it.
const fn review_requested_holds(seen: &EntrySeen) -> bool {
    seen.requested && (!seen.verdict_on_head || seen.requested_after_verdict) && !seen.own
}

/// Every reason, in the order of the entry file: the same reasons give the same bytes.
pub const REASONS: [QueueReason; 6] = [
    QueueReason::ReviewRequested,
    QueueReason::Assigned,
    QueueReason::Mentioned,
    QueueReason::OwnNewComments,
    QueueReason::OwnPipelineFailed,
    QueueReason::OwnReadyToMerge,
];

/// What one poll read about the reasons other than `review_requested`. `None`: this poll does not
/// know (a 304, a list that failed, or GitHub), and the entry keeps its value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReasonsRead {
    /// I am an assignee of an MR that I did not write.
    pub assigned: Option<bool>,
    /// I have a pending to-do that mentions me.
    pub mentioned: Option<bool>,
    /// The discussions were read: the newest note that counts against `seen_at`, if any.
    /// Compared on the task thread, so that a `seen` during the poll wins.
    pub new_comment: Option<Option<Timestamp>>,
    /// A pipeline failed on the current head of my MR.
    pub pipeline_failed: Option<bool>,
    /// My MR can be merged and has an approval.
    pub ready_to_merge: Option<bool>,
}

impl ReasonsRead {
    /// Nothing known: each reason keeps its value.
    pub const UNKNOWN: Self = Self {
        assigned: None,
        mentioned: None,
        new_comment: None,
        pipeline_failed: None,
        ready_to_merge: None,
    };

    /// The forge no longer shows the MR: no reason holds.
    pub const NONE_HOLD: Self = Self {
        assigned: Some(false),
        mentioned: Some(false),
        new_comment: Some(None),
        pipeline_failed: Some(false),
        ready_to_merge: Some(false),
    };
}

/// What a full read of a GitLab MR tells, for [`reasons_read`].
#[derive(Debug, Clone, Copy)]
pub struct MrFacts<'a> {
    /// The MR.
    pub mr: &'a Mr,
    /// The other fields of its read.
    pub signals: &'a MrSignals,
    /// The login of the token.
    pub login: &'a str,
    /// The discussions, `None` when this read did not need them.
    pub threads: Option<&'a [Thread]>,
    /// How many users approved the MR.
    pub approvals: usize,
    /// Whether a pending to-do mentions me, `None` when that list failed.
    pub mentioned: Option<bool>,
    /// When the forge first had the current head, when the read needed it.
    pub head_since: Option<Timestamp>,
}

/// The reasons that a full read tells. Pure.
#[must_use]
pub fn reasons_read(facts: &MrFacts<'_>) -> ReasonsRead {
    let own = facts.mr.author.eq_ignore_ascii_case(facts.login);
    ReasonsRead {
        assigned: Some(is_assigned(&facts.signals.assignees, facts.login, own)),
        mentioned: facts.mentioned,
        // Not my MR: the `own_` reasons do not hold. Mine: the discussions tell, if read.
        new_comment: if own {
            facts
                .threads
                .map(|threads| new_comment(threads, facts.login))
        } else {
            Some(None)
        },
        pipeline_failed: if own {
            pipeline_failed(
                facts.signals.head_pipeline.as_ref(),
                &facts.mr.version.head_sha,
                facts.head_since,
            )
        } else {
            Some(false)
        },
        ready_to_merge: if own {
            ready_to_merge(facts.signals.merge_status, facts.mr.draft, facts.approvals)
        } else {
            Some(false)
        },
    }
}

/// I am an assignee of an MR that I did not write. My own MRs are often assigned to me, so that
/// case counts only through the `own_` reasons. Pure.
#[must_use]
pub fn is_assigned(assignees: &[String], login: &str, own: bool) -> bool {
    !own && assignees
        .iter()
        .any(|name| name.eq_ignore_ascii_case(login))
}

/// The newest note from someone else that is newer than my latest note. Pure.
#[must_use]
pub fn new_comment(threads: &[Thread], login: &str) -> Option<Timestamp> {
    let comments = || threads.iter().flat_map(|thread| &thread.comments);
    let mine = comments()
        .filter(|comment| comment.author.eq_ignore_ascii_case(login))
        .map(|comment| comment.created_at)
        .max();
    comments()
        .filter(|comment| !comment.author.eq_ignore_ascii_case(login))
        .map(|comment| comment.created_at)
        .max()
        .filter(|newest| mine.is_none_or(|mine| *newest > mine))
}

/// Whether the note found by [`new_comment`] is newer than the time I opened the entry. Pure.
#[must_use]
pub fn has_new_comment(newest: Option<Timestamp>, seen_at: Option<Timestamp>) -> bool {
    newest.is_some_and(|newest| seen_at.is_none_or(|seen| newest > seen))
}

/// A pipeline failed on `head`: a failure on an older head says nothing about this one. Pure.
///
/// A merged results or merge train pipeline runs on a temporary merge commit, which says nothing
/// about the head. `head_since` dates the head: the pipeline is of it unless it was created before
/// that. GitLab keeps `head_pipeline` after a push that creates no pipeline, so without this date
/// the queue would report a failure on a head that never ran. `None`: it cannot be dated, and the
/// reason keeps its value.
#[must_use]
pub fn pipeline_failed(
    pipeline: Option<&HeadPipeline>,
    head: &Sha,
    head_since: Option<Timestamp>,
) -> Option<bool> {
    let Some(pipeline) = pipeline else {
        return Some(false);
    };
    if pipeline.outcome != PipelineOutcome::Failed {
        return Some(false);
    }
    match &pipeline.commit {
        PipelineCommit::Commit(sha) => Some(sha.as_ref() == Some(head)),
        PipelineCommit::MergedResult => Some(pipeline.created_at? >= head_since?),
    }
}

/// Whether [`pipeline_failed`] needs the time of the head: the one case that costs a request. Pure.
#[must_use]
pub fn needs_head_time(pipeline: Option<&HeadPipeline>) -> bool {
    pipeline.is_some_and(|pipeline| {
        pipeline.outcome == PipelineOutcome::Failed
            && pipeline.commit == PipelineCommit::MergedResult
    })
}

/// When the forge first had `head`: the oldest of its versions, since a new base gives a later
/// version of the same head. Pure.
#[must_use]
pub fn head_since(versions: &[MrVersion], head: &Sha) -> Option<Timestamp> {
    versions
        .iter()
        .filter(|version| version.head_sha == *head)
        .filter_map(|version| version.created_at)
        .min()
}

/// My MR can be merged and has an approval, and it is not a draft. `None` while the forge computes
/// the status: the reason does not change, so it does not come and go at each push. Pure.
#[must_use]
pub const fn ready_to_merge(status: MergeStatus, draft: bool, approvals: usize) -> Option<bool> {
    if draft {
        return Some(false);
    }
    match status {
        MergeStatus::Computing => None,
        MergeStatus::Mergeable => Some(approvals > 0),
        MergeStatus::NotMergeable => Some(false),
    }
}

/// The reasons other than `review_requested` after a poll: what it read, and otherwise what the
/// file holds. Pure.
#[must_use]
pub fn other_reasons(
    stored: &[QueueReason],
    read: &ReasonsRead,
    seen_at: Option<Timestamp>,
) -> Vec<QueueReason> {
    REASONS
        .into_iter()
        .filter(|reason| {
            let known = match reason {
                QueueReason::ReviewRequested => return false,
                QueueReason::Assigned => read.assigned,
                QueueReason::Mentioned => read.mentioned,
                QueueReason::OwnNewComments => read
                    .new_comment
                    .map(|newest| has_new_comment(newest, seen_at)),
                QueueReason::OwnPipelineFailed => read.pipeline_failed,
                QueueReason::OwnReadyToMerge => read.ready_to_merge,
            };
            known.unwrap_or_else(|| stored.contains(reason))
        })
        .collect()
}

/// The reasons of the entry file after a poll, in the order of [`REASONS`]. Pure.
#[must_use]
pub fn entry_reasons(seen: &EntrySeen) -> Vec<QueueReason> {
    let requested = review_requested_holds(seen).then_some(QueueReason::ReviewRequested);
    requested
        .into_iter()
        .chain(seen.other_reasons.iter().copied())
        .collect()
}

/// The entry of `target` after the entry `source` of the same MR moves onto it: a file with no
/// reason must not drop the reasons of the entry, and the later `seen_at` wins. Pure.
#[must_use]
pub fn merged_entry(target: &QueueEntry, source: &QueueEntry) -> QueueEntry {
    QueueEntry {
        reasons: REASONS
            .into_iter()
            .filter(|reason| target.reasons.contains(reason) || source.reasons.contains(reason))
            .collect(),
        seen_at: target.seen_at.max(source.seen_at),
        ..target.clone()
    }
}

/// The entry after I open it, at `at`: `own_new_comments` goes, the other reasons stay. Pure.
#[must_use]
pub fn mark_seen(entry: &QueueEntry, at: Timestamp) -> QueueEntry {
    QueueEntry {
        reasons: entry
            .reasons
            .iter()
            .copied()
            .filter(|reason| *reason != QueueReason::OwnNewComments)
            .collect(),
        seen_at: Some(at),
        ..entry.clone()
    }
}

/// A start of an MR whose read shows no reason left: my direct request is gone, and no other
/// reason holds (an assignee, or a to-do that mentions me). Pure.
#[must_use]
pub const fn waits_no_more(was_requested: bool, reviewer: bool, other_reason: bool) -> bool {
    was_requested && !reviewer && !other_reason
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
    /// The read shows that my direct request is gone, and no other reason is left.
    RequestGone,
}

/// Whether the MR that a start read can get a Review task. `request_gone`: [`waits_no_more`]; a
/// read does not show a request through a team. Pure.
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
    /// What this poll read about the other reasons.
    pub reasons: &'a ReasonsRead,
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
    let had_request = input
        .stored
        .map(|entry| entry.reasons.contains(&QueueReason::ReviewRequested));
    // Unknown (an incomplete search): as at the last poll.
    let requested = input
        .requested
        .or(had_request)
        .or_else(|| recorded.map(|state| state.requested))
        .unwrap_or(false);
    // A 304, with no verdict read: an entry that lost this reason to my verdict does not get it
    // back. It can have other reasons now, so a stored entry no longer means "requested".
    let unread = input.read.is_none() && input.verdict.is_none();
    let requested = requested && !(unread && had_request == Some(false));
    let stored_reasons = input
        .stored
        .map_or(&[][..], |entry| entry.reasons.as_slice());
    Some(EntrySeen {
        tracked: input.tracked,
        has_entry: !stored_reasons.is_empty(),
        parked: input.stored.is_some() && stored_reasons.is_empty(),
        has_seen_at: input.stored.is_some_and(|entry| entry.seen_at.is_some()),
        other_reasons: other_reasons(
            stored_reasons,
            input.reasons,
            input.stored.and_then(|entry| entry.seen_at),
        ),
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
