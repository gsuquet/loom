//! The git repository of each context store (ADR-0013, decision 8), through the `git` CLI
//! (ADR-0008): created at the first commit, one commit per change, unsigned.
//!
//! Every command runs as the repository updates do (`repo_update`): in the store's root,
//! non-interactively, with a timeout after which its process group is stopped, and never
//! looking above the root for a repository. Commands run with `commit.gpgsign=false` and
//! `tag.gpgsign=false` (a daemon cannot answer a pinentry prompt, and the store is private data
//! that no forge reviews) and `core.hooksPath=/dev/null` (the store is loom's data: a user's
//! hook, such as a global pre-commit framework, must not block its backup). The user's identity
//! and other configuration apply.
//!
//! With `store.backup.remote`, [`sync`] fetches the remote `backup`, then pushes the local
//! commits or fast-forwards the store, as [`backup::decide_sync`] says. It never merges,
//! rebases, resets or forces: a divergence stops the sync of that store.
//!
//! What is committed, how commits are named and what a sync does is decided in
//! [`crate::backup`]. This module is in the library for its integration tests.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use loom_api::{BackupOutcome, BackupStep, ContextName, TaskId};
use tracing::{debug, info_span, warn};

use crate::backup::{self, GITATTRIBUTES, GITIGNORE, REMOTE, Scope, Sides, SyncAction};
use crate::repo_update::{Failure, Git, GitRunner};

/// Longest wait for a local git command of a store (`init`, `add`, `commit`).
pub const LOCAL_TIMEOUT: Duration = Duration::from_secs(60);

/// Branch of a store repository created by loomd.
pub const BRANCH: &str = "main";

/// Configuration given to every git command of a store, over the user's: no signing, no hooks.
const STORE_CONFIG: [&str; 6] = [
    "-c",
    "commit.gpgsign=false",
    "-c",
    "tag.gpgsign=false",
    "-c",
    "core.hooksPath=/dev/null",
];

/// A git command of a store that did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitError {
    /// The git command (`init`, `add`, `commit`…).
    pub step: &'static str,
    /// What went wrong: exit status, timeout, or why git could not start.
    pub error: String,
    /// The end of git's standard error.
    pub stderr_tail: String,
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "git {} failed: {}", self.step, self.error)?;
        if let Some(line) = self
            .stderr_tail
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
        {
            write!(f, ": {}", line.trim())?;
        }
        Ok(())
    }
}

impl GitError {
    pub(crate) fn new(step: &'static str, failure: Failure) -> Self {
        let (error, stderr_tail) = match failure {
            Failure::Run(error) => (format!("cannot run git: {error}"), String::new()),
            Failure::Timeout { after, stderr_tail } => {
                (format!("timed out after {}s", after.as_secs()), stderr_tail)
            }
            Failure::Exit {
                status,
                stderr_tail,
            } => (status.to_string(), stderr_tail),
        };
        Self {
            step,
            error,
            stderr_tail,
        }
    }

    fn io(step: &'static str, path: &Path, error: &io::Error) -> Self {
        Self {
            step,
            error: format!("{}: {error}", path.display()),
            stderr_tail: String::new(),
        }
    }
}

/// What a commit did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Committed {
    /// A commit was made.
    Created,
    /// Nothing was left to commit.
    Nothing,
}

/// Whether `root` holds a git repository of its own (`<root>/.git`).
#[must_use]
pub fn is_repository(root: &Path) -> bool {
    root.join(".git").exists()
}

/// The commits of every context store, and the stores whose last commit failed.
#[derive(Debug)]
pub struct StoreCommits {
    runner: GitRunner,
    roots: BTreeMap<ContextName, PathBuf>,
    /// The last failure of each store whose changes are not all committed.
    pending: BTreeMap<ContextName, GitError>,
}

impl StoreCommits {
    /// The commits of the stores `roots`, made with `runner`.
    #[must_use]
    pub const fn new(runner: GitRunner, roots: BTreeMap<ContextName, PathBuf>) -> Self {
        Self {
            runner,
            roots,
            pending: BTreeMap::new(),
        }
    }

    /// The stores whose changes are not all committed, with the last failure.
    #[must_use]
    pub const fn pending(&self) -> &BTreeMap<ContextName, GitError> {
        &self.pending
    }

    /// The root of the store of `context`.
    #[must_use]
    pub fn root(&self, context: &ContextName) -> Option<&Path> {
        self.roots.get(context).map(PathBuf::as_path)
    }

    /// Commits the change of task `id` in the store of `context` with `message`: its directory,
    /// or every change of the store when an earlier commit failed or the repository is new (see
    /// [`backup::scope`]). The repository is created first when the store has none.
    ///
    /// # Errors
    ///
    /// [`GitError`] when a git command fails; the store is then pending, and its next commit
    /// takes every change.
    pub fn commit_task(
        &mut self,
        context: &ContextName,
        id: &TaskId,
        message: &str,
    ) -> Result<Committed, GitError> {
        let Some(root) = self.roots.get(context) else {
            return Ok(Committed::Nothing);
        };
        let scope = backup::scope(id, self.pending.contains_key(context), is_repository(root));
        let result = commit(&self.runner, root, &scope, message);
        self.record(context, &scope, result)
    }

    /// Creates the repository of the store of `context` when it has none (without a commit,
    /// so that a fast-forward can fill an empty store), then commits every change with
    /// `message`: what a sync does before it fetches.
    ///
    /// # Errors
    ///
    /// [`GitError`] when a git command fails; the store is then pending.
    pub fn prepare_sync(&mut self, context: &ContextName, message: &str) -> Result<(), GitError> {
        let Some(root) = self.roots.get(context) else {
            return Ok(());
        };
        if !is_repository(root) {
            let git = Git::local(&self.runner, root);
            if let Err(error) = init(&git) {
                self.pending.insert(context.clone(), error.clone());
                return Err(error);
            }
        }
        self.commit_all(context, message).map(drop)
    }

    /// Fast-forwards the store of `context` to the commit `to`, when its working tree is clean.
    ///
    /// # Errors
    ///
    /// [`GitError`] when the store has uncommitted changes, or git refuses (not a
    /// fast-forward, a file in the way) or fails.
    pub fn fast_forward(&self, context: &ContextName, to: &str) -> Result<(), GitError> {
        let Some(root) = self.roots.get(context) else {
            return Ok(());
        };
        let _span = info_span!("store.fast_forward", root = %root.display(), to).entered();
        let git = Git::local(&self.runner, root);
        let status = run(&git, "status", &["status", "--porcelain"])?;
        if !status.is_empty() {
            return Err(GitError {
                step: "status",
                error: "the store has uncommitted changes".to_owned(),
                stderr_tail: status,
            });
        }
        run(
            &git,
            "merge",
            &["merge", "--ff-only", "--no-autostash", "--no-edit", to],
        )
        .map(drop)
    }

    /// Commits every change of the store of `context` with `message`. A store without tasks
    /// and without a commit is left as it is.
    ///
    /// # Errors
    ///
    /// [`GitError`] when a git command fails; the store is then pending.
    pub fn commit_all(
        &mut self,
        context: &ContextName,
        message: &str,
    ) -> Result<Committed, GitError> {
        let Some(root) = self.roots.get(context) else {
            return Ok(Committed::Nothing);
        };
        if !has_tasks(root) && !has_commit(&self.runner, root) {
            return Ok(Committed::Nothing);
        }
        let result = commit(&self.runner, root, &Scope::All, message);
        self.record(context, &Scope::All, result)
    }

    fn record(
        &mut self,
        context: &ContextName,
        scope: &Scope,
        result: Result<Committed, GitError>,
    ) -> Result<Committed, GitError> {
        match &result {
            Ok(_) => {
                // Only a commit of every change leaves nothing behind.
                if *scope == Scope::All {
                    self.pending.remove(context);
                }
            }
            Err(error) => {
                self.pending.insert(context.clone(), error.clone());
            }
        }
        result
    }
}

/// Whether `<root>/tasks` holds an entry other than a hidden one.
fn has_tasks(root: &Path) -> bool {
    fs::read_dir(root.join(loom_store::files::TASKS_DIR)).is_ok_and(|mut entries| {
        entries.any(|entry| {
            entry.is_ok_and(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        })
    })
}

/// Whether the store at `root` has a repository with a commit.
fn has_commit(runner: &GitRunner, root: &Path) -> bool {
    is_repository(root)
        && head(&Git::local(runner, root), "HEAD")
            .ok()
            .flatten()
            .is_some()
}

/// The commit `name` resolves to; `None` when it does not exist (an unborn `HEAD`).
fn head(git: &Git<'_>, name: &str) -> Result<Option<String>, GitError> {
    let spec = format!("{name}^{{commit}}");
    match git.output(
        &args(&["rev-parse", "--verify", "--quiet", &spec]),
        LOCAL_TIMEOUT,
    ) {
        Ok(id) => Ok(Some(id.trim().to_owned())),
        Err(Failure::Exit { status, .. }) if status.code() == Some(1) => Ok(None),
        Err(failure) => Err(GitError::new("rev-parse", failure)),
    }
}

/// Stages `scope` in the store at `root` and commits it with `message`, unsigned; creates the
/// repository first when there is none.
fn commit(
    runner: &GitRunner,
    root: &Path,
    scope: &Scope,
    message: &str,
) -> Result<Committed, GitError> {
    let _span = info_span!("store.commit", root = %root.display(), message).entered();
    let git = Git::local(runner, root);
    if !is_repository(root) {
        init(&git)?;
    }
    // A file just written is committed with this change, whatever its scope.
    let scope = if write_attributes(root)? {
        &Scope::All
    } else {
        scope
    };
    let path = match scope {
        Scope::Task(id) => format!("{}/{id}", loom_store::files::TASKS_DIR),
        Scope::All => ".".to_owned(),
    };
    run(&git, "add", &["add", "-A", "--", &path])?;
    // `--quiet` exits with 1 when something is staged.
    match git.output(&args(&["diff", "--cached", "--quiet"]), LOCAL_TIMEOUT) {
        Ok(_) => {
            debug!(root = %root.display(), "nothing to commit");
            return Ok(Committed::Nothing);
        }
        Err(Failure::Exit { status, .. }) if status.code() == Some(1) => {}
        Err(failure) => return Err(GitError::new("diff", failure)),
    }
    run(&git, "commit", &["commit", "-q", "-m", message])?;
    Ok(Committed::Created)
}

/// `git init` of a store on [`BRANCH`].
fn init(git: &Git<'_>) -> Result<(), GitError> {
    run(
        git,
        "init",
        &["init", "-q", &format!("--initial-branch={BRANCH}")],
    )
    .map(drop)
}

/// Writes the `.gitattributes` and `.gitignore` of the store at `root` when missing; whether
/// one was written.
fn write_attributes(root: &Path) -> Result<bool, GitError> {
    let mut created = false;
    for (name, text) in [(".gitattributes", GITATTRIBUTES), (".gitignore", GITIGNORE)] {
        let path = root.join(name);
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| file.write_all(text.as_bytes()));
        match written {
            Ok(()) => created = true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(GitError::io("init", &path, &error)),
        }
    }
    Ok(created)
}

/// `git -c commit.gpgsign=false -c tag.gpgsign=false -c core.hooksPath=/dev/null <command>`.
fn args<'a>(command: &[&'a str]) -> Vec<&'a OsStr> {
    STORE_CONFIG
        .iter()
        .chain(command)
        .map(|arg| OsStr::new(*arg))
        .collect()
}

/// Runs `command` (see [`args`]); its standard output.
pub(crate) fn run(git: &Git<'_>, step: &'static str, command: &[&str]) -> Result<String, GitError> {
    git.output(&args(command), LOCAL_TIMEOUT)
        .map_err(|failure| {
            let error = GitError::new(step, failure);
            warn!(%error, "store git command failed");
            error
        })
}

/// A step of a sync that runs on the task thread, serialized with the writes of the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalStep<'a> {
    /// Create the repository if needed and commit every change ([`StoreCommits::prepare_sync`]).
    Prepare,
    /// Fast-forward the store to this commit ([`StoreCommits::fast_forward`]).
    FastForward {
        /// The commit.
        to: &'a str,
    },
}

/// Syncs the store at `root` with the git repository `url`.
///
/// `local` prepares the store, the remote `backup` is set to `url` and fetched, then the local
/// commits are pushed to its [`BRANCH`] or the store is fast-forwarded by `local`, as
/// [`backup::decide_sync`] says.
pub fn sync(
    runner: &GitRunner,
    root: &Path,
    url: &str,
    local: &mut dyn FnMut(LocalStep<'_>) -> Result<(), GitError>,
) -> BackupOutcome {
    let _span = info_span!("store.sync", root = %root.display()).entered();
    let failed = |step, error: GitError| BackupOutcome::Failed {
        step,
        error: format!("git {}: {}", error.step, error.error),
        stderr_tail: error.stderr_tail,
    };
    if let Err(error) = local(LocalStep::Prepare) {
        return failed(BackupStep::Commit, error);
    }
    let git_dir = root.join(".git");
    let git = Git::remote(runner, root, &git_dir);
    if let Err(error) = set_remote(&git, url) {
        return failed(BackupStep::Remote, error);
    }
    let fetched = git.output(
        &args(&["fetch", "--prune", "--no-tags", REMOTE]),
        runner.fetch_timeout,
    );
    if let Err(failure) = fetched {
        return failed(BackupStep::Fetch, GitError::new("fetch", failure));
    }
    let tracking = format!("refs/remotes/{REMOTE}/{BRANCH}");
    let (sides, remote_head) = match compare(&git, &tracking) {
        Ok(compared) => compared,
        Err(error) => return failed(BackupStep::Compare, error),
    };
    match backup::decide_sync(sides) {
        SyncAction::UpToDate => BackupOutcome::UpToDate,
        SyncAction::Diverged { local, remote } => {
            warn!(root = %root.display(), local, remote, "the store and its backup remote diverged: not synced");
            BackupOutcome::Diverged { local, remote }
        }
        SyncAction::Push { commits } => {
            let refspec = format!("HEAD:refs/heads/{BRANCH}");
            match git.output(&args(&["push", REMOTE, &refspec]), runner.fetch_timeout) {
                Ok(_) => BackupOutcome::Pushed { commits },
                Err(Failure::Exit { stderr_tail, .. }) if backup::push_refused(&stderr_tail) => {
                    warn!(root = %root.display(), "the backup remote refused the push: commits kept local");
                    BackupOutcome::PushRejected { stderr_tail }
                }
                Err(failure) => failed(BackupStep::Push, GitError::new("push", failure)),
            }
        }
        SyncAction::FastForward { commits } => {
            let Some(to) = remote_head else {
                return BackupOutcome::UpToDate;
            };
            match local(LocalStep::FastForward { to: &to }) {
                Ok(()) => BackupOutcome::Pulled { commits },
                Err(error) => failed(BackupStep::FastForward, error),
            }
        }
    }
}

/// Points the remote [`REMOTE`] at `url`: added when missing, changed when it differs.
fn set_remote(git: &Git<'_>, url: &str) -> Result<(), GitError> {
    match git.output(&args(&["remote", "get-url", REMOTE]), LOCAL_TIMEOUT) {
        Ok(current) if current.trim_end() == url => Ok(()),
        Ok(_) => run(git, "remote", &["remote", "set-url", REMOTE, url]).map(drop),
        // `get-url` exits with 2 when there is no such remote.
        Err(Failure::Exit { status, .. }) if status.code() == Some(2) => {
            run(git, "remote", &["remote", "add", REMOTE, url]).map(drop)
        }
        Err(failure) => Err(GitError::new("remote", failure)),
    }
}

/// The sides of a sync, from `HEAD` and the remote-tracking ref `tracking`, and the commit of
/// the latter.
fn compare(git: &Git<'_>, tracking: &str) -> Result<(Sides, Option<String>), GitError> {
    let local = head(git, "HEAD")?;
    let remote = head(git, tracking)?;
    let count = |range: &str| -> Result<u64, GitError> {
        let output = run(git, "rev-list", &["rev-list", "--count", range])?;
        output.trim().parse().map_err(|_| GitError {
            step: "rev-list",
            error: format!("unexpected output `{}`", output.trim()),
            stderr_tail: String::new(),
        })
    };
    let sides = match (&local, &remote) {
        (None, None) => Sides::Empty,
        (Some(_), None) => Sides::LocalOnly(count("HEAD")?),
        (None, Some(_)) => Sides::RemoteOnly(count(tracking)?),
        (Some(_), Some(_)) => {
            let range = format!("HEAD...{tracking}");
            let output = run(
                git,
                "rev-list",
                &["rev-list", "--left-right", "--count", &range],
            )?;
            let (ahead, behind) = backup::parse_counts(&output).ok_or_else(|| GitError {
                step: "rev-list",
                error: format!("unexpected output `{}`", output.trim()),
                stderr_tail: String::new(),
            })?;
            Sides::Both { ahead, behind }
        }
    };
    Ok((sides, remote))
}
