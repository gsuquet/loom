//! The worker of MR refs and diffs (ADR-0016, decisions 3 and 4): one thread that, after each
//! poll of a context and forge host, gives the open Review tasks of that poll the refs and the
//! patches they lack. The decisions are in `loomd::mr_refs`, the task thread writes the mirrors.
//!
//! It has its own thread and its own failures. A fetch can wait 2 minutes, and a git failure
//! must not fail, delay or undo the mirror sync: a failure is logged, and the next poll asks for
//! the same work again, since the record of a diff is what ends the work of a head. A poll with
//! nothing new starts no git command.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use loom_api::{ContextName, ForgeKind, RepoSummary, TaskId};
use loom_forge::client::{
    Conditional, ForgeError, GithubClient, GithubForge as _, GitlabClient, GitlabForge as _,
    GitlabMr,
};
use loom_forge::diff::reduced_patch;
use loom_forge::model::{FileDiff, Mr, MrForgeIds, MrKey, MrState, Sha};
use loom_store::files::{ARCHIVE_DIR, TASKS_DIR};
use loom_store::port::TaskLocation;
use loom_store::{Reader, RepoFilter};
use loomd::forge::check::ForgeTarget;
use loomd::forge::token::TokenResolver;
use loomd::mr_refs::git::{
    Reviewed, Step, clone_candidates, delete_refs, fetch_mr, set_reviewed, version_diff,
};
use loomd::mr_refs::{
    DiffKind, DiffPlan, DiffRecord, MrClone, VersionDiff, choose_clone, diff_plan, patch_path,
    read_clone, refs_to_clean, review_rows,
};
use loomd::repo_update::GitRunner;
use loomd::review_sync::{
    MR_FILE, SYNC_FILE, SyncState, VERSIONS_FILE, VersionRecord, mirror_dir, read_mr,
    tracked_reviews,
};
use loomd::shutdown::{JOIN_DEADLINE, join_within};
use miette::{Context as _, IntoDiagnostic as _, Result};
use tracing::{debug, info_span, warn};

use crate::tasks::TaskHandle;

/// A request to the worker.
enum Request {
    /// A poll of this context and host ended.
    Pass(Box<ForgeTarget>),
    Stop,
}

/// The running worker.
pub struct MrWorker {
    requests: mpsc::Sender<Request>,
    stopping: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

/// What asks the worker for a pass. Never blocks the poller.
#[derive(Clone)]
pub struct MrWorkerHandle {
    requests: mpsc::Sender<Request>,
}

impl MrWorkerHandle {
    /// Asks for a pass over the Review tasks of `target`.
    pub fn pass(&self, target: &ForgeTarget) {
        let _ = self.requests.send(Request::Pass(Box::new(target.clone())));
    }
}

impl MrWorker {
    /// Stops the thread before its next MR. Waits for it up to [`JOIN_DEADLINE`]: the daemon
    /// stops the git commands that run.
    pub fn stop(self) {
        self.stopping.store(true, Ordering::Relaxed);
        let _ = self.requests.send(Request::Stop);
        join_within(self.thread, JOIN_DEADLINE, "mr-refs");
    }
}

/// Starts the worker thread.
pub fn start(
    runner: GitRunner,
    stores: BTreeMap<ContextName, PathBuf>,
    resolver: Arc<TokenResolver>,
    tasks: TaskHandle,
    user_agent: String,
) -> Result<(MrWorker, MrWorkerHandle)> {
    let (requests, received) = mpsc::channel();
    let stopping = Arc::new(AtomicBool::new(false));
    let worker = Worker {
        stopping: Arc::clone(&stopping),
        runner,
        stores,
        resolver,
        tasks,
        user_agent,
        reviewed: HashMap::new(),
        cleaned: HashSet::new(),
    };
    let thread = thread::Builder::new()
        .name("mr-refs".to_owned())
        .spawn(move || worker.run(&received))
        .into_diagnostic()
        .wrap_err("cannot start the MR refs thread")?;
    Ok((
        MrWorker {
            requests: requests.clone(),
            stopping,
            thread,
        },
        MrWorkerHandle { requests },
    ))
}

struct Worker {
    stopping: Arc<AtomicBool>,
    runner: GitRunner,
    stores: BTreeMap<ContextName, PathBuf>,
    resolver: Arc<TokenResolver>,
    tasks: TaskHandle,
    user_agent: String,
    /// What each `reviewed` ref points at, as far as this process set it: no git command when
    /// it does not change. A restart sets each one again, which changes nothing.
    reviewed: HashMap<(PathBuf, String, u64), Sha>,
    /// The archived tasks whose refs are gone, or that have none. A restart looks at each once
    /// more: so a cleanup that stopped with the daemon ends after the restart.
    cleaned: HashSet<TaskId>,
}

/// What is known not to work for the rest of a pass: its MRs are on one host, and each attempt
/// could wait for a timeout.
#[derive(Default)]
struct Down {
    git: bool,
    forge: bool,
}

/// The mirror of an MR, as the worker reads it.
struct MirrorRead {
    dir: PathBuf,
    mr: Mr,
    clone: Option<MrClone>,
    versions: Vec<VersionRecord>,
    state: Option<SyncState>,
}

fn read_mirror(dir: PathBuf) -> Option<MirrorRead> {
    let mr_json = std::fs::read(dir.join(MR_FILE)).ok()?;
    let read_json = |name: &str| std::fs::read(dir.join(name)).ok();
    Some(MirrorRead {
        mr: read_mr(&mr_json, "")?,
        clone: read_clone(&mr_json),
        versions: read_json(VERSIONS_FILE)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default(),
        state: read_json(SYNC_FILE).and_then(|bytes| serde_json::from_slice(&bytes).ok()),
        dir,
    })
}

impl Worker {
    fn stopping(&self) -> bool {
        self.stopping.load(Ordering::Relaxed)
    }

    fn run(mut self, received: &mpsc::Receiver<Request>) {
        while let Ok(request) = received.recv() {
            let Request::Pass(first) = request else {
                return;
            };
            // Polls that ended during the last pass ask for one pass each, at most.
            let mut targets = vec![first];
            for request in received.try_iter() {
                match request {
                    Request::Pass(target) => {
                        let same = |known: &ForgeTarget| {
                            known.context == target.context && known.host == target.host
                        };
                        if !targets.iter().any(|known| same(known)) {
                            targets.push(target);
                        }
                    }
                    Request::Stop => return,
                }
            }
            for target in targets {
                if self.stopping() {
                    return;
                }
                self.pass(&target);
            }
        }
    }

    /// One pass over the Review tasks of `target`.
    fn pass(&mut self, target: &ForgeTarget) {
        let span = info_span!("reviews.refs", context = %target.context, host = %target.host);
        let _entered = span.enter();
        let read = Reader::open(self.tasks.index_db()).and_then(|reader| {
            let repos = reader.list(&RepoFilter::default())?;
            // Each context: refs are per clone, and another context can have a task of the MR.
            let rows = reader.tasks(None)?;
            Ok((repos, rows))
        });
        let (repos, rows) = match read {
            Ok(read) => read,
            Err(error) => {
                warn!(%error, "cannot read the index, MR refs wait for the next poll");
                return;
            }
        };
        self.clean(target, &review_rows(&rows));
        let mut down = Down::default();
        for (id, key, _) in tracked_reviews(&rows, &target.context, &target.host) {
            if self.stopping() {
                return;
            }
            self.review(target, &repos, &id, &key, &mut down);
        }
    }

    /// Deletes the refs of the archived tasks of `target`.
    fn clean(&mut self, target: &ForgeTarget, rows: &[loomd::mr_refs::ReviewRow]) {
        let Some(root) = self.stores.get(&target.context).cloned() else {
            return;
        };
        for row in refs_to_clean(rows, &target.context, &target.host) {
            if self.cleaned.contains(&row.id) || self.stopping() {
                continue;
            }
            let TaskLocation::Archived(year) = row.location else {
                continue;
            };
            let task_dir = root
                .join(ARCHIVE_DIR)
                .join(format!("{year:04}"))
                .join(row.id.as_str());
            let clone = std::fs::read(mirror_dir(&task_dir, &row.key).join(MR_FILE))
                .ok()
                .and_then(|bytes| read_clone(&bytes))
                .filter(|clone| clone.path.is_dir());
            let number = row.key.number();
            let step = clone
                .as_ref()
                .map_or(Step::Done, |clone| delete_refs(&self.runner, clone, number));
            match step {
                Step::Done => {
                    if let Some(clone) = clone {
                        self.reviewed.remove(&(clone.path, clone.remote, number));
                    }
                    self.cleaned.insert(row.id.clone());
                }
                Step::Busy(_) => {
                    debug!(key = %row.key, "a git process holds a lock, the refs of the archived task go at the next poll");
                }
                Step::Failed(error) => {
                    warn!(key = %row.key, %error, "cannot delete the refs of an archived task, tried again at the next poll");
                }
            }
        }
    }

    /// The refs and the patches of the Review task `id`.
    fn review(
        &mut self,
        target: &ForgeTarget,
        repos: &[RepoSummary],
        id: &TaskId,
        key: &MrKey,
        down: &mut Down,
    ) {
        let Some(root) = self.stores.get(&target.context) else {
            return;
        };
        let task_dir = root.join(TASKS_DIR).join(id.as_str());
        let Some(mirror) = read_mirror(mirror_dir(&task_dir, key)) else {
            // No mirror yet, or one that the next full read replaces.
            return;
        };
        let head = mirror.mr.version.head_sha.clone();
        let recorded = mirror
            .versions
            .iter()
            .find(|record| record.version.head_sha == head)
            .and_then(|record| record.diff);
        let patch_exists = patch_path(&mirror.dir, &head).is_file();
        let candidates = clone_candidates(repos, &target.host, key.project());
        let clone = choose_clone(
            mirror.clone.as_ref(),
            &candidates,
            &target.host,
            key.project(),
        );
        // No more versions come; its refs stay until the archive.
        if mirror.mr.state != MrState::Open {
            return;
        }
        let plan = diff_plan(mirror.mr.state, recorded, patch_exists, clone.is_some());
        if let Some(clone) = &clone {
            self.keep_reviewed(clone, key, mirror.state.as_ref());
        }
        let diffs = match (plan, &clone) {
            (DiffPlan::FromClone, Some(clone)) if !down.git => {
                self.local_diffs(clone, key, &mirror, down)
            }
            (DiffPlan::FromForge, _) if !down.forge => self.forge_diff(target, key, &mirror, down),
            (DiffPlan::Keep | DiffPlan::FromClone | DiffPlan::FromForge, _) => Vec::new(),
        };
        if clone == mirror.clone && diffs.is_empty() {
            return;
        }
        // The fetch may have brought the commit that the last verdict is on.
        if let Some(clone) = &clone {
            self.keep_reviewed(clone, key, mirror.state.as_ref());
        }
        let record = DiffRecord {
            context: target.context.clone(),
            id: id.clone(),
            key: key.clone(),
            clone,
            diffs,
        };
        match self.tasks.record_mr_diffs(record) {
            Some(Ok(())) | None => {}
            Some(Err(error)) => {
                warn!(%key, %error, "cannot write the diffs of an MR, made again at the next poll");
            }
        }
    }

    /// Points the `reviewed` ref of the MR at my last verdict, before a fetch moves its head.
    fn keep_reviewed(&mut self, clone: &MrClone, key: &MrKey, state: Option<&SyncState>) {
        let Some(verdict) = state.and_then(|state| state.verdict.as_ref()) else {
            return;
        };
        let number = key.number();
        let cached = (clone.path.clone(), clone.remote.clone(), number);
        if self.reviewed.get(&cached) == Some(&verdict.sha) {
            return;
        }
        match set_reviewed(&self.runner, clone, number, &verdict.sha) {
            Reviewed::Set => {
                self.reviewed.insert(cached, verdict.sha.clone());
            }
            Reviewed::NoCommit => {
                debug!(%key, "the clone does not have the commit of the last verdict yet");
            }
            Reviewed::Not(Step::Busy(_) | Step::Done) => {
                debug!(%key, "a git process holds a lock, the reviewed ref moves at the next poll");
            }
            Reviewed::Not(Step::Failed(error)) => {
                warn!(%key, %error, "cannot move the reviewed ref of an MR");
            }
        }
    }

    /// Fetches the refs, then makes the patch of each version that the clone can give.
    fn local_diffs(
        &self,
        clone: &MrClone,
        key: &MrKey,
        mirror: &MirrorRead,
        down: &mut Down,
    ) -> Vec<VersionDiff> {
        match fetch_mr(&self.runner, clone, key, &mirror.mr.base_ref) {
            Step::Done => {}
            Step::Busy(_) => {
                debug!(%key, "a git process holds a lock, the MR is fetched at the next poll");
                return Vec::new();
            }
            Step::Failed(error) => {
                warn!(%key, %error, "cannot fetch an MR, tried again at the next poll");
                down.git = true;
                return Vec::new();
            }
        }
        mirror
            .versions
            .iter()
            .filter(|record| {
                record.diff != Some(DiffKind::Local)
                    || !patch_path(&mirror.dir, &record.version.head_sha).is_file()
            })
            .filter_map(|record| {
                let version = &record.version;
                match version_diff(&self.runner, &clone.path, version) {
                    Ok(Some(diff)) => Some(VersionDiff {
                        head: version.head_sha.clone(),
                        merge_base: Some(diff.merge_base),
                        kind: DiffKind::Local,
                        patch: diff.patch,
                    }),
                    Ok(None) => {
                        debug!(%key, head = version.head_sha.as_str(), "the clone does not have this version");
                        None
                    }
                    Err(error) => {
                        warn!(%key, head = version.head_sha.as_str(), %error, "cannot make the diff of a version");
                        None
                    }
                }
            })
            .collect()
    }

    /// The patch of the current head from the per-file patches of the forge. The forge gives
    /// the patches of its current head only: a head that moved during the read gives nothing,
    /// and the next poll sees it.
    fn forge_diff(
        &self,
        target: &ForgeTarget,
        key: &MrKey,
        mirror: &MirrorRead,
        down: &mut Down,
    ) -> Vec<VersionDiff> {
        let head = &mirror.mr.version.head_sha;
        let token = match self.resolver.resolve(&target.source) {
            Ok(token) => token,
            Err(error) => {
                warn!(%key, %error, "cannot resolve the forge token, no patch for the MR");
                down.forge = true;
                return Vec::new();
            }
        };
        let read = match target.kind {
            ForgeKind::Github => {
                let client = GithubClient::new(&target.api, token.clone(), &self.user_agent);
                github_files(&client, key, head)
            }
            ForgeKind::Gitlab => {
                let client = GitlabClient::new(&target.api, token.clone(), &self.user_agent);
                gitlab_files(&client, key, &mirror.mr, head)
            }
        };
        match read {
            Ok(Some(files)) => vec![VersionDiff {
                head: head.clone(),
                merge_base: None,
                kind: DiffKind::Reduced,
                patch: reduced_patch(&files),
            }],
            Ok(None) => Vec::new(),
            Err(error) => {
                down.forge = stops_the_pass(&error);
                let error = token.redact(&error.to_string());
                warn!(%key, error = error.as_str(), "cannot read the patches of an MR from the forge, tried again at the next poll");
                Vec::new()
            }
        }
    }
}

/// A failure that the other MRs of the host would meet too.
const fn stops_the_pass(error: &ForgeError) -> bool {
    match error {
        ForgeError::Unauthorized { .. }
        | ForgeError::RateLimited { .. }
        | ForgeError::SsoRequired { .. }
        | ForgeError::Transport(_) => true,
        ForgeError::BadRequest { .. }
        | ForgeError::InsufficientScope { .. }
        | ForgeError::Forbidden { .. }
        | ForgeError::NotFound { .. }
        | ForgeError::ApprovalRefused { .. }
        | ForgeError::Conflict { .. }
        | ForgeError::Unprocessable { .. }
        | ForgeError::Status { .. }
        | ForgeError::Graphql { .. }
        | ForgeError::Parse(_)
        | ForgeError::Unexpected(_) => false,
    }
}

/// Whether the forge still has `head` as the head of this answer.
fn still_head(answer: Conditional<Mr>, head: &Sha) -> bool {
    match answer {
        Conditional::Modified { value, .. } => value.version.head_sha == *head,
        // No `etag` was sent.
        Conditional::NotModified => false,
    }
}

fn github_files(
    client: &GithubClient,
    key: &MrKey,
    head: &Sha,
) -> Result<Option<Vec<FileDiff>>, ForgeError> {
    let files = client.files(key)?;
    Ok(still_head(client.pull(key, None)?, head).then_some(files))
}

fn gitlab_files(
    client: &GitlabClient,
    key: &MrKey,
    mr: &Mr,
    head: &Sha,
) -> Result<Option<Vec<FileDiff>>, ForgeError> {
    let MrForgeIds::Gitlab { project_id, .. } = mr.forge_ids else {
        return Ok(None);
    };
    let reference = GitlabMr {
        project_id,
        iid: key.number(),
    };
    let files = client.diffs(reference)?;
    Ok(still_head(client.merge_request(reference, None)?, head).then_some(files))
}
