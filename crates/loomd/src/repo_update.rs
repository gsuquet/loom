//! The update of one repository (ADR-0012): fetch of the primary remote, then the
//! fast-forwards [`loom_repo::decide_update`] allows, through the `git` CLI (ADR-0008).
//!
//! This module runs git; it is in the library for its integration tests. What runs is decided
//! by the pure functions of `loom_repo::update`; the state is read with `gix` before the fetch,
//! after it, right before acting, and after the last command. Every command runs in the
//! repository's working tree, non-interactively, with a timeout after which its whole process
//! group is stopped. Nothing is ever rebased, reset, stashed, checked out, deleted or pruned.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use loom_api::{
    LastUpdate, RepoState, RepoUpdatePolicy, SkipReason, Timestamp, UpdateOutcome, UpdateStep,
};
use loom_repo::state::UpdateRefs;
use loom_repo::{
    Action, FetchDecision, StateOptions, decide_fetch, decide_update, git_dirs, state, update_refs,
};
use tracing::field::Empty;
use tracing::{Span, debug, info, info_span, warn};

/// Longest wait for `git fetch`.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(120);
/// Longest wait for `git merge --ff-only`, hooks included.
pub const MERGE_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest wait for `git config --get`.
pub const CONFIG_TIMEOUT: Duration = Duration::from_secs(10);
/// Time a process group has to exit after `SIGTERM`, before `SIGKILL`.
pub const KILL_GRACE: Duration = Duration::from_secs(5);
/// Bytes of standard error kept in an outcome.
pub const STDERR_TAIL_BYTES: usize = 2_000;
/// SSH command set when the user configured none: never ask for a password or a host key.
pub const BATCH_SSH_COMMAND: &str = "ssh -o BatchMode=yes";

/// Variables that would point git at another repository than the one updated.
const REPOSITORY_VARIABLES: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

/// How git commands are run.
#[derive(Debug, Clone)]
pub struct GitRunner {
    /// The `git` executable, looked up in `PATH` when it has no `/`.
    pub program: PathBuf,
    /// Variables set (`Some`) or removed (`None`) for every command, over the inherited
    /// environment.
    pub env: Vec<(OsString, Option<OsString>)>,
    /// `GIT_SSH_COMMAND` or `GIT_SSH` is in the environment git inherits: the user chose an SSH
    /// command, `BatchMode` is not forced.
    pub ssh_in_env: bool,
    /// Longest wait for a fetch.
    pub fetch_timeout: Duration,
    /// Longest wait for a fast-forward.
    pub merge_timeout: Duration,
    /// Time a stopped process group has between `SIGTERM` and `SIGKILL`.
    pub kill_grace: Duration,
}

impl GitRunner {
    /// `git` from `PATH`, the environment of this process, the default timeouts.
    #[must_use]
    pub fn from_env() -> Self {
        let ssh_in_env = ["GIT_SSH_COMMAND", "GIT_SSH"]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
        Self {
            program: PathBuf::from("git"),
            env: Vec::new(),
            ssh_in_env,
            fetch_timeout: FETCH_TIMEOUT,
            merge_timeout: MERGE_TIMEOUT,
            kill_grace: KILL_GRACE,
        }
    }
}

/// The repository to update.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    /// Root of its working tree.
    pub path: &'a Path,
    /// Its slug, for the logs and traces.
    pub slug: &'a str,
    /// Name of its primary remote.
    pub primary_remote: Option<&'a str>,
    /// Its update policy.
    pub policy: RepoUpdatePolicy,
}

/// Whether the fetch ran and succeeded: what the scheduler's backoff needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchStatus {
    /// Skipped before any git command.
    NotRun,
    /// `git fetch` exited with status 0.
    Succeeded,
    /// `git fetch` failed, timed out or could not start.
    Failed,
}

/// The result of [`update`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// What happened, to store as the repository's last update.
    pub update: LastUpdate,
    /// The state read after the last git command; `None` when no git command ran.
    pub state: Option<RepoState>,
    /// Whether the fetch ran and succeeded.
    pub fetch: FetchStatus,
}

/// Updates the repository `target`: decides, fetches, decides again on the state read after
/// the fetch, fast-forwards. `clock` gives the time of the result.
#[must_use]
pub fn update(
    target: Target<'_>,
    runner: &GitRunner,
    options: &StateOptions,
    clock: &dyn Fn() -> Timestamp,
) -> Report {
    let span = info_span!(
        "repo.update",
        slug = target.slug,
        path = %target.path.display(),
        policy = %target.policy,
        outcome = Empty,
    );
    let _entered = span.enter();
    let report = run(target, runner, options, clock);
    let outcome = summary(&report.update);
    span.record("outcome", outcome.as_str());
    if report.update.failed() {
        warn!(slug = target.slug, %outcome, "repository update failed");
    } else {
        info!(slug = target.slug, %outcome, "repository updated");
    }
    report
}

/// The outcomes on one line, separated by `; `.
#[must_use]
pub fn summary(update: &LastUpdate) -> String {
    update
        .outcomes
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

fn run(
    target: Target<'_>,
    runner: &GitRunner,
    options: &StateOptions,
    clock: &dyn Fn() -> Timestamp,
) -> Report {
    let finish = |outcomes: Vec<UpdateOutcome>, state: Option<RepoState>, fetch| Report {
        update: LastUpdate {
            time: clock(),
            policy: target.policy,
            outcomes,
        },
        state,
        fetch,
    };
    let skipped = |reason| vec![UpdateOutcome::Skipped { reason }];
    let (git_dir, common_dir) = match git_dirs(target.path) {
        Ok(dirs) => dirs,
        Err(error) => {
            return finish(
                skipped(SkipReason::Unreadable {
                    error: error.to_string(),
                }),
                None,
                FetchStatus::NotRun,
            );
        }
    };
    let read = || state(&git_dir, &common_dir, target.path, clock(), options);
    let remote = match decide_fetch(target.policy, target.primary_remote, &read()) {
        FetchDecision::Skip(reason) => return finish(skipped(reason), None, FetchStatus::NotRun),
        FetchDecision::Fetch { remote } => remote,
    };

    let git = Git::new(runner, target.path, &git_dir);
    if let Err(failure) = git.fetch(&remote) {
        return finish(
            vec![failure.outcome(UpdateStep::Fetch)],
            Some(read()),
            FetchStatus::Failed,
        );
    }

    // Read right before acting: the working tree may have changed since the first read.
    let after = read();
    let refs = update_refs(&common_dir, target.path, &after.default_branch, options);
    if let Some(error) = &refs.error {
        debug!(path = %target.path.display(), error, "update refs partly unknown");
    }
    let actions = decide_update(target.policy, &after, &refs);
    let mut outcomes = Vec::with_capacity(actions.len());
    for action in actions {
        outcomes.push(match action {
            Action::FetchOnly => UpdateOutcome::Fetched,
            Action::Skip(reason) => UpdateOutcome::Skipped { reason },
            Action::FastForward { branch, from, to } => git.fast_forward(branch, from, &to.id),
            Action::MoveDefaultBranch {
                branch,
                remote,
                merge,
                from,
                to,
            } => git.move_default_branch(branch, &remote, &merge, from, to),
        });
    }
    let state = read();
    let refs = update_refs(&common_dir, target.path, &state.default_branch, options);
    for outcome in &mut outcomes {
        settle(outcome, &refs);
    }
    finish(outcomes, Some(state), FetchStatus::Succeeded)
}

/// Replaces the expected `to` of a move with the commit read after it, when known.
fn settle(outcome: &mut UpdateOutcome, refs: &UpdateRefs) {
    match outcome {
        UpdateOutcome::FastForwarded { to, .. } => {
            if let Some(Some(head)) = refs.head.as_known() {
                to.clone_from(head);
            }
        }
        UpdateOutcome::DefaultBranchMoved { branch, to, .. } => {
            if let Some(Some(default)) = refs.default_branch.as_known()
                && default.branch == *branch
            {
                to.clone_from(&default.id);
            }
        }
        UpdateOutcome::Fetched | UpdateOutcome::Skipped { .. } | UpdateOutcome::Failed { .. } => {}
    }
}

/// A git command that did not succeed.
#[derive(Debug)]
enum Failure {
    /// It could not start, or its end could not be read.
    Run(io::Error),
    /// It did not end in time; its process group was stopped.
    Timeout {
        after: Duration,
        stderr_tail: String,
    },
    /// It exited with a non-zero status.
    Exit {
        status: ExitStatus,
        stderr_tail: String,
    },
}

impl Failure {
    fn outcome(self, step: UpdateStep) -> UpdateOutcome {
        let (error, stderr_tail) = match self {
            Self::Run(error) => (format!("cannot run git: {error}"), String::new()),
            Self::Timeout { after, stderr_tail } => {
                (format!("timed out after {}s", after.as_secs()), stderr_tail)
            }
            Self::Exit {
                status,
                stderr_tail,
            } => (status.to_string(), stderr_tail),
        };
        UpdateOutcome::Failed {
            step,
            error,
            stderr_tail,
        }
    }
}

/// git in one working tree.
struct Git<'a> {
    runner: &'a GitRunner,
    workdir: &'a Path,
    /// `GIT_SSH_COMMAND` to set, `None` when the user configured one.
    ssh_command: Option<&'static str>,
}

impl<'a> Git<'a> {
    fn new(runner: &'a GitRunner, workdir: &'a Path, git_dir: &Path) -> Self {
        let mut git = Self {
            runner,
            workdir,
            ssh_command: None,
        };
        let configured = git.config("core.sshCommand");
        if let Err(error) = &configured {
            debug!(git_dir = %git_dir.display(), %error, "cannot read core.sshCommand");
        }
        git.ssh_command =
            batch_ssh_command(configured.ok().flatten().as_deref(), runner.ssh_in_env);
        git
    }

    /// `git config --get <key>`; `None` when unset.
    fn config(&self, key: &str) -> Result<Option<String>, String> {
        match self.output(
            &[OsStr::new("config"), OsStr::new("--get"), OsStr::new(key)],
            CONFIG_TIMEOUT,
        ) {
            Ok(stdout) => Ok(Some(stdout.trim_end().to_owned())),
            // `git config --get` exits with 1 when the key is unset.
            Err(Failure::Exit { status, .. }) if status.code() == Some(1) => Ok(None),
            Err(failure) => Err(format!("{failure:?}")),
        }
    }

    /// `git fetch --no-prune --tags <remote>`: no ref is ever deleted.
    fn fetch(&self, remote: &str) -> Result<(), Failure> {
        let span = info_span!("repo.fetch", remote, outcome = Empty);
        let _entered = span.enter();
        let result = self.output(
            &[
                OsStr::new("fetch"),
                OsStr::new("--no-prune"),
                OsStr::new("--tags"),
                OsStr::new(remote),
            ],
            self.runner.fetch_timeout,
        );
        record(&span, &result);
        result.map(drop)
    }

    /// `git merge --ff-only --no-autostash <to>`. A refusal of git (an untracked file in the
    /// way, a change made since the decision) is a skip.
    fn fast_forward(&self, branch: String, from: String, to: &str) -> UpdateOutcome {
        let span = info_span!(
            "repo.fast_forward",
            branch = branch.as_str(),
            from = from.as_str(),
            to,
            outcome = Empty,
        );
        let _entered = span.enter();
        let result = self.output(
            &[
                OsStr::new("merge"),
                OsStr::new("--ff-only"),
                OsStr::new("--no-autostash"),
                OsStr::new(to),
            ],
            self.runner.merge_timeout,
        );
        record(&span, &result);
        match result {
            Ok(_) => UpdateOutcome::FastForwarded {
                branch,
                from,
                to: to.to_owned(),
            },
            Err(Failure::Exit { stderr_tail, .. }) => UpdateOutcome::Skipped {
                reason: SkipReason::MergeRefused { stderr_tail },
            },
            Err(failure) => failure.outcome(UpdateStep::FastForward),
        }
    }

    /// `git fetch --no-prune <remote> <merge>:refs/heads/<branch>`: git refuses anything but a
    /// fast-forward, and a branch checked out in any working tree.
    fn move_default_branch(
        &self,
        branch: String,
        remote: &str,
        merge: &str,
        from: String,
        to: String,
    ) -> UpdateOutcome {
        let span = info_span!(
            "repo.move_default_branch",
            branch = branch.as_str(),
            from = from.as_str(),
            to = to.as_str(),
            outcome = Empty,
        );
        let _entered = span.enter();
        let refspec = format!("{merge}:refs/heads/{branch}");
        let result = self.output(
            &[
                OsStr::new("fetch"),
                OsStr::new("--no-prune"),
                OsStr::new(remote),
                OsStr::new(&refspec),
            ],
            self.runner.fetch_timeout,
        );
        record(&span, &result);
        match result {
            Ok(_) => UpdateOutcome::DefaultBranchMoved { branch, from, to },
            Err(failure) => failure.outcome(UpdateStep::MoveDefaultBranch),
        }
    }

    /// Runs `git <args>` in the working tree; its standard output when it exits with 0.
    fn output(&self, args: &[&OsStr], timeout: Duration) -> Result<String, Failure> {
        let mut command = Command::new(&self.runner.program);
        command
            .current_dir(self.workdir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Its own group: stopped as a whole on timeout (ssh, hooks, credential helpers).
            .process_group(0);
        // The inherited environment first, then what loom enforces over it.
        for (name, value) in &self.runner.env {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        for name in REPOSITORY_VARIABLES {
            command.env_remove(name);
        }
        command.env("GIT_TERMINAL_PROMPT", "0");
        // git never looks above the working tree for a repository.
        if let Some(parent) = self.workdir.parent() {
            command.env("GIT_CEILING_DIRECTORIES", parent);
        }
        if let Some(ssh) = self.ssh_command {
            command.env("GIT_SSH_COMMAND", ssh);
        }
        let started = Instant::now();
        let child = command.spawn().map_err(Failure::Run)?;
        let result = wait(child, timeout, self.runner.kill_grace);
        debug!(
            args = ?args,
            duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            ok = result.is_ok(),
            "git ran"
        );
        result
    }
}

/// `GIT_SSH_COMMAND` for git: `BatchMode`, unless `core.sshCommand` is set or the environment
/// has an SSH command. Pure.
#[must_use]
pub fn batch_ssh_command(core_ssh_command: Option<&str>, ssh_in_env: bool) -> Option<&'static str> {
    let configured = core_ssh_command.is_some_and(|value| !value.trim().is_empty());
    (!configured && !ssh_in_env).then_some(BATCH_SSH_COMMAND)
}

fn record(span: &Span, result: &Result<String, Failure>) {
    let outcome = match result {
        Ok(_) => "ok",
        Err(Failure::Run(_)) => "error",
        Err(Failure::Timeout { .. }) => "timeout",
        Err(Failure::Exit { .. }) => "refused",
    };
    span.record("outcome", outcome);
}

/// Waits for `child` until `timeout`, then stops its process group: `SIGTERM` (git removes its
/// lock files), then `SIGKILL` after `grace`.
fn wait(mut child: Child, timeout: Duration, grace: Duration) -> Result<String, Failure> {
    let stdout = child.stdout.take().map(reader);
    let stderr = child.stderr.take().map(reader);
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                stop_group(&mut child, grace);
                // A process that left the group may keep the pipe open: do not wait for it.
                let stderr_tail = stderr
                    .map(|received| received.recv_timeout(grace).unwrap_or_default())
                    .map(|bytes| tail(&bytes))
                    .unwrap_or_default();
                return Err(Failure::Timeout {
                    after: timeout,
                    stderr_tail,
                });
            }
            Err(error) => {
                stop_group(&mut child, grace);
                return Err(Failure::Run(error));
            }
        }
    };
    // The process exited; a child it left behind may still hold the pipes.
    let collect = |received: Option<mpsc::Receiver<Vec<u8>>>| {
        received
            .and_then(|received| received.recv_timeout(grace).ok())
            .unwrap_or_default()
    };
    let out = collect(stdout);
    let err = collect(stderr);
    if status.success() {
        Ok(String::from_utf8_lossy(&out).into_owned())
    } else {
        Err(Failure::Exit {
            status,
            stderr_tail: tail(&err),
        })
    }
}

/// Reads `pipe` to its end on a thread; the bytes arrive on the channel.
fn reader(mut pipe: impl io::Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (send, received) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("git-output".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            let _ = send.send(bytes);
        });
    if let Err(error) = spawned {
        warn!(%error, "cannot start a thread to read git's output");
    }
    received
}

/// Sends `SIGTERM` to the process group of `child` (its id: it leads its own group), waits
/// `grace`, then `SIGKILL`, through the `kill` command (no `unsafe`).
fn stop_group(child: &mut Child, grace: Duration) {
    let group = format!("-{}", child.id());
    let signal = |name: &str| {
        let sent = Command::new("kill")
            .args([name, "--", group.as_str()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Err(error) = sent {
            warn!(%error, "cannot run kill");
        }
    };
    signal("-TERM");
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    // The leader may be gone and others of the group still running.
    signal("-KILL");
    let _ = child.kill();
    let _ = child.wait();
}

/// The last [`STDERR_TAIL_BYTES`] of `bytes`, as text, from the start of a line when possible.
fn tail(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(STDERR_TAIL_BYTES);
    let text = String::from_utf8_lossy(&bytes[start..]);
    let text = if start > 0 {
        text.split_once('\n')
            .map_or_else(|| text.as_ref(), |(_, rest)| rest)
    } else {
        text.as_ref()
    };
    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::{BATCH_SSH_COMMAND, STDERR_TAIL_BYTES, batch_ssh_command, tail};

    #[test]
    fn batch_mode_unless_the_user_chose_an_ssh_command() {
        assert_eq!(batch_ssh_command(None, false), Some(BATCH_SSH_COMMAND));
        assert_eq!(batch_ssh_command(Some(""), false), Some(BATCH_SSH_COMMAND));
        assert_eq!(batch_ssh_command(Some("ssh -i k"), false), None);
        assert_eq!(batch_ssh_command(None, true), None);
        assert_eq!(batch_ssh_command(Some("ssh -i k"), true), None);
    }

    #[test]
    fn stderr_tail_keeps_the_end_from_a_line_start() {
        assert_eq!(tail(b"fatal: x\n"), "fatal: x\n");
        let long = format!("{}\nlast line\n", "a".repeat(STDERR_TAIL_BYTES * 2));
        let kept = tail(long.as_bytes());
        assert!(kept.len() < STDERR_TAIL_BYTES);
        assert_eq!(kept, "last line\n");
    }
}
