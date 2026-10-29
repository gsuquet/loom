//! MR review sync (ADR-0016, decisions 1, 2 and 8): what one poll of a forge does to a Review
//! task and to its MR mirror. Pure: the poller (`reviews.rs`) calls the forge, the task thread
//! writes the files.
//!
//! - [`decide`]: create, reopen, close, cancel or leave the Review task of one MR.
//! - [`SyncState`]: what loomd remembers of an MR between polls (`sync.json`): whether I am a
//!   requested reviewer, and my last verdict.
//! - [`Mirror`]: the bytes of the daemon-owned mirror files; a file is written only when its
//!   bytes change, so a poll that sees no change writes nothing.

use std::path::Path;
use std::time::Duration;

use loom_api::{ContextName, ForgeKind, RepoSummary, TaskId, Timestamp};
use loom_core::{ExternalRef, Kind, Link, NewTask, Relations, Status, StatusName};
use loom_forge::github::{Review, ReviewState};
use loom_forge::model::{Mr, MrKey, MrState, MrVersion, Sha, Thread};
use loom_store::IndexedTask;
use loom_store::port::TaskLocation;
use loom_store::tasks::IndexedState;
use serde::{Deserialize, Serialize};

use crate::schedule::{backoff, jittered};

/// The directory of the MR mirrors in a task directory.
pub const MRS_DIR: &str = "mrs";
/// The snapshot of the MR, without its description.
pub const MR_FILE: &str = "mr.json";
/// The description of the MR, read-only.
pub const DESCRIPTION_FILE: &str = "description.md";
/// Every version seen.
pub const VERSIONS_FILE: &str = "versions.json";
/// The threads and general comments.
pub const THREADS_FILE: &str = "threads.json";
/// What loomd remembers between polls.
pub const SYNC_FILE: &str = "sync.json";

/// Threads are pulled in full at least this often, even when the MR did not change: a
/// resolution may not change `updated_at`.
pub const THREADS_REFRESH: Duration = Duration::from_hours(1);

/// The provider of the `origin` link of a Review task: `github` or `gitlab`.
#[must_use]
pub const fn provider(forge: ForgeKind) -> &'static str {
    match forge {
        ForgeKind::Github => "github",
        ForgeKind::Gitlab => "gitlab",
    }
}

/// My last verdict on an MR, as the forge shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeenVerdict {
    /// The head SHA that the verdict is on.
    pub sha: Sha,
    /// What tells two verdicts apart: the review node ID (GitHub), `approved` (GitLab).
    pub id: String,
}

/// What loomd remembers of an MR between polls (`sync.json`, daemon-owned).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncState {
    /// Version of the file format, [`SyncState::VERSION`].
    pub v: u32,
    /// I was a requested reviewer at the last poll.
    pub requested: bool,
    /// My last verdict seen on the forge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<SeenVerdict>,
}

impl SyncState {
    /// The version of the format.
    pub const VERSION: u32 = 1;
}

/// What a poll saw of one MR, against what the mirror holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// The state of the MR.
    pub state: MrState,
    /// The head SHA now.
    pub head: Sha,
    /// The head SHA of the mirror, `None` without a mirror.
    pub previous_head: Option<Sha>,
    /// I am a requested reviewer now.
    pub requested: bool,
    /// I was one at the last poll; `None` without a `sync.json`.
    pub was_requested: Option<bool>,
    /// The head SHA of my last verdict, the new one included.
    pub verdict: Option<Sha>,
    /// The poll saw a verdict of mine that the last one did not, on the current head.
    pub new_verdict: bool,
}

/// Why the status of a Review task changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// A new head after the task was done.
    NewHead {
        /// The head of the mirror.
        from: Sha,
        /// The new head.
        to: Sha,
    },
    /// I am a requested reviewer again (a GitHub re-request).
    RequestedAgain,
    /// My verdict on the current head.
    Verdict,
    /// The MR is merged.
    Merged,
    /// The MR is closed without a merge.
    Closed,
    /// My review request was removed.
    RequestRemoved,
}

impl Reason {
    /// The text of the note that the change logs on the task.
    #[must_use]
    pub fn note(&self, key: &MrKey) -> String {
        match self {
            Self::NewHead { from, to } => format!(
                "MR {key}: new head {} after {}, reopened",
                short(to),
                short(from)
            ),
            Self::RequestedAgain => format!("MR {key}: my review is requested again, reopened"),
            Self::Verdict => format!("MR {key}: my verdict is posted on the current head"),
            Self::Merged => format!("MR {key}: merged"),
            Self::Closed => format!("MR {key}: closed without a merge"),
            Self::RequestRemoved => format!("MR {key}: my review request was removed"),
        }
    }
}

fn short(sha: &Sha) -> &str {
    sha.as_str().get(..7).unwrap_or_else(|| sha.as_str())
}

/// What to do with the Review task of an MR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// No task, and none to create: the mirror is not written.
    Ignore,
    /// Create the task, in `inbox`.
    Create,
    /// Keep the status; the mirror is updated.
    Keep,
    /// Set the status, for `reason`; the mirror is updated.
    SetStatus {
        /// The new status: `ready`, `done` or `cancelled`.
        status: StatusName,
        /// Why.
        reason: Reason,
    },
}

/// What a poll does with the Review task of an MR, whose status is `task` (`None`: no task;
/// `Some(None)`: a status unknown to this version of loom). Pure.
///
/// - No task: one is created for an open MR where I am a requested reviewer.
/// - Merged: `done`; closed: `cancelled` (a terminal remote state closes the local task).
/// - Open task: `done` on a new verdict of mine on the current head; my request removed:
///   `done` when I posted a verdict, otherwise `cancelled`.
/// - Done task: a new head reopens it (`ready`), even when the forge removed me from the
///   reviewers after my review; so does a new request.
/// - Cancelled task: a new request reopens it.
///
/// A request is new, or removed, only against the last poll: a task that I close or reopen by
/// hand stays as I set it until the MR changes.
#[must_use]
pub fn decide(task: Option<Option<StatusName>>, seen: &Observation) -> Decision {
    let Some(status) = task else {
        return if seen.state == MrState::Open && seen.requested {
            Decision::Create
        } else {
            Decision::Ignore
        };
    };
    // Written by a newer loom: read-only.
    let Some(status) = status else {
        return Decision::Keep;
    };
    let set = |status, reason| Decision::SetStatus { status, reason };
    let open = matches!(
        status,
        StatusName::Inbox | StatusName::Ready | StatusName::Active | StatusName::Waiting
    );
    match seen.state {
        MrState::Merged if open => return set(StatusName::Done, Reason::Merged),
        MrState::Closed if open => return set(StatusName::Cancelled, Reason::Closed),
        MrState::Merged | MrState::Closed => return Decision::Keep,
        MrState::Open => {}
    }
    let requested_again = seen.requested && seen.was_requested == Some(false);
    let request_removed = !seen.requested && seen.was_requested == Some(true);
    match status {
        StatusName::Inbox | StatusName::Ready | StatusName::Active | StatusName::Waiting => {
            if seen.new_verdict {
                set(StatusName::Done, Reason::Verdict)
            } else if request_removed {
                let status = if seen.verdict.is_some() {
                    StatusName::Done
                } else {
                    StatusName::Cancelled
                };
                set(status, Reason::RequestRemoved)
            } else {
                Decision::Keep
            }
        }
        StatusName::Done => match &seen.previous_head {
            Some(from) if *from != seen.head => set(
                StatusName::Ready,
                Reason::NewHead {
                    from: from.clone(),
                    to: seen.head.clone(),
                },
            ),
            Some(_) | None if requested_again => set(StatusName::Ready, Reason::RequestedAgain),
            Some(_) | None => Decision::Keep,
        },
        StatusName::Cancelled if requested_again => set(StatusName::Ready, Reason::RequestedAgain),
        StatusName::Cancelled => Decision::Keep,
    }
}

/// My last submitted verdict among the `reviews` of a GitHub pull request: a review of `login`
/// that approves, asks for changes or comments, on a commit. Pending and dismissed reviews are
/// not verdicts. Pure.
#[must_use]
pub fn github_verdict(reviews: &[Review], login: &str) -> Option<SeenVerdict> {
    reviews
        .iter()
        .filter(|review| review.author.eq_ignore_ascii_case(login))
        .filter(|review| match review.state {
            ReviewState::Commented | ReviewState::Approved | ReviewState::ChangesRequested => true,
            ReviewState::Pending | ReviewState::Dismissed => false,
        })
        .filter_map(|review| {
            Some((
                review.submitted_at?,
                SeenVerdict {
                    sha: review.commit_sha.clone()?,
                    id: review.id.clone(),
                },
            ))
        })
        .max_by_key(|(at, _)| *at)
        .map(|(_, verdict)| verdict)
}

/// My verdict on a GitLab merge request: my approval of the head `head`. GitLab does not say
/// which head an approval was given on: it is taken as the head of the poll that first sees it.
/// Pure.
#[must_use]
pub fn gitlab_verdict(user_has_approved: bool, head: &Sha) -> Option<SeenVerdict> {
    user_has_approved.then(|| SeenVerdict {
        sha: head.clone(),
        id: "approved".to_owned(),
    })
}

/// The verdict to remember after a poll that saw `latest`, and whether it is new: one that
/// the last poll did not see. A verdict no longer shown (a GitLab approval reset by a push)
/// stays remembered. Pure.
#[must_use]
pub fn next_verdict(
    recorded: Option<&SeenVerdict>,
    latest: Option<SeenVerdict>,
) -> (Option<SeenVerdict>, bool) {
    match latest {
        Some(latest) if recorded != Some(&latest) => (Some(latest), true),
        Some(_) | None => (recorded.cloned(), false),
    }
}

/// A version of an MR in `versions.json`: the forge's version and when loom first saw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionRecord {
    /// The version.
    #[serde(flatten)]
    pub version: MrVersion,
    /// When a poll first saw this head.
    pub first_seen: Timestamp,
}

/// `known` (oldest first) with each version of `seen` whose head is new, first seen at `now`.
///
/// A known version gets the merge base and the forge version ID that it lacked (GitLab gives
/// them); the rest of it is kept. Pure.
#[must_use]
pub fn merge_versions(
    known: &[VersionRecord],
    seen: &[MrVersion],
    now: Timestamp,
) -> Vec<VersionRecord> {
    let mut versions = known.to_vec();
    // The forges list the newest first.
    let mut seen: Vec<&MrVersion> = seen.iter().collect();
    seen.sort_by_key(|version| version.created_at);
    for version in seen {
        if let Some(record) = versions
            .iter_mut()
            .find(|record| record.version.head_sha == version.head_sha)
        {
            if record.version.merge_base.is_none() {
                record.version.merge_base.clone_from(&version.merge_base);
            }
            if record.version.forge_version_id.is_none() {
                record.version.forge_version_id = version.forge_version_id;
            }
            if record.version.created_at.is_none() {
                record.version.created_at = version.created_at;
            }
        } else {
            versions.push(VersionRecord {
                version: version.clone(),
                first_seen: now,
            });
        }
    }
    versions
}

/// The Review task of `mr`, found by the context `context`: `inbox`, kind `review`, with an
/// `origin` link to the MR. Pure.
#[must_use]
pub fn new_review_task(mr: &Mr, context: &ContextName) -> NewTask {
    NewTask {
        title: format!("Review {}: {}", mr.key, mr.title),
        kind: Kind::Review,
        context: context.clone(),
        status: Status::Inbox,
        importance: None,
        due: None,
        pinned: false,
        links: vec![origin_link(&mr.key, &mr.web_url)],
        relations: Relations::default(),
    }
}

/// The `origin` link of the Review task of the MR `key`.
#[must_use]
pub fn origin_link(key: &MrKey, web_url: &str) -> Link {
    Link::Origin(ExternalRef {
        provider: provider(key.forge()).to_owned(),
        key: key.to_string(),
        url: Some(web_url.to_owned()),
    })
}

/// The MR key of a Review task: its `origin` link to GitHub or GitLab, and the host of its
/// URL. `None` for any other task. Pure.
#[must_use]
pub fn review_key(kind: &Kind, links: &[Link]) -> Option<(MrKey, Option<String>)> {
    if *kind != Kind::Review {
        return None;
    }
    links.iter().find_map(|link| match link {
        Link::Origin(origin) => {
            let key = MrKey::parse(&origin.key).ok()?;
            (provider(key.forge()) == origin.provider)
                .then(|| (key, origin.url.as_deref().and_then(url_host)))
        }
        Link::Related(_) | Link::Repo { .. } => None,
    })
}

/// The host of an `https://host[:port]/…` URL, lowercase, without the port.
fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// The indexed clone of project `project` on `host`: a repository with a remote that names
/// them (case ignored), the most recently used first, then the shortest path. Pure.
#[must_use]
pub fn clone_of<'a>(
    repos: &'a [RepoSummary],
    host: &str,
    project: &str,
) -> Option<&'a RepoSummary> {
    repos
        .iter()
        .filter(|repo| {
            repo.identity.remotes.iter().any(|remote| {
                remote.location.as_ref().is_some_and(|location| {
                    location.host.eq_ignore_ascii_case(host)
                        && location.path.eq_ignore_ascii_case(project)
                })
            })
        })
        .min_by(|a, b| {
            b.last_used.cmp(&a.last_used).then_with(|| {
                let length = |repo: &RepoSummary| repo.identity.path.as_os_str().len();
                length(a).cmp(&length(b))
            })
        })
}

/// The bytes of the daemon-owned files of an MR mirror.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mirror {
    /// `mr.json`.
    pub mr: Vec<u8>,
    /// `description.md`.
    pub description: Vec<u8>,
    /// `versions.json`.
    pub versions: Vec<u8>,
    /// `threads.json`, `None` when the poll did not pull the threads: the file is kept.
    pub threads: Option<Vec<u8>>,
    /// `sync.json`.
    pub sync: Vec<u8>,
}

impl Mirror {
    /// The files, by name; `threads.json` only when pulled.
    #[must_use]
    pub fn files(&self) -> Vec<(&'static str, &[u8])> {
        let mut files = vec![
            (MR_FILE, self.mr.as_slice()),
            (DESCRIPTION_FILE, self.description.as_slice()),
            (VERSIONS_FILE, self.versions.as_slice()),
            (SYNC_FILE, self.sync.as_slice()),
        ];
        if let Some(threads) = &self.threads {
            files.push((THREADS_FILE, threads.as_slice()));
        }
        files
    }
}

/// The mirror files of `mr`, with its `versions`, its `threads` when pulled (sorted) and the
/// sync `state`. Pure.
///
/// # Errors
///
/// The serialization error, which the model types never give.
pub fn mirror(
    mr: &Mr,
    versions: &[VersionRecord],
    threads: Option<&[Thread]>,
    state: &SyncState,
) -> Result<Mirror, serde_json::Error> {
    let mut snapshot = serde_json::to_value(mr)?;
    if let Some(object) = snapshot.as_object_mut() {
        // In `description.md`.
        object.remove("description");
    }
    let threads = threads
        .map(|threads| {
            let mut threads = threads.to_vec();
            loom_forge::sort_threads(&mut threads);
            json(&threads)
        })
        .transpose()?;
    Ok(Mirror {
        mr: json(&snapshot)?,
        description: mr.description.as_bytes().to_vec(),
        versions: json(&versions)?,
        threads,
        sync: json(state)?,
    })
}

fn json(value: &impl Serialize) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// The MR of the `mr.json` and `description.md` of a mirror. `None` when it cannot be read.
/// Pure.
#[must_use]
pub fn read_mr(mr_json: &[u8], description: &str) -> Option<Mr> {
    let mut value: serde_json::Value = serde_json::from_slice(mr_json).ok()?;
    value
        .as_object_mut()?
        .insert("description".to_owned(), description.into());
    serde_json::from_value(value).ok()
}

/// The path of the mirror of `key` in the task directory `task_dir`.
#[must_use]
pub fn mirror_dir(task_dir: &Path, key: &MrKey) -> std::path::PathBuf {
    task_dir.join(MRS_DIR).join(key.dir_name())
}

/// What a poll fetched of one MR, for the task thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MrUpdate {
    /// The key of the MR.
    pub key: MrKey,
    /// I am a requested reviewer now: the MR is in the search, or I am in its reviewers.
    pub requested: bool,
    /// The MR, `None` when it did not change since the last poll (304): the mirror holds it.
    pub mr: Option<Mr>,
    /// The versions that the forge lists (GitLab), or the current one (GitHub); empty when
    /// `mr` is `None`.
    pub versions: Vec<MrVersion>,
    /// The threads, when this poll pulled them.
    pub threads: Option<Vec<Thread>>,
    /// My last verdict as the forge shows it, when this poll read it.
    pub verdict: Option<Option<SeenVerdict>>,
}

/// One poll of a context and forge host, for the task thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MrBatch {
    /// The context of the store.
    pub context: ContextName,
    /// The host of the MRs.
    pub host: String,
    /// The MRs.
    pub updates: Vec<MrUpdate>,
}

/// The active Review tasks of `context` whose MR is on `host`, with their key. Pure.
#[must_use]
pub fn tracked_reviews(
    rows: &[IndexedTask],
    context: &ContextName,
    host: &str,
) -> Vec<(TaskId, MrKey)> {
    rows.iter()
        .filter(|row| row.context == *context && row.location == TaskLocation::Active)
        .filter_map(|row| match &row.state {
            IndexedState::Valid { task, .. } => {
                let (key, task_host) = review_key(&task.kind, &task.links)?;
                (task_host.as_deref() == Some(host)).then(|| (row.id.clone(), key))
            }
            IndexedState::Invalid { .. } => None,
        })
        .collect()
}

/// The Review task of `key` in `context`, with its status (`None`: unknown to this loom), and
/// whether it is archived (an archived task is never changed). Pure.
#[must_use]
pub fn find_review(
    rows: &[IndexedTask],
    context: &ContextName,
    key: &MrKey,
) -> Option<(TaskId, Option<StatusName>, bool)> {
    rows.iter()
        .filter(|row| row.context == *context)
        .find_map(|row| match &row.state {
            IndexedState::Valid { task, .. } => {
                let (found, _) = review_key(&task.kind, &task.links)?;
                (found == *key).then(|| {
                    (
                        row.id.clone(),
                        task.status.name(),
                        row.location != TaskLocation::Active,
                    )
                })
            }
            IndexedState::Invalid { .. } => None,
        })
}

/// What a failed poll of a host does to the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollOutcome {
    /// The poll succeeded.
    Done,
    /// The forge limits the rate: wait this long.
    RateLimited(Duration),
    /// Another failure, the `failures`-th in a row.
    Failed {
        /// Failures in a row, this one included.
        failures: u32,
    },
}

/// The wait before the next poll of a host, after `outcome`: the interval jittered by ±10 %
/// (`random` in `[0, 1]`), the rate-limit wait when longer, or the backoff of automatic
/// updates after a failure. Pure.
#[must_use]
pub fn next_poll(interval: Duration, outcome: PollOutcome, random: f64) -> Duration {
    match outcome {
        PollOutcome::Done => jittered(interval, random),
        PollOutcome::RateLimited(wait) => wait.max(jittered(interval, random)),
        PollOutcome::Failed { failures } => jittered(backoff(interval, failures), random),
    }
}

#[cfg(test)]
mod tests;
