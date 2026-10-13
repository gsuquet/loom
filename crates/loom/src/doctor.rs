//! `loom repo doctor`: the checks, decided without I/O from what `main.rs` read (configuration,
//! filesystem, loomd and its remote checks).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use loom_api::{
    CheckStatus, ContextName, DaemonDiagnostics, DoctorCheck, DoctorReport, RemoteCheck,
    RemoteCheckOutcome, RemoteCheckResult, RemoteProblem, RemoteScheme, RepoSummary,
    ServicePlatform, SkipReason, SshAgentSocket, UpdateOutcome, UpdateStep,
};
use loom_config::config::Config;
use loom_repo::{ContextRules, classify_remote_failure, remote_endpoint};

use crate::repo::is_unassigned;

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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use loom_api::{
        CheckStatus, ContextAssignment, DaemonDiagnostics, DoctorCheck, DoctorReport, LastUpdate,
        Remote, RemoteCheck, RemoteCheckOutcome, RemoteCheckResult, RemoteProblem, RemoteScheme,
        RepoIdentity, RepoSummary, RepoUpdatePolicy, ServicePlatform, SkipReason, SshAgentSocket,
        Timestamp, UpdateOutcome, UpdateStep,
    };
    use loom_config::config::parse_config;

    use super::{
        agent_check, contexts_check, home_check, remote_checks, render, roots, roots_checks, tilde,
        unassigned_check, updates_checks, uses_ssh,
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
                    "https://gitlab.ippon.fr",
                    RemoteScheme::Https,
                    failed(
                        RemoteProblem::HttpsAuth,
                        "exit status: 128",
                        "fatal: could not read Username for 'https://gitlab.ippon.fr': terminal prompts disabled\n",
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
                "fail  remote https://gitlab.ippon.fr (https, via personal/loom): fatal: could not read Username for 'https://gitlab.ippon.fr': terminal prompts disabled",
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
        let mut repos: Vec<RepoSummary> = ["norauto/a", "norauto/b", "norauto/c", "norauto/d"]
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
                "remote not found (4): norauto/a, norauto/b, norauto/c, …",
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
}
