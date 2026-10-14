//! `loom repo doctor`: the checks, decided without I/O from what `main.rs` read (configuration,
//! filesystem, loomd and its remote checks).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use loom_api::{
    BackupOutcome, BackupReport, BackupStep, CheckStatus, ContextBackup, ContextName,
    DaemonDiagnostics, DiskUsage, DoctorCheck, DoctorReport, RemoteCheck, RemoteCheckOutcome,
    RemoteCheckResult, RemoteProblem, RemoteScheme, RepoSummary, ServicePlatform, SkipReason,
    SshAgentSocket, UpdateOutcome, UpdateStep,
};
use loom_config::config::Config;
use loom_repo::{ContextRules, classify_remote_failure, remote_endpoint};
use loom_update::versions::{KEEP, to_prune};
use semver::Version;

use crate::repo::is_unassigned;
use crate::status::{disk_text, format_bytes};

/// `disk-wal` warns when `index.db-wal` is larger than this (64 MB). SQLite checkpoints the
/// WAL every 1,000 pages (about 4 MB) by default: a WAL this large means checkpoints are not
/// happening.
pub const WAL_WARN_BYTES: u64 = 64_000_000;

/// `disk-free` warns when less than this (1 GB) is free on the volume of the loom home.
pub const FREE_SPACE_WARN_BYTES: u64 = 1_000_000_000;

/// Most slugs named per group of the `updates` check.
const UPDATES_EXAMPLES: usize = 3;

fn check(name: impl Into<String>, status: CheckStatus, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        check: name.into(),
        status,
        detail: detail.into(),
        fix: None,
    }
}

fn with_fix(mut check: DoctorCheck, fix: impl Into<String>) -> DoctorCheck {
    check.fix = Some(fix.into());
    check
}

/// `path` with `home` replaced by `~`.
#[must_use]
pub fn tilde(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// Where a scan root comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootSource {
    /// `[repos] roots` (or its default).
    Roots,
    /// `match.paths` of this context.
    Context(ContextName),
}

/// A scan root: as written, its source, expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// As written in the configuration.
    pub written: String,
    /// Where it comes from.
    pub source: RootSource,
    /// `~` expanded.
    pub path: PathBuf,
}

/// The scan roots of `config`, in scan order, each directory once (as `loomd` walks them).
#[must_use]
pub fn roots(config: &Config, home: &Path) -> Vec<Root> {
    let roots = config.repos.roots.iter().map(|root| Root {
        written: root.as_str().to_owned(),
        source: RootSource::Roots,
        path: root.expand(home),
    });
    let paths = config.contexts.iter().flat_map(|(name, context)| {
        context.matching.paths.iter().map(|entry| Root {
            written: entry.get_ref().as_str().to_owned(),
            source: RootSource::Context(name.clone()),
            path: entry.get_ref().expand(home),
        })
    });
    let mut out: Vec<Root> = Vec::new();
    for root in roots.chain(paths) {
        if !out.iter().any(|seen| seen.path == root.path) {
            out.push(root);
        }
    }
    out
}

/// One `warn` per root that does not exist (`exists` tells), or one `ok`.
pub fn roots_checks(
    roots: &[Root],
    exists: impl Fn(&Path) -> bool,
    config: &Path,
) -> Vec<DoctorCheck> {
    let missing: Vec<DoctorCheck> = roots
        .iter()
        .filter(|root| !exists(&root.path))
        .map(|root| {
            let (detail, fix) = match &root.source {
                RootSource::Roots => (
                    format!("{} does not exist", root.written),
                    format!(
                        "create it, or set `[repos] roots` in {} to the directories that hold your repositories (default: [\"~/perso\", \"~/work\"])",
                        config.display()
                    ),
                ),
                RootSource::Context(context) => (
                    format!("{} (match.paths of {context}) does not exist", root.written),
                    format!(
                        "create it, or remove it from `[contexts.{context}] match.paths` in {}",
                        config.display()
                    ),
                ),
            };
            with_fix(check("roots", CheckStatus::Warn, detail), fix)
        })
        .collect();
    if missing.is_empty() {
        let detail = match roots.len() {
            0 => "no root to scan".to_owned(),
            1 => "1 root exists".to_owned(),
            count => format!("{count} roots exist"),
        };
        return vec![check("roots", CheckStatus::Ok, detail)];
    }
    missing
}

/// The `contexts` check: entries that loomd would refuse.
///
/// `match.paths` of different contexts that are the same directory once `~` is expanded, and
/// `[repos."<path>"]` keys naming the same repository, fail. Nested entries are valid (the
/// longest prefix wins) and are listed in the `ok` detail.
#[must_use]
pub fn contexts_check(config: &Config, home: &Path, config_path: &Path) -> DoctorCheck {
    let entries: Vec<(&ContextName, &str, PathBuf)> =
        config
            .contexts
            .iter()
            .flat_map(|(name, context)| {
                context.matching.paths.iter().map(move |entry| {
                    (name, entry.get_ref().as_str(), entry.get_ref().expand(home))
                })
            })
            .collect();
    let mut ties = Vec::new();
    let mut nested = Vec::new();
    for (index, (a_context, a_written, a_path)) in entries.iter().enumerate() {
        for (b_context, b_written, b_path) in &entries[index + 1..] {
            if a_context == b_context {
                continue;
            }
            if a_path == b_path {
                ties.push(format!(
                    "`{a_written}` ({a_context}) and `{b_written}` ({b_context}) are the same directory {}",
                    a_path.display()
                ));
            } else if a_path.starts_with(b_path) {
                nested.push(format!(
                    "{a_written} ({a_context}) inside {b_written} ({b_context})"
                ));
            } else if b_path.starts_with(a_path) {
                nested.push(format!(
                    "{b_written} ({b_context}) inside {a_written} ({a_context})"
                ));
            }
        }
    }
    if !ties.is_empty() {
        return with_fix(
            check(
                "contexts",
                CheckStatus::Fail,
                format!(
                    "match.paths of different contexts overlap: {}; loomd refuses this configuration",
                    ties.join("; ")
                ),
            ),
            format!(
                "keep each directory in the match.paths of one context only, in {}, then restart loomd",
                config_path.display()
            ),
        );
    }
    if let Err(error) = ContextRules::new(config, home) {
        return with_fix(
            check("contexts", CheckStatus::Fail, error.to_string()),
            format!(
                "keep one table per repository in {}, then restart loomd",
                config_path.display()
            ),
        );
    }
    let count = config.contexts.len();
    let mut detail = match count {
        0 => "no context defined".to_owned(),
        1 => "1 context, no overlapping match.paths".to_owned(),
        count => format!("{count} contexts, no overlapping match.paths"),
    };
    if !nested.is_empty() {
        let _ = write!(
            detail,
            "; nested, the longest prefix wins: {}",
            nested.join(", ")
        );
    }
    check("contexts", CheckStatus::Ok, detail)
}

/// Repositories without a context: their count, an example, and the configuration that
/// would assign them.
#[must_use]
pub fn unassigned_check(repos: &[RepoSummary], home: Option<&Path>, config: &Path) -> DoctorCheck {
    let unassigned: Vec<&RepoSummary> = repos.iter().filter(|repo| is_unassigned(repo)).collect();
    let Some(example) = unassigned.first() else {
        return check(
            "unassigned",
            CheckStatus::Ok,
            format!("every repository has a context ({} indexed)", repos.len()),
        );
    };
    let count = unassigned.len();
    let detail = if count == 1 {
        format!(
            "1 repository has no context: {}",
            example.identity.path.display()
        )
    } else {
        format!(
            "{count} repositories have no context, such as {}",
            example.identity.path.display()
        )
    };
    let parent = example
        .identity
        .path
        .parent()
        .unwrap_or(&example.identity.path);
    with_fix(
        check("unassigned", CheckStatus::Warn, detail),
        format!(
            "add a context that matches them to {}, then restart loomd:\n  [contexts.<name>]\n  match.paths = [\"{}\"]\nor match their remotes with `match.hosts`, or set `default_context = \"<name>\"`",
            config.display(),
            tilde(parent, home)
        ),
    )
}

/// `SSH_AUTH_SOCK` in loomd's environment. A missing agent fails when an indexed remote uses
/// SSH, and only warns otherwise.
#[must_use]
pub fn agent_check(
    diagnostics: &DaemonDiagnostics,
    platform: Option<ServicePlatform>,
    ssh_remotes: bool,
) -> DoctorCheck {
    let problem = match &diagnostics.ssh_auth_sock {
        SshAgentSocket::Socket { path } => {
            return check(
                "ssh-agent",
                CheckStatus::Ok,
                format!("loomd sees an agent socket at {}", path.display()),
            );
        }
        SshAgentSocket::Unset => "SSH_AUTH_SOCK is not set in loomd's environment".to_owned(),
        SshAgentSocket::Missing { path } => format!(
            "SSH_AUTH_SOCK of loomd is {}, which does not exist (an agent from a closed session?)",
            path.display()
        ),
        SshAgentSocket::NotASocket { path } => format!(
            "SSH_AUTH_SOCK of loomd is {}, which is not a socket",
            path.display()
        ),
    };
    let (status, consequence) = if ssh_remotes {
        (CheckStatus::Fail, "loomd cannot fetch the SSH remotes")
    } else {
        (CheckStatus::Warn, "no indexed remote uses SSH yet")
    };
    let fix = match platform {
        Some(ServicePlatform::Linux) => {
            "from a shell where `ssh-add -l` works, give the agent socket to the systemd user manager, then restart loomd:\n  systemctl --user import-environment SSH_AUTH_SOCK\n  systemctl --user restart loomd.service\nan agent with a fixed socket (such as the ssh-agent.socket user unit) survives new sessions"
        }
        Some(ServicePlatform::Macos) => {
            "loomd gets SSH_AUTH_SOCK from the launchd session; restart it:\n  launchctl kickstart -k gui/$(id -u)/io.github.gsuquet.loomd\nwhen loomd was not started by the service, stop it and run `loom status` from a shell where `ssh-add -l` works"
        }
        None => "start loomd from a shell where `ssh-add -l` works",
    };
    with_fix(
        check("ssh-agent", status, format!("{problem}: {consequence}")),
        fix,
    )
}

/// `HOME` of loomd against the one of this shell: `~` in the configuration expands to it.
#[must_use]
pub fn home_check(daemon: Option<&Path>, local: Option<&Path>) -> DoctorCheck {
    match (daemon, local) {
        (Some(daemon), Some(local)) if daemon == local => check(
            "home",
            CheckStatus::Ok,
            format!("loomd expands `~` to {}", daemon.display()),
        ),
        (Some(daemon), local) => with_fix(
            check(
                "home",
                CheckStatus::Warn,
                format!(
                    "loomd expands `~` to {}, this shell to {}",
                    daemon.display(),
                    local.map_or_else(
                        || "nothing (HOME unset)".to_owned(),
                        |local| local.display().to_string()
                    )
                ),
            ),
            "run loomd with the same HOME as your shell (restart it, or fix the environment of the service)",
        ),
        (None, _) => with_fix(
            check(
                "home",
                CheckStatus::Warn,
                "HOME is not set in loomd's environment",
            ),
            "set HOME in the environment of loomd, then restart it",
        ),
    }
}

/// Whether the primary remote of a repository of `repos` uses SSH: loomd then needs an agent
/// (or a configured SSH command) to fetch it.
#[must_use]
pub fn uses_ssh(repos: &[RepoSummary]) -> bool {
    repos.iter().any(|repo| {
        let identity = &repo.identity;
        identity
            .primary_remote
            .as_deref()
            .and_then(|name| identity.remotes.iter().find(|remote| remote.name == name))
            .is_some_and(|remote| remote_endpoint(&remote.url).scheme == RemoteScheme::Ssh)
    })
}

/// Lines git adds after the error of a remote, lowercase: they never say what went wrong.
const GIT_BOILERPLATE: [&str; 3] = [
    "fatal: could not read from remote repository.",
    "please make sure you have the correct access rights",
    "and the repository exists.",
];

/// The last non-empty line of git's error output `text` that is not
/// [`GIT_BOILERPLATE`], trimmed; the last non-empty line when every line is.
fn last_line(text: &str) -> Option<&str> {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let last = lines.clone().next_back();
    lines
        .rfind(|line| !GIT_BOILERPLATE.contains(&line.to_ascii_lowercase().as_str()))
        .or(last)
}

/// One check per remote endpoint loomd checked: `ok` when it answered, `fail` with the last
/// line of git's error and the fix of its kind of failure otherwise.
#[must_use]
pub fn remote_checks(result: &RemoteCheckResult) -> Vec<DoctorCheck> {
    if result.remotes.is_empty() {
        return vec![check(
            "remote",
            CheckStatus::Ok,
            "no indexed repository has a remote",
        )];
    }
    result.remotes.iter().map(remote_check).collect()
}

fn remote_check(remote: &RemoteCheck) -> DoctorCheck {
    let name = format!(
        "remote {} ({}, via {})",
        remote.endpoint, remote.scheme, remote.via
    );
    match &remote.result {
        RemoteCheckOutcome::Reachable => check(name, CheckStatus::Ok, "reachable"),
        RemoteCheckOutcome::Failed {
            problem,
            error,
            stderr_tail,
        } => {
            let detail = match (last_line(stderr_tail), *problem) {
                // A timeout says more than the last line git wrote before it.
                (_, RemoteProblem::Network) if stderr_tail.trim().is_empty() => error.clone(),
                (Some(line), _) => line.to_owned(),
                (None, _) => error.clone(),
            };
            with_fix(
                check(name, CheckStatus::Fail, detail),
                remote_fix(*problem, remote),
            )
        }
    }
}

/// How to fix a remote loomd's git cannot reach.
fn remote_fix(problem: RemoteProblem, remote: &RemoteCheck) -> String {
    let RemoteCheck {
        endpoint,
        via,
        path,
        remote,
        ..
    } = remote;
    let path = path.display();
    let try_it = format!("git -C {path} ls-remote {remote} HEAD");
    match problem {
        RemoteProblem::SshAuth => format!(
            "SSH refused loomd's git: load the key in the agent loomd sees (`ssh-add`; see the ssh-agent check), or give git the key with `core.sshCommand` (per remote: `includeIf \"hasconfig:remote.*.url:…\"`); a new or changed host key: run `{try_it}` once in a terminal"
        ),
        RemoteProblem::HttpsAuth => format!(
            "git has no credential for {endpoint} and cannot prompt: store one with a credential helper (run `{try_it}` once in a terminal with a helper configured), or switch the remote to SSH (`git -C {path} remote set-url {remote} <ssh url>`)"
        ),
        RemoteProblem::NotFound => format!(
            "the project was moved or deleted upstream, or access to it was lost: update the remote (`git -C {path} remote set-url {remote} <new url>`), or forget the clone (`loom repo forget {via}`)"
        ),
        RemoteProblem::Network => format!(
            "{endpoint} cannot be reached from loomd: check the network or the VPN, then run `loom repo doctor` again"
        ),
        RemoteProblem::Other => format!("run `{try_it}` in a terminal to see the whole error"),
    }
}

/// Why the last update of a repository did not complete: one group of the `updates` check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum UpdateIssue {
    Fetch(RemoteProblem),
    FastForwardFailed,
    MoveDefaultBranchFailed,
    Unreadable,
    Busy,
    Unknown,
    Operation,
    Dirty,
    Detached,
    Unborn,
    NoUpstream,
    Diverged,
    MergeRefused,
    DefaultBranchDiverged,
}

impl UpdateIssue {
    /// The issue an outcome reports; `None` for a success or a skip asked for (policy `off`,
    /// no remote).
    fn of(outcome: &UpdateOutcome) -> Option<Self> {
        match outcome {
            UpdateOutcome::Fetched
            | UpdateOutcome::FastForwarded { .. }
            | UpdateOutcome::DefaultBranchMoved { .. } => None,
            UpdateOutcome::Failed {
                step, stderr_tail, ..
            } => Some(match step {
                UpdateStep::Fetch => Self::Fetch(classify_remote_failure(stderr_tail)),
                UpdateStep::FastForward => Self::FastForwardFailed,
                UpdateStep::MoveDefaultBranch => Self::MoveDefaultBranchFailed,
            }),
            UpdateOutcome::Skipped { reason } => match reason {
                SkipReason::PolicyOff | SkipReason::NoRemote => None,
                SkipReason::Unreadable { .. } => Some(Self::Unreadable),
                SkipReason::Busy { .. } => Some(Self::Busy),
                SkipReason::Unknown { .. } => Some(Self::Unknown),
                SkipReason::Operation { .. } => Some(Self::Operation),
                SkipReason::Dirty { .. } => Some(Self::Dirty),
                SkipReason::Detached => Some(Self::Detached),
                SkipReason::Unborn { .. } => Some(Self::Unborn),
                SkipReason::NoUpstream { .. } => Some(Self::NoUpstream),
                SkipReason::Diverged { .. } => Some(Self::Diverged),
                SkipReason::MergeRefused { .. } => Some(Self::MergeRefused),
                SkipReason::DefaultBranchDiverged { .. } => Some(Self::DefaultBranchDiverged),
            },
        }
    }

    /// The group, as printed, and how to fix it.
    const fn text(self) -> (&'static str, &'static str) {
        match self {
            Self::Fetch(RemoteProblem::SshAuth) => (
                "fetch refused by SSH",
                "loomd's git cannot authenticate: see the `remote` check of the host",
            ),
            Self::Fetch(RemoteProblem::HttpsAuth) => (
                "fetch without HTTPS credential",
                "store a credential for the host, or switch these remotes to SSH: see the `remote` check of the host",
            ),
            Self::Fetch(RemoteProblem::NotFound) => (
                "remote not found",
                "the project was moved or deleted upstream, or access to it was lost: update the remote (`git remote set-url`), or forget the clone (`loom repo forget <slug>`)",
            ),
            Self::Fetch(RemoteProblem::Network) => (
                "remote unreachable",
                "check the network or the VPN; loomd tries again, less often while the host stays down",
            ),
            Self::Fetch(RemoteProblem::Other) => (
                "fetch failed",
                "`loom repo show <slug>` shows the end of git's error; `git fetch` in the repository shows all of it",
            ),
            Self::FastForwardFailed => (
                "fast-forward failed",
                "`loom repo show <slug>` shows git's error; `git status` in the repository shows its state",
            ),
            Self::MoveDefaultBranchFailed => (
                "default branch move failed",
                "`loom repo show <slug>` shows git's error",
            ),
            Self::Unreadable => (
                "repository unreadable",
                "check its `.git`, or forget it (`loom repo forget <slug>`)",
            ),
            Self::Busy => (
                "repository busy",
                "another git process held a lock: nothing to do, the next update tries again; if it lasts, check that no git process runs there (a crashed one leaves its lock file behind)",
            ),
            Self::Unknown => (
                "state unknown",
                "the state could not be read in time, or a file needs a filter driver: `loom repo show --refresh <slug>` and `git status`",
            ),
            Self::Operation => (
                "operation in progress",
                "finish or abort it (`git rebase --continue` or `--abort`, and so on)",
            ),
            Self::Dirty => (
                "local changes",
                "commit, stash or discard them, then `loom repo sync <slug>`",
            ),
            Self::Detached => ("HEAD detached", "check out a branch"),
            Self::Unborn => ("branch without commit", "commit, or check out a branch"),
            Self::NoUpstream => (
                "branch without upstream",
                "`git branch --set-upstream-to <remote>/<branch>`, or leave it: a local branch is never moved",
            ),
            Self::Diverged => ("diverged from upstream", "rebase or merge by hand"),
            Self::MergeRefused => (
                "fast-forward refused by git",
                "usually an untracked file the remote now tracks: move it aside",
            ),
            Self::DefaultBranchDiverged => (
                "default branch diverged",
                "rebase or merge the default branch by hand",
            ),
        }
    }
}

/// The `updates` check: the repositories whose last update failed or was skipped.
///
/// One `warn` per reason with its count, up to [`UPDATES_EXAMPLES`] slugs and its fix; one
/// `ok` when there are none. A skip asked for (policy `off`, no remote) is not counted.
#[must_use]
pub fn updates_checks(repos: &[RepoSummary]) -> Vec<DoctorCheck> {
    let mut groups: BTreeMap<UpdateIssue, BTreeSet<&str>> = BTreeMap::new();
    let mut updated = 0_usize;
    for repo in repos {
        let Some(update) = &repo.last_update else {
            continue;
        };
        updated += 1;
        for issue in update.outcomes.iter().filter_map(UpdateIssue::of) {
            groups
                .entry(issue)
                .or_default()
                .insert(repo.identity.slug.as_str());
        }
    }
    if groups.is_empty() {
        let detail = match updated {
            0 => "no repository updated yet".to_owned(),
            1 => "the last update of 1 repository went through".to_owned(),
            count => format!("the last update of all {count} repositories went through"),
        };
        return vec![check("updates", CheckStatus::Ok, detail)];
    }
    groups
        .into_iter()
        .map(|(issue, slugs)| {
            let (label, fix) = issue.text();
            let mut examples: Vec<&str> = slugs.iter().copied().take(UPDATES_EXAMPLES).collect();
            if slugs.len() > UPDATES_EXAMPLES {
                examples.push("…");
            }
            with_fix(
                check(
                    "updates",
                    CheckStatus::Warn,
                    format!("{label} ({}): {}", slugs.len(), examples.join(", ")),
                ),
                fix,
            )
        })
        .collect()
}

/// The disk checks of the loom home `home`, from the usage measured by loomd `running`.
///
/// `disk` (total, parts that could not be measured), `disk-versions` (previous versions kept
/// beyond what `loom rollback` needs), `disk-wal` ([`WAL_WARN_BYTES`]) and `disk-free`
/// ([`FREE_SPACE_WARN_BYTES`], absent when the free space is unknown).
#[must_use]
pub fn disk_checks(disk: &DiskUsage, home: &Path, running: &str) -> Vec<DoctorCheck> {
    let summary = format!("{} in {}", disk_text(disk), home.display());
    let usage = if disk.warnings.is_empty() {
        check("disk", CheckStatus::Ok, summary)
    } else {
        with_fix(
            check(
                "disk",
                CheckStatus::Warn,
                format!("{summary}; counted as 0: {}", disk.warnings.join("; ")),
            ),
            "make these paths readable by the user running loomd",
        )
    };
    let mut checks = vec![usage, versions_check(disk, home, running)];
    checks.push(if disk.index_wal_bytes > WAL_WARN_BYTES {
        with_fix(
            check(
                "disk-wal",
                CheckStatus::Warn,
                format!(
                    "index.db-wal is {}, above {}",
                    format_bytes(disk.index_wal_bytes),
                    format_bytes(WAL_WARN_BYTES)
                ),
            ),
            "restart loomd: SQLite copies the WAL into index.db and removes it when loomd closes the index",
        )
    } else {
        check(
            "disk-wal",
            CheckStatus::Ok,
            format!(
                "index.db-wal is {}, below {}",
                format_bytes(disk.index_wal_bytes),
                format_bytes(WAL_WARN_BYTES)
            ),
        )
    });
    if let Some(available) = disk.available_bytes {
        let free = format!(
            "{} free on the volume of {}",
            format_bytes(available),
            home.display()
        );
        checks.push(if available < FREE_SPACE_WARN_BYTES {
            with_fix(
                check(
                    "disk-free",
                    CheckStatus::Warn,
                    format!("{free}, below {}", format_bytes(FREE_SPACE_WARN_BYTES)),
                ),
                "free space on that volume: loomd writes its index, logs and upgrade downloads in the loom home",
            )
        } else {
            check("disk-free", CheckStatus::Ok, free)
        });
    }
    checks
}

/// `disk-versions`: the saved versions `loom rollback` does not need, by the retention rule of
/// the upgrades (`loom_update::versions::to_prune`); by count alone when the running version
/// is not a version. Directories not named after a version are left out.
fn versions_check(disk: &DiskUsage, home: &Path, running: &str) -> DoctorCheck {
    let saved: Vec<(&str, Version)> = disk
        .kept_versions
        .iter()
        .filter_map(|name| Some((name.as_str(), Version::parse(name).ok()?)))
        .collect();
    if saved.is_empty() {
        return check("disk-versions", CheckStatus::Ok, "no previous version kept");
    }
    let versions: Vec<Version> = saved.iter().map(|(_, version)| version.clone()).collect();
    let mut doomed = Version::parse(running).map_or_else(
        |_| {
            let mut sorted = versions.clone();
            sorted.sort_by(|a, b| b.cmp_precedence(a));
            sorted.split_off(KEEP.min(sorted.len()))
        },
        |installed| to_prune(&versions, &installed, None, KEEP),
    );
    doomed.sort_by(Version::cmp_precedence);
    let kept = format!(
        "{} previous version{} kept ({})",
        saved.len(),
        if saved.len() == 1 { "" } else { "s" },
        format_bytes(disk.versions_bytes)
    );
    if doomed.is_empty() {
        return check(
            "disk-versions",
            CheckStatus::Ok,
            format!("{kept}: rollback needs at most {KEEP}"),
        );
    }
    let names: Vec<&str> = doomed
        .iter()
        .filter_map(|version| {
            saved
                .iter()
                .find(|(_, saved)| saved.cmp_precedence(version).is_eq())
                .map(|(name, _)| *name)
        })
        .collect();
    let paths: Vec<String> = names
        .iter()
        .map(|name| home.join("versions").join(name).display().to_string())
        .collect();
    let (verb, pronoun) = if names.len() == 1 {
        ("is", "it")
    } else {
        ("are", "them")
    };
    with_fix(
        check(
            "disk-versions",
            CheckStatus::Warn,
            format!(
                "{kept}, rollback needs at most {KEEP}: {} {verb} not needed",
                names.join(", ")
            ),
        ),
        format!(
            "delete {pronoun} (`rm -r {}`), or let the next upgrade or rollback prune {pronoun}",
            paths.join(" ")
        ),
    )
}

/// Human output: one `ok|warn|fail  <check>: <detail>` line per check, each fix below its
/// check, indented.
#[must_use]
pub fn render(report: &DoctorReport) -> String {
    let mut text = String::new();
    for check in &report.checks {
        let _ = writeln!(
            text,
            "{:<4}  {}: {}",
            check.status, check.check, check.detail
        );
        if let Some(fix) = &check.fix {
            for (index, line) in fix.lines().enumerate() {
                let label = if index == 0 { "fix: " } else { "     " };
                let _ = writeln!(text, "      {label}{line}");
            }
        }
    }
    text
}

/// The `machine` check and one `backup <context>` check per store held here: a divergence or a
/// refused push fails, a failed step or uncommitted changes warn.
#[must_use]
pub fn backup_checks(report: &BackupReport, home: Option<&Path>) -> Vec<DoctorCheck> {
    let mut detail = format!("this machine is `{}`", report.machine);
    if !report.not_held.is_empty() {
        let names: Vec<&str> = report.not_held.iter().map(ContextName::as_str).collect();
        let _ = write!(
            detail,
            "; contexts held elsewhere, not loaded here: {}",
            names.join(", ")
        );
    }
    let mut checks = vec![check("machine", CheckStatus::Ok, detail)];
    checks.extend(
        report
            .contexts
            .iter()
            .map(|context| backup_check(context, home)),
    );
    checks
}

fn backup_check(context: &ContextBackup, home: Option<&Path>) -> DoctorCheck {
    let name = format!("backup {}", context.context);
    let path = tilde(&context.path, home);
    let Some(remote) = &context.remote else {
        return match &context.uncommitted {
            Some(error) => uncommitted(&name, error),
            None => check(
                name,
                CheckStatus::Ok,
                format!("{path}: commits stay local, no store.backup.remote"),
            ),
        };
    };
    let Some(sync) = &context.last_sync else {
        return match &context.uncommitted {
            Some(error) => uncommitted(&name, error),
            None => check(
                name,
                CheckStatus::Ok,
                format!("{path}: not synced with {remote} since loomd started"),
            ),
        };
    };
    let detail = format!("{path} and {remote}: {}", sync.outcome);
    match &sync.outcome {
        BackupOutcome::UpToDate | BackupOutcome::Pushed { .. } | BackupOutcome::Pulled { .. } => {
            match &context.uncommitted {
                Some(error) => uncommitted(&name, error),
                None => check(name, CheckStatus::Ok, detail),
            }
        }
        BackupOutcome::Diverged { .. } => with_fix(
            check(name, CheckStatus::Fail, detail),
            format!(
                "loom never merges a store: in {path}, `git fetch backup`, then merge or rebase onto backup/main by hand, then `loom sync -c {}`",
                context.context
            ),
        ),
        BackupOutcome::PushRejected { .. } => with_fix(
            check(name, CheckStatus::Fail, detail),
            format!(
                "the commits are kept in {path}: check that {remote} accepts pushes to main (access, protected branch, hooks), then `loom sync -c {}`",
                context.context
            ),
        ),
        BackupOutcome::Failed { step, .. } => {
            let fix = match step {
                BackupStep::Commit | BackupStep::FastForward => format!(
                    "commit the changes of {path} by hand (`git status` there shows them; check the git identity and a leftover `.git/index.lock`), then `loom sync -c {}`",
                    context.context
                ),
                BackupStep::Remote | BackupStep::Fetch | BackupStep::Compare | BackupStep::Push => {
                    format!(
                        "check that loomd reaches {remote} (network, SSH agent, credentials), then `loom sync -c {}`",
                        context.context
                    )
                }
            };
            with_fix(check(name, CheckStatus::Warn, detail), fix)
        }
    }
}

fn uncommitted(name: &str, error: &str) -> DoctorCheck {
    with_fix(
        check(
            name,
            CheckStatus::Warn,
            format!("changes not committed: {error}"),
        ),
        "fix the cause (git identity, a lock held by another git process); the next change or `loom sync` commits them",
    )
}

#[cfg(test)]
mod backup_tests {
    use loom_api::{
        BackupOutcome, BackupReport, BackupStep, BackupSync, CheckStatus, ContextBackup, Timestamp,
    };

    use super::backup_checks;

    fn context(outcome: Option<BackupOutcome>, uncommitted: Option<&str>) -> ContextBackup {
        ContextBackup {
            context: "work".parse().unwrap(),
            path: "/home/me/.loom/contexts/work".into(),
            remote: Some("/srv/work.git".to_owned()),
            uncommitted: uncommitted.map(str::to_owned),
            last_sync: outcome.map(|outcome| BackupSync {
                time: Timestamp::EPOCH,
                outcome,
            }),
        }
    }

    fn checks(contexts: Vec<ContextBackup>) -> Vec<super::DoctorCheck> {
        backup_checks(
            &BackupReport {
                machine: "desk".to_owned(),
                contexts,
                not_held: vec!["lab".parse().unwrap()],
            },
            Some(std::path::Path::new("/home/me")),
        )
    }

    #[test]
    fn the_machine_and_the_contexts_held_elsewhere_are_named() {
        let checks = checks(Vec::new());
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].check, "machine");
        assert_eq!(
            checks[0].detail,
            "this machine is `desk`; contexts held elsewhere, not loaded here: lab"
        );
    }

    #[test]
    fn a_divergence_fails_and_names_the_context() {
        let checks = checks(vec![context(
            Some(BackupOutcome::Diverged {
                local: 1,
                remote: 2,
            }),
            None,
        )]);
        let backup = &checks[1];
        assert_eq!(backup.check, "backup work");
        assert_eq!(backup.status, CheckStatus::Fail);
        assert_eq!(
            backup.detail,
            "~/.loom/contexts/work and /srv/work.git: diverged: 1 local and 2 remote commits (nothing merged)"
        );
        assert!(
            backup
                .fix
                .as_deref()
                .unwrap()
                .contains("`loom sync -c work`"),
            "{backup:?}"
        );
    }

    #[test]
    fn a_refused_push_fails_and_other_problems_warn() {
        let status = |outcome, uncommitted| checks(vec![context(outcome, uncommitted)])[1].status;
        assert_eq!(
            status(
                Some(BackupOutcome::PushRejected {
                    stderr_tail: String::new()
                }),
                None
            ),
            CheckStatus::Fail
        );
        assert_eq!(
            status(
                Some(BackupOutcome::Failed {
                    step: BackupStep::Fetch,
                    error: "exit status: 128".to_owned(),
                    stderr_tail: String::new(),
                }),
                None
            ),
            CheckStatus::Warn
        );
        assert_eq!(
            status(
                Some(BackupOutcome::Pushed { commits: 1 }),
                Some("git commit failed")
            ),
            CheckStatus::Warn
        );
        assert_eq!(
            status(Some(BackupOutcome::Pushed { commits: 1 }), None),
            CheckStatus::Ok
        );
        assert_eq!(status(None, None), CheckStatus::Ok);
        let mut local = context(None, None);
        local.remote = None;
        let check = &checks(vec![local])[1];
        assert_eq!(check.status, CheckStatus::Ok);
        assert!(check.detail.contains("commits stay local"), "{check:?}");
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use loom_api::{
        CheckStatus, ContextAssignment, DaemonDiagnostics, DiskUsage, DoctorCheck, DoctorReport,
        LastUpdate, Remote, RemoteCheck, RemoteCheckOutcome, RemoteCheckResult, RemoteProblem,
        RemoteScheme, RepoIdentity, RepoSummary, RepoUpdatePolicy, ServicePlatform, SkipReason,
        SshAgentSocket, Timestamp, UpdateOutcome, UpdateStep,
    };
    use loom_config::config::parse_config;

    use super::{
        FREE_SPACE_WARN_BYTES, WAL_WARN_BYTES, agent_check, contexts_check, disk_checks,
        home_check, remote_checks, render, roots, roots_checks, tilde, unassigned_check,
        updates_checks, uses_ssh,
    };

    const HOME: &str = "/home/me";
    const CONFIG: &str = "/home/me/.loom/config.toml";

    fn config(text: &str) -> loom_config::config::Config {
        parse_config(text, Path::new("config.toml")).unwrap()
    }

    #[test]
    fn roots_are_listed_once_with_their_source() {
        let config = config(
            "[repos]\nroots = [\"~/src\", \"/srv\"]\n[contexts.a]\nmatch.paths = [\"~/src\", \"~/a\"]\n",
        );
        let found = roots(&config, Path::new(HOME));
        let written: Vec<&str> = found.iter().map(|root| root.written.as_str()).collect();
        assert_eq!(written, ["~/src", "/srv", "~/a"]);
        assert_eq!(found[2].path, PathBuf::from("/home/me/a"));

        let checks = roots_checks(
            &found,
            |path| path != Path::new("/home/me/a"),
            Path::new(CONFIG),
        );
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, CheckStatus::Warn);
        assert_eq!(checks[0].detail, "~/a (match.paths of a) does not exist");
        assert!(
            checks[0]
                .fix
                .as_deref()
                .unwrap()
                .contains("[contexts.a] match.paths")
        );

        let checks = roots_checks(&found, |_| true, Path::new(CONFIG));
        assert_eq!(checks[0].status, CheckStatus::Ok);
        assert_eq!(checks[0].detail, "3 roots exist");
    }

    #[test]
    fn contexts_overlap_only_when_they_are_the_same_directory() {
        let tie = config(
            "[contexts.a]\nmatch.paths = [\"~/work\"]\n[contexts.b]\nmatch.paths = [\"/home/me/work/\"]\n",
        );
        let result = contexts_check(&tie, Path::new(HOME), Path::new(CONFIG));
        assert_eq!(result.status, CheckStatus::Fail);
        assert!(
            result.detail.contains(
                "`~/work` (a) and `/home/me/work/` (b) are the same directory /home/me/work"
            ),
            "{}",
            result.detail
        );
        // Another home: no tie.
        assert_eq!(
            contexts_check(&tie, Path::new("/Users/me"), Path::new(CONFIG)).status,
            CheckStatus::Ok
        );

        let nested = config(
            "[contexts.a]\nmatch.paths = [\"~/work\"]\n[contexts.b]\nmatch.paths = [\"~/work/b\"]\n",
        );
        let result = contexts_check(&nested, Path::new(HOME), Path::new(CONFIG));
        assert_eq!(result.status, CheckStatus::Ok);
        assert_eq!(
            result.detail,
            "2 contexts, no overlapping match.paths; nested, the longest prefix wins: ~/work/b (b) inside ~/work (a)"
        );

        let overrides =
            config("[repos.\"~/x\"]\nupdate = \"off\"\n[repos.\"/home/me/x\"]\nupdate = \"ff\"\n");
        let result = contexts_check(&overrides, Path::new(HOME), Path::new(CONFIG));
        assert_eq!(result.status, CheckStatus::Fail);

        assert_eq!(
            contexts_check(&config(""), Path::new(HOME), Path::new(CONFIG)).detail,
            "no context defined"
        );
    }

    fn repo(path: &str, context: ContextAssignment, urls: &[&str]) -> RepoSummary {
        RepoSummary {
            identity: RepoIdentity {
                path: PathBuf::from(path),
                name: path.rsplit('/').next().unwrap().to_owned(),
                slug: format!("{}/x", context.context_name()),
                context,
                remotes: urls
                    .iter()
                    .map(|url| Remote {
                        name: "origin".to_owned(),
                        url: (*url).to_owned(),
                        location: None,
                    })
                    .collect(),
                primary_remote: None,
                forge: None,
            },
            registered: false,
            last_used: None,
            state: None,
            last_update: None,
        }
    }

    #[test]
    fn unassigned_repositories_get_a_snippet() {
        let assigned = ContextAssignment::Default {
            context: "personal".parse().unwrap(),
        };
        let repos = vec![
            repo("/home/me/perso/a", assigned, &[]),
            repo("/home/me/src/tool", ContextAssignment::Unassigned, &[]),
            repo("/home/me/src/other", ContextAssignment::Unassigned, &[]),
        ];
        let result = unassigned_check(&repos, Some(Path::new(HOME)), Path::new(CONFIG));
        assert_eq!(result.status, CheckStatus::Warn);
        assert_eq!(
            result.detail,
            "2 repositories have no context, such as /home/me/src/tool"
        );
        let fix = result.fix.unwrap();
        assert!(
            fix.contains("[contexts.<name>]\n  match.paths = [\"~/src\"]"),
            "{fix}"
        );
        assert!(fix.contains(CONFIG));

        let result = unassigned_check(&repos[..1], None, Path::new(CONFIG));
        assert_eq!(result.status, CheckStatus::Ok);
        assert_eq!(result.detail, "every repository has a context (1 indexed)");
    }

    #[test]
    fn agent_states_and_platform_fixes() {
        let diagnostics = |ssh_auth_sock| DaemonDiagnostics {
            ssh_auth_sock,
            home: None,
        };
        let ok = agent_check(
            &diagnostics(SshAgentSocket::Socket {
                path: "/run/agent".into(),
            }),
            Some(ServicePlatform::Linux),
            true,
        );
        assert_eq!(ok.status, CheckStatus::Ok);
        assert_eq!(ok.fix, None);

        let unset = diagnostics(SshAgentSocket::Unset);
        let linux = agent_check(&unset, Some(ServicePlatform::Linux), true);
        assert_eq!(linux.status, CheckStatus::Fail);
        assert!(
            linux
                .fix
                .unwrap()
                .contains("systemctl --user import-environment SSH_AUTH_SOCK")
        );
        let macos = agent_check(&unset, Some(ServicePlatform::Macos), false);
        assert_eq!(macos.status, CheckStatus::Warn);
        assert!(macos.fix.unwrap().contains("launchctl kickstart -k"));

        let stale = agent_check(
            &diagnostics(SshAgentSocket::Missing {
                path: "/tmp/gone".into(),
            }),
            None,
            true,
        );
        assert!(stale.detail.contains("/tmp/gone, which does not exist"));
    }

    #[test]
    fn home_mismatch_warns() {
        let home = Path::new(HOME);
        assert_eq!(home_check(Some(home), Some(home)).status, CheckStatus::Ok);
        let other = home_check(Some(home), Some(Path::new("/Users/me")));
        assert_eq!(other.status, CheckStatus::Warn);
        assert_eq!(
            other.detail,
            "loomd expands `~` to /home/me, this shell to /Users/me"
        );
        assert_eq!(home_check(None, Some(home)).status, CheckStatus::Warn);
    }

    #[test]
    fn ssh_is_used_when_a_primary_remote_uses_it() {
        let mut https = repo(
            "/a",
            ContextAssignment::Unassigned,
            &["https://github.com/a/b.git"],
        );
        https.identity.primary_remote = Some("origin".to_owned());
        assert!(!uses_ssh(std::slice::from_ref(&https)));
        let mut ssh = repo(
            "/b",
            ContextAssignment::Unassigned,
            &["git@github.com:c/d.git"],
        );
        ssh.identity.primary_remote = Some("origin".to_owned());
        assert!(uses_ssh(&[https, ssh]));
        assert!(!uses_ssh(&[repo("/c", ContextAssignment::Unassigned, &[])]));
    }

    fn remote(endpoint: &str, scheme: RemoteScheme, result: RemoteCheckOutcome) -> RemoteCheck {
        RemoteCheck {
            endpoint: endpoint.to_owned(),
            scheme,
            via: "personal/loom".to_owned(),
            path: PathBuf::from("/home/me/perso/loom"),
            remote: "origin".to_owned(),
            result,
        }
    }

    fn failed(problem: RemoteProblem, error: &str, stderr_tail: &str) -> RemoteCheckOutcome {
        RemoteCheckOutcome::Failed {
            problem,
            error: error.to_owned(),
            stderr_tail: stderr_tail.to_owned(),
        }
    }

    #[test]
    fn one_line_per_remote_with_the_fix_of_its_failure() {
        let checks = remote_checks(&RemoteCheckResult {
            remotes: vec![
                remote(
                    "git@github.com",
                    RemoteScheme::Ssh,
                    RemoteCheckOutcome::Reachable,
                ),
                remote(
                    "git@gitlab.com",
                    RemoteScheme::Ssh,
                    failed(
                        RemoteProblem::SshAuth,
                        "exit status: 128",
                        "git@gitlab.com: Permission denied (publickey).\nfatal: Could not read from remote repository.\n\nPlease make sure you have the correct access rights\nand the repository exists.\n",
                    ),
                ),
                remote(
                    "https://gitlab.example.com",
                    RemoteScheme::Https,
                    failed(
                        RemoteProblem::HttpsAuth,
                        "exit status: 128",
                        "fatal: could not read Username for 'https://gitlab.example.com': terminal prompts disabled\n",
                    ),
                ),
                remote(
                    "git@gitlab.corp",
                    RemoteScheme::Ssh,
                    failed(RemoteProblem::Network, "no answer within 20s", ""),
                ),
                remote(
                    "git@gone.example",
                    RemoteScheme::Ssh,
                    failed(
                        RemoteProblem::NotFound,
                        "exit status: 128",
                        "ERROR: Repository not found.\n",
                    ),
                ),
                remote(
                    "/srv/x.git",
                    RemoteScheme::Local,
                    failed(RemoteProblem::Other, "cannot run git: not found", ""),
                ),
            ],
        });
        let report = render(&DoctorReport { checks });
        let lines: Vec<&str> = report
            .lines()
            .filter(|line| !line.starts_with(' '))
            .collect();
        assert_eq!(
            lines,
            [
                "ok    remote git@github.com (ssh, via personal/loom): reachable",
                "fail  remote git@gitlab.com (ssh, via personal/loom): git@gitlab.com: Permission denied (publickey).",
                "fail  remote https://gitlab.example.com (https, via personal/loom): fatal: could not read Username for 'https://gitlab.example.com': terminal prompts disabled",
                "fail  remote git@gitlab.corp (ssh, via personal/loom): no answer within 20s",
                "fail  remote git@gone.example (ssh, via personal/loom): ERROR: Repository not found.",
                "fail  remote /srv/x.git (local, via personal/loom): cannot run git: not found",
            ]
        );
        assert!(report.contains("core.sshCommand"), "{report}");
        assert!(report.contains("switch the remote to SSH"), "{report}");
        assert!(report.contains("VPN"), "{report}");
        assert!(
            report.contains("loom repo forget personal/loom"),
            "{report}"
        );
        assert!(
            report.contains("git -C /home/me/perso/loom ls-remote origin HEAD"),
            "{report}"
        );
        let none = remote_checks(&RemoteCheckResult { remotes: vec![] });
        assert_eq!(none[0].status, CheckStatus::Ok);
    }

    fn updated(slug: &str, outcomes: Vec<UpdateOutcome>) -> RepoSummary {
        let mut repo = repo(
            &format!("/home/me/{slug}"),
            ContextAssignment::Unassigned,
            &[],
        );
        repo.identity.slug = slug.to_owned();
        repo.last_update = Some(LastUpdate {
            time: Timestamp::EPOCH,
            policy: RepoUpdatePolicy::Ff,
            outcomes,
        });
        repo
    }

    #[test]
    fn failed_and_skipped_updates_are_grouped_by_reason() {
        let not_found = || {
            UpdateOutcome::Failed {
            step: UpdateStep::Fetch,
            error: "exit status: 128".to_owned(),
            stderr_tail: "remote: ERROR: The project you were looking for could not be found or you don't have permission to view it.\n".to_owned(),
        }
        };
        let mut repos: Vec<RepoSummary> =
            ["company-a/a", "company-a/b", "company-a/c", "company-a/d"]
                .into_iter()
                .map(|slug| updated(slug, vec![not_found()]))
                .collect();
        repos.push(updated(
            "personal/busy",
            vec![UpdateOutcome::Skipped {
                reason: SkipReason::Busy {
                    step: UpdateStep::FastForward,
                    stderr_tail: String::new(),
                },
            }],
        ));
        repos.push(updated(
            "personal/work",
            vec![
                UpdateOutcome::Skipped {
                    reason: SkipReason::Dirty { paths: 2 },
                },
                UpdateOutcome::Skipped {
                    reason: SkipReason::DefaultBranchDiverged {
                        branch: "main".to_owned(),
                        ahead: 1,
                        behind: 1,
                    },
                },
            ],
        ));
        repos.push(updated("personal/fine", vec![UpdateOutcome::Fetched]));
        repos.push(updated(
            "personal/off",
            vec![UpdateOutcome::Skipped {
                reason: SkipReason::PolicyOff,
            }],
        ));
        let checks = updates_checks(&repos);
        let details: Vec<&str> = checks.iter().map(|check| check.detail.as_str()).collect();
        assert_eq!(
            details,
            [
                "remote not found (4): company-a/a, company-a/b, company-a/c, …",
                "repository busy (1): personal/busy",
                "local changes (1): personal/work",
                "default branch diverged (1): personal/work",
            ]
        );
        assert!(checks.iter().all(|check| check.check == "updates"
            && check.status == CheckStatus::Warn
            && check.fix.is_some()));
        assert!(
            checks[0]
                .fix
                .as_deref()
                .unwrap()
                .contains("moved or deleted upstream")
        );

        let ok = updates_checks(&repos[6..]);
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].status, CheckStatus::Ok);
        assert_eq!(
            ok[0].detail,
            "the last update of all 2 repositories went through"
        );
    }

    #[test]
    fn report_lines() {
        let report = DoctorReport {
            checks: vec![
                DoctorCheck {
                    check: "roots".to_owned(),
                    status: CheckStatus::Ok,
                    detail: "2 roots exist".to_owned(),
                    fix: None,
                },
                DoctorCheck {
                    check: "unassigned".to_owned(),
                    status: CheckStatus::Warn,
                    detail: "1 repository has no context: /x".to_owned(),
                    fix: Some("add\n  snippet".to_owned()),
                },
            ],
        };
        assert_eq!(
            render(&report),
            "ok    roots: 2 roots exist\nwarn  unassigned: 1 repository has no context: /x\n      fix: add\n             snippet\n"
        );
        assert_eq!(
            tilde(Path::new("/home/me/src"), Some(Path::new(HOME))),
            "~/src"
        );
        assert_eq!(tilde(Path::new("/home/me"), Some(Path::new(HOME))), "~");
        assert_eq!(tilde(Path::new("/srv"), Some(Path::new(HOME))), "/srv");
    }

    fn disk() -> DiskUsage {
        DiskUsage {
            computed_at: Timestamp::EPOCH,
            total_bytes: 35_000_000,
            versions_bytes: 30_000_000,
            index_bytes: 4_200_000,
            index_wal_bytes: 4_096,
            logs_bytes: 24_000,
            state_bytes: 8_192,
            other_bytes: 767_712,
            contexts: vec![],
            kept_versions: vec!["0.1.0".to_owned(), "0.1.1".to_owned()],
            available_bytes: Some(50_000_000_000),
            warnings: vec![],
        }
    }

    fn disk_check(checks: &[DoctorCheck], name: &str) -> DoctorCheck {
        checks
            .iter()
            .find(|check| check.check == name)
            .unwrap_or_else(|| panic!("no {name} in {checks:?}"))
            .clone()
    }

    const LOOM_HOME: &str = "/home/me/.loom";

    #[test]
    fn a_healthy_home_gives_four_ok_checks() {
        let checks = disk_checks(&disk(), Path::new(LOOM_HOME), "0.2.0");
        let names: Vec<&str> = checks.iter().map(|check| check.check.as_str()).collect();
        assert_eq!(names, ["disk", "disk-versions", "disk-wal", "disk-free"]);
        assert!(
            checks
                .iter()
                .all(|check| check.status == CheckStatus::Ok && check.fix.is_none()),
            "{checks:?}"
        );
        assert_eq!(
            checks[0].detail,
            "35 MB (versions 30 MB, index 4.2 MB, logs 24 KB, contexts 0 B) in /home/me/.loom"
        );
        assert_eq!(
            checks[1].detail,
            "2 previous versions kept (30 MB): rollback needs at most 2"
        );
        assert_eq!(checks[2].detail, "index.db-wal is 4.1 KB, below 64 MB");
        assert_eq!(
            checks[3].detail,
            "50 GB free on the volume of /home/me/.loom"
        );
    }

    #[test]
    fn unmeasured_parts_warn() {
        let usage = DiskUsage {
            warnings: vec![
                "cannot read /home/me/.loom/log: permission denied".to_owned(),
                "cannot read /home/me/.loom/state: permission denied".to_owned(),
            ],
            ..disk()
        };
        let check = disk_check(&disk_checks(&usage, Path::new(LOOM_HOME), "0.2.0"), "disk");
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(
            check.detail.ends_with(
                "; counted as 0: cannot read /home/me/.loom/log: permission denied; cannot read /home/me/.loom/state: permission denied"
            ),
            "{}",
            check.detail
        );
        assert!(check.fix.unwrap().contains("readable"));
    }

    #[test]
    fn versions_beyond_what_rollback_needs_warn_with_the_ones_to_delete() {
        let usage = DiskUsage {
            kept_versions: ["0.1.0", "0.1.1", "0.1.2", "notes"]
                .map(str::to_owned)
                .to_vec(),
            ..disk()
        };
        let checks = disk_checks(&usage, Path::new(LOOM_HOME), "0.2.0");
        let check = disk_check(&checks, "disk-versions");
        assert_eq!(check.status, CheckStatus::Warn);
        assert_eq!(
            check.detail,
            "3 previous versions kept (30 MB), rollback needs at most 2: 0.1.0 is not needed"
        );
        assert_eq!(
            check.fix.as_deref(),
            Some(
                "delete it (`rm -r /home/me/.loom/versions/0.1.0`), or let the next upgrade or rollback prune it"
            )
        );
    }

    #[test]
    fn a_copy_of_the_running_version_is_not_needed() {
        let usage = DiskUsage {
            kept_versions: vec!["0.1.0".to_owned(), "0.2.0".to_owned()],
            ..disk()
        };
        let checks = disk_checks(&usage, Path::new(LOOM_HOME), "0.2.0");
        let check = disk_check(&checks, "disk-versions");
        assert_eq!(check.status, CheckStatus::Warn, "{check:?}");
        assert!(
            check.detail.ends_with(": 0.2.0 is not needed"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn several_versions_to_delete_are_listed_from_the_oldest() {
        let usage = DiskUsage {
            kept_versions: ["0.1.3", "0.1.0", "0.1.2", "0.1.1"]
                .map(str::to_owned)
                .to_vec(),
            ..disk()
        };
        // A running version that is not semver: only the count decides.
        let checks = disk_checks(&usage, Path::new(LOOM_HOME), "unknown");
        let check = disk_check(&checks, "disk-versions");
        assert!(
            check.detail.ends_with(": 0.1.0, 0.1.1 are not needed"),
            "{}",
            check.detail
        );
        assert_eq!(
            check.fix.as_deref(),
            Some(
                "delete them (`rm -r /home/me/.loom/versions/0.1.0 /home/me/.loom/versions/0.1.1`), or let the next upgrade or rollback prune them"
            )
        );
    }

    #[test]
    fn no_previous_version_is_ok() {
        let usage = DiskUsage {
            kept_versions: vec![],
            versions_bytes: 0,
            ..disk()
        };
        let checks = disk_checks(&usage, Path::new(LOOM_HOME), "0.2.0");
        let check = disk_check(&checks, "disk-versions");
        assert_eq!(check.status, CheckStatus::Ok);
        assert_eq!(check.detail, "no previous version kept");
    }

    #[test]
    fn a_large_wal_warns_above_the_threshold() {
        let at = |bytes| {
            let usage = DiskUsage {
                index_wal_bytes: bytes,
                ..disk()
            };
            disk_check(
                &disk_checks(&usage, Path::new(LOOM_HOME), "0.2.0"),
                "disk-wal",
            )
        };
        assert_eq!(at(WAL_WARN_BYTES).status, CheckStatus::Ok);
        let check = at(WAL_WARN_BYTES + 1);
        assert_eq!(check.status, CheckStatus::Warn);
        assert_eq!(check.detail, "index.db-wal is 64 MB, above 64 MB");
        assert!(check.fix.unwrap().contains("restart loomd"));
    }

    #[test]
    fn low_free_space_warns_below_the_threshold() {
        let at = |available| {
            let usage = DiskUsage {
                available_bytes: available,
                ..disk()
            };
            disk_checks(&usage, Path::new(LOOM_HOME), "0.2.0")
        };
        assert_eq!(
            disk_check(&at(Some(FREE_SPACE_WARN_BYTES)), "disk-free").status,
            CheckStatus::Ok
        );
        let check = disk_check(&at(Some(FREE_SPACE_WARN_BYTES - 1)), "disk-free");
        assert_eq!(check.status, CheckStatus::Warn);
        assert_eq!(
            check.detail,
            "1.0 GB free on the volume of /home/me/.loom, below 1.0 GB"
        );
        assert!(check.fix.is_some());
        // Unknown: `disk` carries the warning of the failed query.
        assert!(at(None).iter().all(|check| check.check != "disk-free"));
    }
}
