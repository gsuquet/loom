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
//! What is committed and how commits are named is decided in [`crate::backup`]. This module is
//! in the library for its integration tests.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use loom_api::{ContextName, TaskId};
use tracing::{debug, info_span, warn};

use crate::backup::{self, GITATTRIBUTES, GITIGNORE, Scope};
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

    /// Commits every change of the store of `context` with `message`. A store without a
    /// repository and without tasks is left as it is.
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
        if !is_repository(root) && !has_tasks(root) {
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
        init(&git, root)?;
    }
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

/// `git init` of the store at `root` on [`BRANCH`], then its `.gitattributes` and `.gitignore`
/// when missing.
fn init(git: &Git<'_>, root: &Path) -> Result<(), GitError> {
    run(
        git,
        "init",
        &["init", "-q", &format!("--initial-branch={BRANCH}")],
    )?;
    for (name, text) in [(".gitattributes", GITATTRIBUTES), (".gitignore", GITIGNORE)] {
        let path = root.join(name);
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| file.write_all(text.as_bytes()));
        match written {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(GitError::io("init", &path, &error)),
        }
    }
    Ok(())
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
