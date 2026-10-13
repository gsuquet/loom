//! The state of a repository (its card), read with `gix`, never starting git (ADR-0008).
//!
//! Reading is bounded: the status walk, the untracked count and the ahead/behind counts share
//! a time budget, and the untracked count stops at a cap. A field that cannot be computed, in
//! budget or at all, is [`MaybeKnown::Unknown`] and the reason is added to
//! [`RepoState::error`]; the other fields are still read. Filter drivers (`filter.<driver>.clean`
//! or `.process`, such as git-lfs) are never run: they are removed from the in-memory
//! configuration before the status walk.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gix::bstr::{BStr, BString};
use gix::dir::walk::{Delegate, EmissionMode};
use gix::dir::{EntryRef, entry};
use gix::head::Kind;
use gix::prelude::ObjectIdExt as _;
use gix::progress::Discard;
use gix::remote::Direction;
use gix::state::InProgress;
use gix::status::UntrackedFiles;
use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};
use loom_api::{MaybeKnown, RepoHead, RepoOperation, RepoState, Timestamp, Untracked, Upstream};

use crate::scan::git;

/// Default time budget of one repository: status walk, untracked count and ahead/behind.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(2);

/// Untracked files counted at most; past it, the count is reported as capped.
pub const UNTRACKED_CAP: u64 = 1_000;

/// Commits walked between two looks at the clock while counting ahead and behind.
const CLOCK_EVERY: u64 = 256;

/// Which git configuration files `gix` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitConfig {
    /// The system, user and repository files, as git does (`HOME`, `XDG_CONFIG_HOME` and the
    /// `GIT_CONFIG_*` variables apply). Never the configuration of the git installation, which
    /// would start git to find it.
    User,
    /// Only the repository's own configuration: for tests.
    RepositoryOnly,
}

/// How [`state()`] reads a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateOptions {
    /// Time budget shared by the status walk, the untracked count and the ahead/behind counts.
    pub budget: Duration,
    /// Untracked files counted at most.
    pub untracked_cap: u64,
    /// Configuration files read.
    pub config: GitConfig,
}

impl Default for StateOptions {
    fn default() -> Self {
        Self {
            budget: DEFAULT_BUDGET,
            untracked_cap: UNTRACKED_CAP,
            config: GitConfig::User,
        }
    }
}

/// Reads the state of the repository whose working tree is `workdir`, with `git_dir` and
/// `common_dir` as [`crate::git_dirs`] resolves them. `now` is the `refreshed_at` of the
/// result.
///
/// Never fails: what cannot be read is [`MaybeKnown::Unknown`], with the reason in
/// [`RepoState::error`]. Nothing is written to the repository (not even the index stat
/// refresh `git status` does).
#[must_use]
pub fn state(
    git_dir: &Path,
    common_dir: &Path,
    workdir: &Path,
    now: Timestamp,
    options: &StateOptions,
) -> RepoState {
    let mut errors = Errors::default();
    let watchdog = Watchdog::start(options.budget);
    let last_fetch = errors.field("last_fetch", last_fetch(git_dir, common_dir));
    let mut repo = match gix::open_opts(workdir, open_options(options.config)) {
        Ok(repo) => repo,
        Err(error) => {
            errors.push(format!("cannot open the repository: {error}"));
            return RepoState {
                last_fetch,
                error: errors.into_message(),
                ..unreadable(now, String::new())
            };
        }
    };
    // Before anything reads the working tree: no filter driver may ever run.
    let drivers = disable_filter_drivers(&mut repo);
    let head = errors.field("head", read_head(&repo));
    let operation = MaybeKnown::Known(repo.state().as_ref().map(operation));
    let last_commit = head.as_known().map_or(MaybeKnown::Unknown, |head| {
        errors.field("last_commit", last_commit(&repo, head))
    });
    let default_branch = errors.field("default_branch", default_branch(&repo, common_dir));
    let dirty = errors.field("dirty", dirty(&repo, &drivers, &watchdog));
    let untracked = errors.field(
        "untracked",
        untracked(&repo, &watchdog, options.untracked_cap),
    );
    let upstream = head.as_known().map_or(MaybeKnown::Unknown, |head| {
        let upstream = upstream(&repo, head, &watchdog, &mut errors);
        errors.field("upstream", upstream)
    });
    RepoState {
        refreshed_at: now,
        head: head.map(|head| head.card),
        dirty,
        untracked,
        operation,
        upstream,
        default_branch,
        last_commit,
        last_fetch,
        error: errors.into_message(),
    }
}

/// The state of a repository that cannot be read at all, at `now`: every field unknown,
/// `error` saying why.
#[must_use]
pub const fn unreadable(now: Timestamp, error: String) -> RepoState {
    RepoState {
        refreshed_at: now,
        head: MaybeKnown::Unknown,
        dirty: MaybeKnown::Unknown,
        untracked: MaybeKnown::Unknown,
        operation: MaybeKnown::Unknown,
        upstream: MaybeKnown::Unknown,
        default_branch: MaybeKnown::Unknown,
        last_commit: MaybeKnown::Unknown,
        last_fetch: MaybeKnown::Unknown,
        error: Some(error),
    }
}

/// A ref and the commit it points to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefTip {
    /// Full ref name, such as `refs/remotes/origin/main`.
    pub name: String,
    /// Commit id, in hexadecimal.
    pub id: String,
}

/// The local default branch and its upstream, as an update needs them to move it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultBranchRefs {
    /// Short branch name, such as `main`.
    pub branch: String,
    /// Commit id of the local branch.
    pub id: String,
    /// `branch.<branch>.remote`: the primary remote.
    pub remote: String,
    /// `branch.<branch>.merge`: the full name of the branch on the remote.
    pub merge: String,
    /// The remote-tracking ref of the upstream and its commit.
    pub upstream: RefTip,
    /// Commits of the local branch not in its upstream.
    pub ahead: u64,
    /// Commits of the upstream not in the local branch.
    pub behind: u64,
    /// The branch is checked out, rebased or bisected in a working tree of the repository (the
    /// main one or a linked one).
    pub checked_out: bool,
}

/// What an update needs beyond [`RepoState`]: the commits it moves between, and the default
/// branch. Read with `gix`, never starting git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateRefs {
    /// Commit id of `HEAD`; `None` on an unborn branch.
    pub head: MaybeKnown<Option<String>>,
    /// The remote-tracking ref of the `HEAD` branch's upstream and its commit; `None` when
    /// detached, without an upstream, or when its ref does not exist.
    pub upstream: MaybeKnown<Option<RefTip>>,
    /// The default branch, when it exists locally and has an existing upstream on the primary
    /// remote; `None` otherwise (there is nothing to move then).
    pub default_branch: MaybeKnown<Option<DefaultBranchRefs>>,
    /// Why some fields are unknown; `None` when every field is known.
    pub error: Option<String>,
}

/// Reads the [`UpdateRefs`] of the repository whose working tree is `workdir`.
///
/// `common_dir` is as [`crate::git_dirs`] resolves it. `default_branch` is the field of the
/// state just read; unknown, it makes the default branch unknown.
///
/// Never fails: what cannot be read is [`MaybeKnown::Unknown`], with the reason in
/// [`UpdateRefs::error`]. Nothing is written.
#[must_use]
pub fn update_refs(
    common_dir: &Path,
    workdir: &Path,
    default_branch: &MaybeKnown<Option<String>>,
    options: &StateOptions,
) -> UpdateRefs {
    let mut errors = Errors::default();
    let watchdog = Watchdog::start(options.budget);
    let repo = match gix::open_opts(workdir, open_options(options.config)) {
        Ok(repo) => repo,
        Err(error) => {
            errors.push(format!("cannot open the repository: {error}"));
            return UpdateRefs {
                head: MaybeKnown::Unknown,
                upstream: MaybeKnown::Unknown,
                default_branch: MaybeKnown::Unknown,
                error: errors.into_message(),
            };
        }
    };
    let head = read_head(&repo);
    let head_id = match &head {
        Ok(head) => match head.card {
            RepoHead::Unborn { .. } => Ok(None),
            RepoHead::Branch { .. } | RepoHead::Detached { .. } => repo
                .head_id()
                .map(|id| Some(id.to_string()))
                .map_err(|error| error.to_string()),
        },
        Err(error) => Err(error.clone()),
    };
    let head_id = errors.field("head", head_id);
    let upstream = match &head {
        Ok(head) => head
            .branch
            .as_ref()
            .map_or(Ok(None), |branch| tracking_tip(&repo, branch.as_ref())),
        Err(error) => Err(error.clone()),
    };
    let upstream = errors.field("upstream", upstream);
    let default_branch = match default_branch {
        MaybeKnown::Known(None) => MaybeKnown::Known(None),
        MaybeKnown::Known(Some(branch)) => errors.field(
            "default_branch",
            default_branch_refs(&repo, common_dir, branch, &watchdog),
        ),
        MaybeKnown::Unknown => {
            errors.push("default_branch: unknown in the state".to_owned());
            MaybeKnown::Unknown
        }
    };
    UpdateRefs {
        head: head_id,
        upstream,
        default_branch,
        error: errors.into_message(),
    }
}

/// The remote-tracking ref of the upstream of `branch` and its commit; `None` when the branch
/// has no upstream or its ref does not exist.
fn tracking_tip(
    repo: &gix::Repository,
    branch: &gix::refs::FullNameRef,
) -> Result<Option<RefTip>, String> {
    let Some(tracking) = repo.branch_remote_tracking_ref_name(branch, Direction::Fetch) else {
        return Ok(None);
    };
    let tracking = tracking.map_err(|error| error.to_string())?;
    let Some(mut reference) = repo
        .try_find_reference(tracking.as_ref())
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let id = reference
        .peel_to_id()
        .map_err(|error| error.to_string())?
        .detach();
    Ok(Some(RefTip {
        name: tracking.as_bstr().to_string(),
        id: id.to_string(),
    }))
}

/// The local `branch` with its upstream; `None` when the branch does not exist locally, has no
/// upstream, its upstream is not on the primary remote, or its ref does not exist.
fn default_branch_refs(
    repo: &gix::Repository,
    common_dir: &Path,
    branch: &str,
    watchdog: &Watchdog,
) -> Result<Option<DefaultBranchRefs>, String> {
    let full = format!("refs/heads/{branch}");
    let Some(mut local) = repo
        .try_find_reference(full.as_str())
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let local_id = local
        .peel_to_id()
        .map_err(|error| error.to_string())?
        .detach();
    let name = local.name().to_owned();
    let Some(merge) = repo.branch_remote_ref_name(name.as_ref(), Direction::Fetch) else {
        return Ok(None);
    };
    let merge = merge.map_err(|error| error.to_string())?;
    let Some(remote) = repo.branch_remote_name(branch, Direction::Fetch) else {
        return Ok(None);
    };
    let remote = remote.as_bstr().to_string();
    // Only the primary remote is fetched: an upstream elsewhere is never moved.
    let remotes: Vec<String> = repo
        .remote_names()
        .into_iter()
        .map(|name| name.to_string())
        .collect();
    let names: Vec<&str> = remotes.iter().map(String::as_str).collect();
    if primary_remote(&names) != Some(remote.as_str()) {
        return Ok(None);
    }
    let Some(upstream) = tracking_tip(repo, name.as_ref())? else {
        return Ok(None);
    };
    let upstream_id =
        gix::ObjectId::from_hex(upstream.id.as_bytes()).map_err(|error| error.to_string())?;
    let (ahead, behind) = ahead_behind(repo, local_id, upstream_id, watchdog)?
        .ok_or_else(|| format!("ahead/behind: {}", watchdog.exceeded()))?;
    let checked_out = branch_checked_out(common_dir, branch)?;
    Ok(Some(DefaultBranchRefs {
        branch: branch.to_owned(),
        id: local_id.to_string(),
        remote,
        merge: merge.as_bstr().to_string(),
        upstream,
        ahead,
        behind,
        checked_out,
    }))
}

/// Files of a git directory that name the branch of its working tree: `HEAD`, the branch a
/// rebase works on, the branch a bisect started from.
const BRANCH_FILES: [&str; 4] = [
    "HEAD",
    "rebase-merge/head-name",
    "rebase-apply/head-name",
    "BISECT_START",
];

/// Whether `branch` is in use in a working tree of the repository whose common git directory
/// is `common_dir`: the main one (`<common>/HEAD`) and every linked one
/// (`<common>/worktrees/<id>/HEAD`), including a rebase or a bisect in progress. A file that
/// exists but cannot be read is an error: the answer would be a guess.
fn branch_checked_out(common_dir: &Path, branch: &str) -> Result<bool, String> {
    let mut dirs = vec![common_dir.to_path_buf()];
    let worktrees = common_dir.join("worktrees");
    match fs::read_dir(&worktrees) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|error| format!("{}: {error}", worktrees.display()))?;
                dirs.push(entry.path());
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{}: {error}", worktrees.display())),
    }
    for dir in dirs {
        for file in BRANCH_FILES {
            let path = dir.join(file);
            match fs::read_to_string(&path) {
                Ok(text) if names_branch(file, &text, branch) => return Ok(true),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("{}: {error}", path.display())),
            }
        }
    }
    Ok(false)
}

/// Whether `text`, the content of `file` (one of [`BRANCH_FILES`]), names `branch`. Pure.
fn names_branch(file: &str, text: &str, branch: &str) -> bool {
    let text = text.trim_end();
    let full = format!("refs/heads/{branch}");
    match file {
        "HEAD" => text.strip_prefix("ref:").map(str::trim) == Some(full.as_str()),
        // `git bisect start` records the branch name, or a commit id when detached.
        "BISECT_START" => text == branch || text == full,
        _ => text == full,
    }
}

/// Open options: read only what `config` allows, never start git.
fn open_options(config: GitConfig) -> gix::open::Options {
    let mut permissions = match config {
        GitConfig::User => gix::open::Permissions::all(),
        GitConfig::RepositoryOnly => gix::open::Permissions::isolated(),
    };
    // Both are off in `all()` and `isolated()`; set again so that no change of default can
    // start git to locate its installation files.
    permissions.config.git_binary = false;
    permissions.attributes.git_binary = false;
    gix::open::Options::default().permissions(permissions)
}

/// The reasons why fields are unknown.
#[derive(Default)]
struct Errors(Vec<String>);

impl Errors {
    fn push(&mut self, message: String) {
        self.0.push(message);
    }

    /// The value of `field`, or unknown with its error recorded.
    fn field<T>(&mut self, field: &str, value: Result<T, String>) -> MaybeKnown<T> {
        match value {
            Ok(value) => MaybeKnown::Known(value),
            Err(reason) => {
                self.push(format!("{field}: {reason}"));
                MaybeKnown::Unknown
            }
        }
    }

    fn into_message(self) -> Option<String> {
        join_errors(&self.0)
    }
}

/// The reasons, joined with `; `; `None` when there is none. Pure.
fn join_errors(errors: &[String]) -> Option<String> {
    (!errors.is_empty()).then(|| errors.join("; "))
}

/// Sets a flag when the budget runs out, for the `gix` walks to stop.
struct Watchdog {
    expired: Arc<AtomicBool>,
    deadline: Instant,
    budget: Duration,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Watchdog {
    fn start(budget: Duration) -> Self {
        let expired = Arc::new(AtomicBool::new(false));
        let (stop, stopped) = mpsc::channel::<()>();
        let flag = Arc::clone(&expired);
        // Without a thread the walks would still stop at the cap, but not at the deadline.
        let thread = thread::Builder::new()
            .name("repo-state-budget".to_owned())
            .spawn(move || {
                if stopped.recv_timeout(budget) == Err(RecvTimeoutError::Timeout) {
                    flag.store(true, Ordering::Release);
                }
            })
            .ok();
        Self {
            expired,
            deadline: Instant::now() + budget,
            budget,
            stop: Some(stop),
            thread,
        }
    }

    fn expired(&self) -> bool {
        self.expired.load(Ordering::Acquire) || Instant::now() >= self.deadline
    }

    fn exceeded(&self) -> String {
        format!("time budget of {:?} exceeded", self.budget)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// `HEAD`, with the full name of its branch for the upstream lookup.
struct Head {
    card: RepoHead,
    branch: Option<gix::refs::FullName>,
}

fn read_head(repo: &gix::Repository) -> Result<Head, String> {
    let head = repo.head().map_err(|error| error.to_string())?;
    Ok(match head.kind {
        Kind::Symbolic(reference) => Head {
            card: RepoHead::Branch {
                name: reference.name.shorten().to_string(),
            },
            branch: Some(reference.name),
        },
        Kind::Unborn(name) => Head {
            card: RepoHead::Unborn {
                name: name.shorten().to_string(),
            },
            branch: Some(name),
        },
        Kind::Detached { target, .. } => Head {
            card: RepoHead::Detached {
                commit: target.attach(repo).shorten_or_id().to_string(),
            },
            branch: None,
        },
    })
}

/// The operation of a `gix` state. Pure.
const fn operation(state: &InProgress) -> RepoOperation {
    match state {
        InProgress::ApplyMailbox => RepoOperation::Am,
        InProgress::ApplyMailboxRebase | InProgress::Rebase | InProgress::RebaseInteractive => {
            RepoOperation::Rebase
        }
        InProgress::Bisect => RepoOperation::Bisect,
        InProgress::CherryPick | InProgress::CherryPickSequence => RepoOperation::CherryPick,
        InProgress::Merge => RepoOperation::Merge,
        InProgress::Revert | InProgress::RevertSequence => RepoOperation::Revert,
    }
}

/// Committer date of `HEAD`'s commit; `None` on an unborn branch.
fn last_commit(repo: &gix::Repository, head: &Head) -> Result<Option<Timestamp>, String> {
    if matches!(head.card, RepoHead::Unborn { .. }) {
        return Ok(None);
    }
    let commit = repo.head_commit().map_err(|error| error.to_string())?;
    let time = commit.time().map_err(|error| error.to_string())?;
    u64::try_from(time.seconds)
        .ok()
        .and_then(Timestamp::from_unix_secs)
        .map(Some)
        .ok_or_else(|| format!("commit date {} out of range", time.seconds))
}

/// Latest modification time of `FETCH_HEAD`: a linked worktree has its own, beside the one of
/// the main working tree. `None` when there is none.
fn last_fetch(git_dir: &Path, common_dir: &Path) -> Result<Option<Timestamp>, String> {
    let mut latest = None;
    for dir in [git_dir, common_dir] {
        let path = dir.join("FETCH_HEAD");
        match fs::metadata(&path).and_then(|metadata| metadata.modified()) {
            Ok(modified) => latest = latest.max(Some(timestamp(modified)?)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", path.display())),
        }
    }
    Ok(latest)
}

fn timestamp(time: SystemTime) -> Result<Timestamp, String> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| Timestamp::from_unix_secs(elapsed.as_secs()))
        .ok_or_else(|| format!("time {time:?} out of range"))
}

/// The default branch: `refs/remotes/<primary>/HEAD`, otherwise `main`, otherwise `master`.
fn default_branch(repo: &gix::Repository, common_dir: &Path) -> Result<Option<String>, String> {
    // The primary remote as the scan finds it: `origin`, otherwise the first one of the config.
    let remotes = git::read_optional(&common_dir.join("config"))
        .map_err(|error| error.to_string())?
        .map_or_else(|| Ok(Vec::new()), |bytes| git::parse_remotes(&bytes))?;
    let names: Vec<&str> = remotes.iter().map(|(name, _)| name.as_str()).collect();
    let primary = primary_remote(&names);
    let remote_head = match primary {
        Some(remote) => {
            let name = format!("refs/remotes/{remote}/HEAD");
            let reference = repo
                .try_find_reference(name.as_str())
                .map_err(|error| error.to_string())?;
            reference.and_then(|reference| match reference.target() {
                gix::refs::TargetRef::Symbolic(target) => Some(target.as_bstr().to_string()),
                gix::refs::TargetRef::Object(_) => None,
            })
        }
        None => None,
    };
    let exists = |branch: &str| -> Result<bool, String> {
        let mut names = vec![format!("refs/heads/{branch}")];
        names.extend(primary.map(|remote| format!("refs/remotes/{remote}/{branch}")));
        for name in names {
            if repo
                .try_find_reference(name.as_str())
                .map_err(|error| error.to_string())?
                .is_some()
            {
                return Ok(true);
            }
        }
        Ok(false)
    };
    let main = exists("main")?;
    let master = !main && exists("master")?;
    Ok(choose_default_branch(
        primary,
        remote_head.as_deref(),
        main,
        master,
    ))
}

/// `origin`, otherwise the first remote. Pure.
fn primary_remote<'a>(names: &[&'a str]) -> Option<&'a str> {
    names
        .iter()
        .find(|name| **name == "origin")
        .or_else(|| names.first())
        .copied()
}

/// The default branch from the target of `refs/remotes/<primary>/HEAD` and whether `main` or
/// `master` exist (locally or on the primary remote). Pure.
fn choose_default_branch(
    primary: Option<&str>,
    remote_head_target: Option<&str>,
    main: bool,
    master: bool,
) -> Option<String> {
    let from_remote = primary
        .zip(remote_head_target)
        .and_then(|(remote, target)| {
            target
                .strip_prefix("refs/remotes/")
                .and_then(|rest| rest.strip_prefix(remote))
                .and_then(|rest| rest.strip_prefix('/'))
                .filter(|branch| !branch.is_empty())
        });
    from_remote
        .map(str::to_owned)
        .or_else(|| main.then(|| "main".to_owned()))
        .or_else(|| master.then(|| "master".to_owned()))
}

/// Removes every `[filter "<driver>"]` section from the in-memory configuration of `repo`
/// (never from its files), so that no clean or process filter driver can run during the
/// status. Returns the names of the drivers that had a `clean` or `process` command.
fn disable_filter_drivers(repo: &mut gix::Repository) -> Result<BTreeSet<String>, String> {
    let mut config = repo.config_snapshot_mut();
    let mut drivers = BTreeSet::new();
    let mut ids = Vec::new();
    for (section, id) in config
        .sections_and_ids_by_name("filter")
        .into_iter()
        .flatten()
    {
        let runs = section.value("clean").is_some() || section.value("process").is_some();
        if let Some(name) = section.header().subsection_name().filter(|_| runs) {
            drivers.insert(name.to_string());
        }
        ids.push(id);
    }
    for id in ids {
        config.remove_section_by_id(id);
    }
    config
        .commit()
        .map_err(|error| format!("cannot disable the filter drivers: {error}"))?;
    Ok(drivers)
}

/// Tracked paths with a change between `HEAD`, the index and the working tree, each counted
/// once.
///
/// Filter drivers are never run (`drivers` were removed from the configuration). A file whose
/// content has to be compared and whose `filter` attribute names one of them cannot be
/// compared without it: the count is then unknown, never a guess.
fn dirty(
    repo: &gix::Repository,
    drivers: &Result<BTreeSet<String>, String>,
    watchdog: &Watchdog,
) -> Result<u64, String> {
    let drivers = drivers.as_ref().map_err(Clone::clone)?;
    if watchdog.expired() {
        return Err(watchdog.exceeded());
    }
    let status = repo
        .status(Discard)
        .map_err(|error| error.to_string())?
        .untracked_files(UntrackedFiles::None)
        .index_worktree_rewrites(None)
        .index_worktree_submodules(gix::status::Submodule::AsConfigured { check_dirty: true })
        .should_interrupt_owned(Arc::clone(&watchdog.expired));
    let iter = status.into_iter(Vec::<BString>::new());
    let mut paths: HashSet<BString> = HashSet::new();
    // Files whose content was compared: without their driver, the comparison may be wrong.
    let mut compared: Vec<(BString, gix::index::entry::Mode)> = Vec::new();
    let result = iter.map_err(|error| error.to_string()).and_then(|iter| {
        for item in iter {
            let item = item.map_err(|error| error.to_string())?;
            let path: &BStr = match &item {
                gix::status::Item::IndexWorktree(item) => {
                    if let gix::status::index_worktree::Item::Modification {
                        entry,
                        rela_path,
                        status:
                            EntryStatus::Change(Change::Modification {
                                content_change: Some(()),
                                ..
                            }),
                        ..
                    } = item
                        && !drivers.is_empty()
                    {
                        compared.push((rela_path.clone(), entry.mode));
                    }
                    item.rela_path()
                }
                gix::status::Item::TreeIndex(change) => change.location(),
            };
            paths.insert(path.to_owned());
        }
        Ok(())
    });
    // An interrupted walk may end early without an error: its count is not complete.
    if watchdog.expired() {
        return Err(watchdog.exceeded());
    }
    result?;
    if let Some((path, driver)) = needing_driver(repo, drivers, &compared)? {
        return Err(format!(
            "filter driver `{driver}` not run: {path} changed on disk and can only be compared through it"
        ));
    }
    Ok(count(paths.len()))
}

/// The first of `compared` whose `filter` attribute names one of `drivers`, with the driver.
fn needing_driver(
    repo: &gix::Repository,
    drivers: &BTreeSet<String>,
    compared: &[(BString, gix::index::entry::Mode)],
) -> Result<Option<(BString, String)>, String> {
    if compared.is_empty() {
        return Ok(None);
    }
    let index = repo.index_or_empty().map_err(|error| error.to_string())?;
    let mut stack = repo
        .attributes_only(
            &index,
            gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
        )
        .map_err(|error| error.to_string())?;
    let mut outcome = stack.selected_attribute_matches(["filter"]);
    for (path, mode) in compared {
        let platform = stack
            .at_entry(path, Some(*mode))
            .map_err(|error| error.to_string())?;
        outcome.reset();
        platform.matching_attributes(&mut outcome);
        let driver = outcome
            .iter_selected()
            .find_map(|found| match found.assignment.state {
                gix::attrs::StateRef::Value(value) => Some(value.as_bstr().to_string()),
                gix::attrs::StateRef::Set
                | gix::attrs::StateRef::Unset
                | gix::attrs::StateRef::Unspecified => None,
            });
        if let Some(driver) = driver.filter(|driver| drivers.contains(driver)) {
            return Ok(Some((path.clone(), driver)));
        }
    }
    Ok(None)
}

/// Untracked files, `.gitignore` applied, counted up to `cap`.
fn untracked(repo: &gix::Repository, watchdog: &Watchdog, cap: u64) -> Result<Untracked, String> {
    if watchdog.expired() {
        return Err(watchdog.exceeded());
    }
    let index = repo.index_or_empty().map_err(|error| error.to_string())?;
    let options = repo
        .dirwalk_options()
        .map_err(|error| error.to_string())?
        .empty_patterns_match_prefix(false)
        .emit_untracked(EmissionMode::Matching)
        .emit_ignored(None)
        .emit_tracked(false)
        .emit_pruned(false)
        .emit_empty_directories(false);
    let mut counter = UntrackedCounter { count: 0, cap };
    let walked = repo.dirwalk(
        &index,
        Vec::<&BStr>::new(),
        &watchdog.expired,
        options,
        &mut counter,
    );
    if watchdog.expired() {
        return Err(watchdog.exceeded());
    }
    walked.map_err(|error| error.to_string())?;
    Ok(cap_count(counter.count, cap))
}

/// Counts the untracked entries, stopping after `cap + 1`.
struct UntrackedCounter {
    count: u64,
    cap: u64,
}

impl Delegate for UntrackedCounter {
    fn emit(
        &mut self,
        entry: EntryRef<'_>,
        _collapsed_directory_status: Option<entry::Status>,
    ) -> gix::dir::walk::Action {
        if entry.status == entry::Status::Untracked {
            self.count += 1;
        }
        if self.count > self.cap {
            gix::dir::walk::Action::Break(())
        } else {
            gix::dir::walk::Action::Continue(())
        }
    }
}

/// `count` reported against `cap`. Pure.
const fn cap_count(count: u64, cap: u64) -> Untracked {
    if count > cap {
        Untracked {
            count: cap,
            capped: true,
        }
    } else {
        Untracked {
            count,
            capped: false,
        }
    }
}

/// The upstream of the `HEAD` branch, `None` when detached, unconfigured or missing.
fn upstream(
    repo: &gix::Repository,
    head: &Head,
    watchdog: &Watchdog,
    errors: &mut Errors,
) -> Result<Option<Upstream>, String> {
    let Some(branch) = &head.branch else {
        return Ok(None);
    };
    let Some(tracking) = repo.branch_remote_tracking_ref_name(branch.as_ref(), Direction::Fetch)
    else {
        return Ok(None);
    };
    let tracking = tracking.map_err(|error| error.to_string())?;
    let Some(mut reference) = repo
        .try_find_reference(tracking.as_ref())
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let upstream_id = reference
        .peel_to_id()
        .map_err(|error| error.to_string())?
        .detach();
    let name = tracking.shorten().to_string();
    let head_id = match &head.card {
        RepoHead::Unborn { .. } => None,
        RepoHead::Branch { .. } | RepoHead::Detached { .. } => {
            Some(repo.head_id().map_err(|error| error.to_string())?.detach())
        }
    };
    // Unborn: nothing is ahead, every upstream commit is behind.
    let counts = head_id.map_or_else(
        || {
            count_commits(repo, upstream_id, None, watchdog)
                .map(|behind| behind.map(|behind| (0, behind)))
        },
        |head_id| ahead_behind(repo, head_id, upstream_id, watchdog),
    );
    let (ahead, behind) = match counts {
        Ok(Some((ahead, behind))) => (MaybeKnown::Known(ahead), MaybeKnown::Known(behind)),
        Ok(None) => {
            errors.push(format!("ahead/behind: {}", watchdog.exceeded()));
            (MaybeKnown::Unknown, MaybeKnown::Unknown)
        }
        Err(reason) => {
            errors.push(format!("ahead/behind: {reason}"));
            (MaybeKnown::Unknown, MaybeKnown::Unknown)
        }
    };
    Ok(Some(Upstream {
        name,
        ahead,
        behind,
    }))
}

/// `(ahead, behind)` of `head` against `upstream`; `None` when the budget runs out.
fn ahead_behind(
    repo: &gix::Repository,
    head: gix::ObjectId,
    upstream: gix::ObjectId,
    watchdog: &Watchdog,
) -> Result<Option<(u64, u64)>, String> {
    if head == upstream {
        return Ok(Some((0, 0)));
    }
    let Some(ahead) = count_commits(repo, head, Some(upstream), watchdog)? else {
        return Ok(None);
    };
    let Some(behind) = count_commits(repo, upstream, Some(head), watchdog)? else {
        return Ok(None);
    };
    Ok(Some((ahead, behind)))
}

/// Commits reachable from `tip` and not from `hidden` (`git rev-list --count tip ^hidden`);
/// `None` when the budget runs out.
fn count_commits(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    hidden: Option<gix::ObjectId>,
    watchdog: &Watchdog,
) -> Result<Option<u64>, String> {
    if watchdog.expired() {
        return Ok(None);
    }
    let walk = repo
        .rev_walk([tip])
        .with_hidden(hidden)
        .all()
        .map_err(|error| error.to_string())?;
    let mut count = 0;
    for info in walk {
        info.map_err(|error| error.to_string())?;
        count += 1;
        if count % CLOCK_EVERY == 0 && watchdog.expired() {
            return Ok(None);
        }
    }
    Ok(Some(count))
}

fn count(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        cap_count, choose_default_branch, join_errors, names_branch, operation, primary_remote,
    };
    use gix::state::InProgress;
    use loom_api::{RepoOperation, Untracked};

    #[test]
    fn every_in_progress_state_maps_to_an_operation() {
        for (state, expected) in [
            (InProgress::ApplyMailbox, RepoOperation::Am),
            (InProgress::ApplyMailboxRebase, RepoOperation::Rebase),
            (InProgress::Rebase, RepoOperation::Rebase),
            (InProgress::RebaseInteractive, RepoOperation::Rebase),
            (InProgress::Bisect, RepoOperation::Bisect),
            (InProgress::CherryPick, RepoOperation::CherryPick),
            (InProgress::CherryPickSequence, RepoOperation::CherryPick),
            (InProgress::Merge, RepoOperation::Merge),
            (InProgress::Revert, RepoOperation::Revert),
            (InProgress::RevertSequence, RepoOperation::Revert),
        ] {
            assert_eq!(operation(&state), expected);
        }
    }

    #[test]
    fn default_branch_prefers_the_remote_head_then_main_then_master() {
        let origin = Some("origin");
        let head = Some("refs/remotes/origin/trunk");
        assert_eq!(
            choose_default_branch(origin, head, true, false).as_deref(),
            Some("trunk")
        );
        assert_eq!(
            choose_default_branch(origin, Some("refs/remotes/origin/feat/x"), false, false)
                .as_deref(),
            Some("feat/x")
        );
        // A target outside the primary remote is ignored.
        for target in [
            "refs/remotes/upstream/trunk",
            "refs/remotes/originx/trunk",
            "refs/heads/trunk",
            "refs/remotes/origin/",
        ] {
            assert_eq!(
                choose_default_branch(origin, Some(target), true, false).as_deref(),
                Some("main"),
                "{target}"
            );
        }
        assert_eq!(
            choose_default_branch(origin, None, false, true).as_deref(),
            Some("master")
        );
        assert_eq!(
            choose_default_branch(None, head, true, true).as_deref(),
            Some("main")
        );
        assert_eq!(choose_default_branch(origin, None, false, false), None);
    }

    #[test]
    fn primary_remote_is_origin_then_the_first() {
        assert_eq!(primary_remote(&["upstream", "origin"]), Some("origin"));
        assert_eq!(primary_remote(&["upstream", "fork"]), Some("upstream"));
        assert_eq!(primary_remote(&[]), None);
    }

    #[test]
    fn untracked_count_is_capped() {
        assert_eq!(
            cap_count(3, 1_000),
            Untracked {
                count: 3,
                capped: false
            }
        );
        assert_eq!(
            cap_count(1_000, 1_000),
            Untracked {
                count: 1_000,
                capped: false
            }
        );
        assert_eq!(
            cap_count(1_001, 1_000),
            Untracked {
                count: 1_000,
                capped: true
            }
        );
    }

    #[test]
    fn branch_files_are_matched_on_the_full_name() {
        assert!(names_branch("HEAD", "ref: refs/heads/main\n", "main"));
        assert!(names_branch("HEAD", "ref:refs/heads/main", "main"));
        assert!(!names_branch("HEAD", "ref: refs/heads/main-2\n", "main"));
        assert!(!names_branch("HEAD", "0123abcd\n", "main"));
        assert!(names_branch(
            "rebase-merge/head-name",
            "refs/heads/main\n",
            "main"
        ));
        assert!(!names_branch(
            "rebase-apply/head-name",
            "detached HEAD\n",
            "main"
        ));
        assert!(names_branch("BISECT_START", "main\n", "main"));
        assert!(!names_branch("BISECT_START", "maintenance\n", "main"));
    }

    #[test]
    fn errors_are_joined() {
        assert_eq!(join_errors(&[]), None);
        assert_eq!(
            join_errors(&["a: x".to_owned(), "b: y".to_owned()]).as_deref(),
            Some("a: x; b: y")
        );
    }
}
