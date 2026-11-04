//! The worker of MR refs and diffs (ADR-0016, decisions 3 and 4): one thread that, after each
//! poll of a context and forge host, gives the open Review tasks of that poll the refs and the
//! patches they lack. The decisions are in `loomd::mr_refs`, the task thread writes the mirrors.
//!
//! It has its own thread and its own failures. A fetch can wait 2 minutes, and a git failure
//! must not fail, delay or undo the mirror sync: a failure is logged, and the next poll asks for
//! the same work again, since the record of a diff is what ends the work of a head. A poll with
//! nothing new starts no git command.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use loom_api::{ContextName, ForgeKind, RemoteProblem, RepoSummary, TaskId};
use loom_forge::client::{
    Conditional, ForgeError, GithubClient, GithubForge as _, GitlabClient, GitlabForge as _,
    GitlabMr,
};
use loom_forge::diff::reduced_patch_within;
use loom_forge::model::{FileDiff, Mr, MrForgeIds, MrKey, MrState, Sha};
use loom_repo::remote_endpoint;
use loom_repo::update::UpdatePolicies;
use loom_store::files::{ARCHIVE_DIR, TASKS_DIR};
use loom_store::port::TaskLocation;
use loom_store::{Reader, RepoFilter};
use loomd::forge::check::ForgeTarget;
use loomd::forge::token::TokenResolver;
use loomd::hooks;
use loomd::mr_refs::git::{
    Reviewed, Step, clone_candidates, delete_refs, fetch_mr, has_head_ref, is_off, set_reviewed,
    version_diff,
};
use loomd::mr_refs::{
    DIFFS_DIR, DiffKind, DiffPlan, DiffRecord, MrClone, MrClones, Retry, VersionDiff, choose_clone,
    diff_plan, needs_refs, next_clones, patch_budget, patch_name, patch_path, read_clones,
    refs_to_clean, remote_names_other_project, review_rows, stops_the_endpoint, too_large_note,
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
    policies: UpdatePolicies,
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
        policies,
        stores,
        resolver,
        tasks,
        user_agent,
        reviewed: HashMap::new(),
        cleaned: HashSet::new(),
        retries: HashMap::new(),
        pass_file: std::env::var(hooks::MR_PASS_FILE_ENV)
            .ok()
            .as_deref()
            .and_then(|value| hooks::mr_pass_file(Some(value)))
            .map(Path::to_path_buf),
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
    /// A repository with the policy `off` gets no git command.
    policies: UpdatePolicies,
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
    /// The MRs whose last work failed. In memory: a restart tries each once more.
    retries: HashMap<TaskId, Backoff>,
    /// The file of the test hook.
    pass_file: Option<PathBuf>,
}

/// What is known not to work for the rest of a pass, since each try could wait for a timeout.
#[derive(Default)]
struct Down {
    /// The endpoints that failed for a reason of the host, and the clones whose remote is gone.
    git: HashSet<String>,
    /// The forge host: a failure of its token, of its rate or of its network.
    forge: bool,
}

/// The mirror of an MR, as the worker reads it.
struct MirrorRead {
    dir: PathBuf,
    mr: Mr,
    clones: MrClones,
    versions: Vec<VersionRecord>,
    state: Option<SyncState>,
}

fn read_mirror(dir: PathBuf) -> Option<MirrorRead> {
    let mr_json = std::fs::read(dir.join(MR_FILE)).ok()?;
    let read_json = |name: &str| std::fs::read(dir.join(name)).ok();
    Some(MirrorRead {
        mr: read_mr(&mr_json, "")?,
        clones: read_clones(&mr_json),
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
            let mut targets = vec![(first, 1)];
            for request in received.try_iter() {
                match request {
                    Request::Pass(target) => {
                        let known = targets.iter_mut().find(|(known, _)| {
                            known.context == target.context && known.host == target.host
                        });
                        match known {
                            Some((_, polls)) => *polls += 1,
                            None => targets.push((target, 1)),
                        }
                    }
                    Request::Stop => return,
                }
            }
            for (target, polls) in targets {
                if self.stopping() {
                    return;
                }
                self.pass(&target);
                self.signal_pass(polls);
            }
        }
    }

    /// The test hook: one line for each poll that the pass served.
    fn signal_pass(&self, polls: usize) {
        let Some(path) = &self.pass_file else {
            return;
        };
        let written = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| file.write_all("pass\n".repeat(polls).as_bytes()));
        if let Err(error) = written {
            warn!(%error, "cannot write the file of the test hook");
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
        self.clean(target, &repos, &review_rows(&rows));
        let mut down = Down::default();
        for (id, key, _) in tracked_reviews(&rows, &target.context, &target.host) {
            if self.stopping() {
                return;
            }
            self.review(target, &repos, &id, &key, &mut down);
        }
    }

    /// Deletes the refs of the archived tasks of `target`.
    fn clean(
        &mut self,
        target: &ForgeTarget,
        repos: &[RepoSummary],
        rows: &[loomd::mr_refs::ReviewRow],
    ) {
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
            let clones = std::fs::read(mirror_dir(&task_dir, &row.key).join(MR_FILE))
                .map(|bytes| read_clones(&bytes))
                .unwrap_or_default();
            let mut done = true;
            for clone in clones.clone.iter().chain(&clones.retired) {
                done &= self.drop_refs(repos, clone, &row.key, &target.host);
            }
            if done {
                self.cleaned.insert(row.id.clone());
            }
        }
    }

    /// Deletes the refs of `key` in `clone`, which no longer uses them. `true` when nothing is
    /// left to do: otherwise the next poll tries again. A repository with the policy `off` gets
    /// no git command, so its refs wait until the policy changes.
    fn drop_refs(
        &mut self,
        repos: &[RepoSummary],
        clone: &MrClone,
        key: &MrKey,
        host: &str,
    ) -> bool {
        if !clone.path.is_dir() {
            return true;
        }
        let indexed = repos.iter().find(|repo| repo.identity.path == clone.path);
        if indexed.is_some_and(|repo| is_off(repo, &self.policies)) {
            return false;
        }
        if remote_names_other_project(repos, clone, host, key.project()) {
            return true;
        }
        let number = key.number();
        match delete_refs(&self.runner, clone, number) {
            Step::Done => {
                self.reviewed
                    .remove(&(clone.path.clone(), clone.remote.clone(), number));
                true
            }
            Step::Busy(_) => {
                debug!(%key, "a git process holds a lock, the refs of the MR go at the next poll");
                false
            }
            Step::Failed { error, .. } => {
                warn!(%key, %error, "cannot delete the refs of an MR, tried again at the next poll");
                false
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
        // No more versions come; its refs stay until the archive.
        if mirror.mr.state != MrState::Open {
            return;
        }
        let head = mirror.mr.version.head_sha.clone();
        let recorded = mirror
            .versions
            .iter()
            .find(|record| record.version.head_sha == head)
            .and_then(|record| record.diff);
        let patch_exists = patch_path(&mirror.dir, &head).is_file();
        let candidates = clone_candidates(repos, &target.host, key.project(), &self.policies);
        let chosen = choose_clone(
            mirror.clones.clone.as_ref(),
            &candidates,
            &target.host,
            key.project(),
        );
        let mut clones = next_clones(&mirror.clones, chosen);
        let mut retired = std::mem::take(&mut clones.retired);
        retired.retain(|old| !self.drop_refs(repos, old, key, &target.host));
        clones.retired = retired;
        let plan = diff_plan(
            mirror.mr.state,
            recorded,
            patch_exists,
            clones.clone.is_some(),
        );
        let same_clone = clones.clone == mirror.clones.clone;
        let refs_only = clones.clone.as_ref().is_some_and(|clone| {
            // A read for each open MR at each poll: refs can go without loom knowing.
            let present = plan != DiffPlan::Keep
                || !same_clone
                || has_head_ref(&self.runner, clone, key.number());
            if !present {
                self.reviewed
                    .remove(&(clone.path.clone(), clone.remote.clone(), key.number()));
            }
            needs_refs(plan, same_clone, present)
        });
        if let Some(clone) = &clones.clone {
            self.keep_reviewed(clone, key, mirror.state.as_ref());
        }
        let work = Work {
            target,
            repos,
            id,
            key,
            mirror: &mirror,
            recorded,
            patch_exists,
        };
        let diffs = match (plan, &clones.clone) {
            (DiffPlan::FromClone, Some(clone)) => self.local_diffs(&work, clone, down),
            (DiffPlan::Keep, Some(clone)) if refs_only => {
                let _ = self.fetch(&work, clone, down);
                Vec::new()
            }
            (DiffPlan::FromForge, _) => self.reduced_diff(&work, down),
            (DiffPlan::Keep | DiffPlan::FromClone, _) => Vec::new(),
        };
        // The fetch may have brought the commit that the last verdict is on.
        if let Some(clone) = &clones.clone {
            self.keep_reviewed(clone, key, mirror.state.as_ref());
        }
        if clones == mirror.clones && diffs.is_empty() {
            return;
        }
        let record = DiffRecord {
            context: target.context.clone(),
            id: id.clone(),
            key: key.clone(),
            clones,
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
            Reviewed::Not(Step::Failed { error, .. }) => {
                warn!(%key, %error, "cannot move the reviewed ref of an MR");
            }
        }
    }

    /// Whether this poll tries the failing work of the MR. The backoff of a head and a base
    /// says nothing of a new head or base.
    fn due(&mut self, work: &Work<'_>) -> bool {
        let mr = &work.mirror.mr;
        let entry = self
            .retries
            .entry(work.id.clone())
            .or_insert_with(|| Backoff {
                head: mr.version.head_sha.clone(),
                base_ref: mr.base_ref.clone(),
                retry: Retry::default(),
            });
        if entry.head != mr.version.head_sha || entry.base_ref != mr.base_ref {
            entry.head.clone_from(&mr.version.head_sha);
            entry.base_ref.clone_from(&mr.base_ref);
            entry.retry = Retry::default();
        }
        entry.retry.due()
    }

    fn failed(&mut self, work: &Work<'_>) {
        if let Some(entry) = self.retries.get_mut(work.id) {
            entry.retry.failed();
        }
    }

    /// Fetches the refs of the MR, unless its endpoint or clone is down, or the MR waits.
    fn fetch(&mut self, work: &Work<'_>, clone: &MrClone, down: &mut Down) -> Fetched {
        let endpoint = endpoint_of(work.repos, clone);
        let clone_key = clone.path.display().to_string();
        if down.git.contains(&endpoint) || down.git.contains(&clone_key) || !self.due(work) {
            return Fetched::Waits;
        }
        let key = work.key;
        match fetch_mr(&self.runner, clone, key, &work.mirror.mr.base_ref) {
            Step::Done => Fetched::Done,
            Step::Busy(_) => {
                debug!(%key, "a git process holds a lock, the MR is fetched at the next poll");
                Fetched::Waits
            }
            Step::Failed { error, problem } if stops_the_endpoint(problem) => {
                warn!(%key, %error, problem = problem.as_str(), "cannot fetch an MR, the other MRs of its endpoint wait for the next poll");
                down.git.insert(endpoint);
                Fetched::Waits
            }
            Step::Failed { error, problem } => {
                warn!(%key, %error, problem = problem.as_str(), "cannot fetch an MR, tried again later");
                if matches!(problem, RemoteProblem::NotFound) {
                    down.git.insert(clone_key);
                }
                self.failed(work);
                Fetched::Failed
            }
        }
    }

    /// Fetches the refs, then makes the patch of each version that the clone can give. The
    /// current head comes first: it has the first right to the room of the MR. When it gets no
    /// local patch, it gets the patches of the forge.
    fn local_diffs(
        &mut self,
        work: &Work<'_>,
        clone: &MrClone,
        down: &mut Down,
    ) -> Vec<VersionDiff> {
        match self.fetch(work, clone, down) {
            Fetched::Done => {}
            Fetched::Waits => return Vec::new(),
            Fetched::Failed => return self.fallback(work, down),
        }
        let MirrorRead {
            dir, mr, versions, ..
        } = work.mirror;
        let head = &mr.version.head_sha;
        let key = work.key;
        let mut used = patches_size(dir, head);
        let mut diffs = Vec::new();
        let mut head_over = None;
        let todo = versions
            .iter()
            .filter(|record| record.version.head_sha == *head)
            .chain(versions.iter().filter(|record| {
                record.version.head_sha != *head
                    && !(matches!(record.diff, Some(DiffKind::Local | DiffKind::TooLarge))
                        && patch_path(dir, &record.version.head_sha).is_file())
            }));
        for record in todo {
            let version = &record.version;
            let limit = patch_budget(used);
            match version_diff(&self.runner, &clone.path, version, limit) {
                Ok(Some(diff)) if !diff.over_limit => {
                    used = used.saturating_add(diff.patch.len() as u64);
                    diffs.push(VersionDiff {
                        head: version.head_sha.clone(),
                        merge_base: Some(diff.merge_base),
                        kind: DiffKind::Local,
                        patch: diff.patch,
                    });
                }
                Ok(Some(diff)) => {
                    warn!(%key, head = version.head_sha.as_str(), limit, "the diff of a version is over the limit of its patch");
                    if version.head_sha == *head {
                        head_over = Some((diff.merge_base, limit));
                    } else {
                        // The forge gives only its current head: a note says why no patch.
                        diffs.push(VersionDiff {
                            head: version.head_sha.clone(),
                            merge_base: Some(diff.merge_base),
                            kind: DiffKind::TooLarge,
                            patch: too_large_note(limit),
                        });
                    }
                }
                Ok(None) => {
                    debug!(%key, head = version.head_sha.as_str(), "the clone does not have this version");
                }
                Err(error) => {
                    warn!(%key, head = version.head_sha.as_str(), %error, "cannot make the diff of a version");
                }
            }
        }
        if let Some((merge_base, limit)) = head_over {
            match self.forge_patch(work, limit, down) {
                Some(patch) => {
                    let mut noted = too_large_note(limit);
                    noted.extend(patch);
                    diffs.push(VersionDiff {
                        head: head.clone(),
                        merge_base: Some(merge_base),
                        kind: DiffKind::TooLarge,
                        patch: noted,
                    });
                }
                None => self.failed(work),
            }
        } else if !diffs.iter().any(|diff| diff.head == *head) {
            // The clone cannot give it (no base, or no head): a failure of this MR.
            self.failed(work);
            diffs.extend(self.fallback(work, down));
        } else {
            self.retries.remove(work.id);
        }
        diffs
    }

    /// The patches of the forge for an MR whose clone failed: nothing when it has them already.
    fn fallback(&self, work: &Work<'_>, down: &mut Down) -> Vec<VersionDiff> {
        if work.patch_exists && work.recorded == Some(DiffKind::Reduced) {
            return Vec::new();
        }
        self.reduced_version(work, down).into_iter().collect()
    }

    /// The patch of an MR without a clone.
    fn reduced_diff(&mut self, work: &Work<'_>, down: &mut Down) -> Vec<VersionDiff> {
        if down.forge || !self.due(work) {
            return Vec::new();
        }
        let version = self.reduced_version(work, down);
        if version.is_some() {
            self.retries.remove(work.id);
        } else if !down.forge {
            self.failed(work);
        }
        version.into_iter().collect()
    }

    fn reduced_version(&self, work: &Work<'_>, down: &mut Down) -> Option<VersionDiff> {
        let MirrorRead { dir, mr, .. } = work.mirror;
        let head = &mr.version.head_sha;
        let limit = patch_budget(patches_size(dir, head));
        Some(VersionDiff {
            head: head.clone(),
            merge_base: None,
            kind: DiffKind::Reduced,
            patch: self.forge_patch(work, limit, down)?,
        })
    }

    /// The patch of the current head from the per-file patches of the forge, within `limit`
    /// bytes. The forge gives the patches of its current head only: a head that moved during
    /// the read gives nothing, and the next poll sees it.
    fn forge_patch(&self, work: &Work<'_>, limit: usize, down: &mut Down) -> Option<Vec<u8>> {
        if down.forge {
            return None;
        }
        let Work {
            target,
            key,
            mirror,
            ..
        } = work;
        let head = &mirror.mr.version.head_sha;
        let token = match self.resolver.resolve(&target.source) {
            Ok(token) => token,
            Err(error) => {
                warn!(%key, %error, "cannot resolve the forge token, no patch for the MR");
                down.forge = true;
                return None;
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
            Ok(files) => files.map(|files| reduced_patch_within(&files, limit)),
            Err(error) => {
                down.forge = stops_the_pass(&error);
                let error = token.redact(&error.to_string());
                warn!(%key, error = error.as_str(), "cannot read the patches of an MR from the forge, tried again at the next poll");
                None
            }
        }
    }
}

/// How a fetch ended, for the work after it.
enum Fetched {
    Done,
    /// Nothing to tell of the MR: a lock, its endpoint down, or its backoff. The next poll sees.
    Waits,
    /// A failure of this MR.
    Failed,
}

/// What the steps of one MR read.
struct Work<'a> {
    target: &'a ForgeTarget,
    repos: &'a [RepoSummary],
    id: &'a TaskId,
    key: &'a MrKey,
    mirror: &'a MirrorRead,
    recorded: Option<DiffKind>,
    patch_exists: bool,
}

/// The backoff of the head and the base of one MR.
struct Backoff {
    head: Sha,
    base_ref: String,
    retry: Retry,
}

/// The key of the endpoint of the remote of `clone`; the path of the clone when it is unknown.
fn endpoint_of(repos: &[RepoSummary], clone: &MrClone) -> String {
    repos
        .iter()
        .find(|repo| repo.identity.path == clone.path)
        .and_then(|repo| {
            repo.identity
                .remotes
                .iter()
                .find(|remote| remote.name == clone.remote)
        })
        .map_or_else(
            || clone.path.display().to_string(),
            |remote| remote_endpoint(&remote.url).endpoint,
        )
}

/// The bytes of the patches of the mirror `dir`, without the one of `head`, which a new one
/// replaces.
fn patches_size(dir: &Path, head: &Sha) -> u64 {
    let own = patch_name(head);
    std::fs::read_dir(dir.join(DIFFS_DIR)).map_or(0, |entries| {
        entries
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy() != own.as_str())
            .filter_map(|entry| entry.metadata().ok())
            .map(|metadata| metadata.len())
            .sum()
    })
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
