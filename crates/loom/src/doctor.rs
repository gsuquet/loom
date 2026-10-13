//! `loom repo doctor`: the checks, decided without I/O from what `main.rs` read (configuration,
//! filesystem, loomd, `ssh`).

use std::collections::BTreeSet;
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};

use loom_api::{
    CheckStatus, ContextName, DaemonDiagnostics, DoctorCheck, DoctorReport, RepoSummary,
    ServicePlatform, SshAgentSocket,
};
use loom_config::config::Config;
use loom_repo::ContextRules;

use crate::repo::is_unassigned;

/// Longest a `ssh` check may run before it is killed.
pub const SSH_DEADLINE_SECS: u64 = 15;

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

/// An SSH endpoint of a remote: `[user@]host[:port]`, as written in the URL (an SSH alias
/// stays an alias, so that `~/.ssh/config` applies).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SshTarget {
    /// User; `git` when the URL has none.
    pub user: String,
    /// Host name or alias.
    pub host: String,
    /// Port, when given.
    pub port: Option<u16>,
}

impl fmt::Display for SshTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.user, self.host)?;
        if let Some(port) = self.port {
            write!(f, ":{port}")?;
        }
        Ok(())
    }
}

impl SshTarget {
    /// Arguments of `ssh` for a non-interactive check of this target.
    #[must_use]
    pub fn ssh_args(&self) -> Vec<String> {
        let mut args: Vec<String> = ["-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "-T"]
            .map(str::to_owned)
            .to_vec();
        if let Some(port) = self.port {
            args.push("-p".to_owned());
            args.push(port.to_string());
        }
        args.push(format!("{}@{}", self.user, self.host));
        args
    }
}

/// The SSH endpoint of a remote URL.
///
/// Read from `ssh://[user@]host[:port]/path` (also `git+ssh://`, `ssh+git://`) or the
/// scp-like `[user@]host:path`. `None` for any other URL, and for a user or host that could
/// pass an option to `ssh` (leading `-`, spaces, control characters).
#[must_use]
pub fn ssh_target(url: &str) -> Option<SshTarget> {
    let (authority, explicit) = if let Some((scheme, rest)) = url.split_once("://") {
        if !matches!(scheme, "ssh" | "git+ssh" | "ssh+git") {
            return None;
        }
        (rest.split('/').next().unwrap_or_default(), true)
    } else {
        // scp-like: a `:` before any `/`.
        let (authority, _) = url.split_once(':')?;
        if authority.contains('/') {
            return None;
        }
        (authority, false)
    };
    let (user, host_port) = match authority.rsplit_once('@') {
        Some((user, rest)) => (Some(user), rest),
        None => (None, authority),
    };
    // `[v6 address]` keeps its colons; a port follows `:` in an `ssh://` URL only.
    let (host, port_text) = match host_port.strip_prefix('[') {
        Some(bracketed) => {
            let (host, rest) = bracketed.split_once(']')?;
            (host, rest.strip_prefix(':'))
        }
        None if explicit => match host_port.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (host_port, None),
        },
        None => (host_port, None),
    };
    let port = match port_text {
        Some(port) if explicit => Some(port.parse::<u16>().ok()?),
        Some(_) => return None,
        None => None,
    };
    let safe = |text: &str| {
        !text.is_empty()
            && !text.starts_with('-')
            && text.chars().all(|c| !c.is_whitespace() && !c.is_control())
    };
    let user = user.unwrap_or("git");
    if !safe(host) || !safe(user) {
        return None;
    }
    Some(SshTarget {
        user: user.to_owned(),
        host: host.to_owned(),
        port,
    })
}

/// Distinct SSH endpoints of every remote of `repos`, sorted.
#[must_use]
pub fn ssh_targets(repos: &[RepoSummary]) -> Vec<SshTarget> {
    repos
        .iter()
        .flat_map(|repo| repo.identity.remotes.iter())
        .filter_map(|remote| ssh_target(&remote.url))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// What `ssh -T` printed and how it ended. `code` is `None` when it was killed (deadline).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshRun {
    /// Exit code.
    pub code: Option<i32>,
    /// stdout and stderr, joined.
    pub output: String,
}

/// Forges greet an authenticated key without opening a shell, and exit with various codes
/// (GitHub with 1): the greeting decides, not the code.
const GREETINGS: [&str; 4] = [
    "successfully authenticated",
    "welcome to gitlab",
    "authenticated via",
    "you can use git to connect",
];

/// The check of one SSH target from how `ssh -T` ended.
#[must_use]
pub fn ssh_check(target: &SshTarget, run: &SshRun) -> DoctorCheck {
    let name = format!("ssh {target}");
    let lines: Vec<&str> = run
        .output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let lower = run.output.to_lowercase();
    if let Some(greeting) = GREETINGS
        .iter()
        .find(|greeting| lower.contains(*greeting))
        .and_then(|greeting| {
            lines
                .iter()
                .find(|line| line.to_lowercase().contains(greeting))
        })
    {
        return check(name, CheckStatus::Ok, format!("authenticated: {greeting}"));
    }
    let last = lines.last().copied().unwrap_or("no output");
    let fix = format!(
        "run `ssh -T {target}` in a terminal: load the key in the agent (`ssh-add`), accept the host key, or check the network (VPN)"
    );
    match run.code {
        Some(0) => check(name, CheckStatus::Ok, "connected"),
        Some(255) => with_fix(check(name, CheckStatus::Fail, last), fix),
        Some(code) => with_fix(
            check(
                name,
                CheckStatus::Warn,
                format!("exited with code {code}: {last}"),
            ),
            fix,
        ),
        None => with_fix(
            check(
                name,
                CheckStatus::Fail,
                format!("no answer within {SSH_DEADLINE_SECS} s"),
            ),
            fix,
        ),
    }
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
        CheckStatus, ContextAssignment, DaemonDiagnostics, DoctorCheck, DoctorReport, Remote,
        RepoIdentity, RepoSummary, ServicePlatform, SshAgentSocket,
    };
    use loom_config::config::parse_config;

    use super::{
        SshRun, SshTarget, agent_check, contexts_check, home_check, render, roots, roots_checks,
        ssh_check, ssh_target, ssh_targets, tilde, unassigned_check,
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

    fn target(user: &str, host: &str, port: Option<u16>) -> SshTarget {
        SshTarget {
            user: user.to_owned(),
            host: host.to_owned(),
            port,
        }
    }

    #[test]
    fn ssh_targets_from_remote_urls() {
        let cases = [
            (
                "git@github.com:gsuquet/loom.git",
                Some(target("git", "github.com", None)),
            ),
            ("gh-work:org/repo.git", Some(target("git", "gh-work", None))),
            (
                "me@host.example:repo",
                Some(target("me", "host.example", None)),
            ),
            (
                "ssh://git@gitlab.example:2222/g/r.git",
                Some(target("git", "gitlab.example", Some(2222))),
            ),
            (
                "ssh://gitlab.example/g/r",
                Some(target("git", "gitlab.example", None)),
            ),
            ("git+ssh://u@h/r", Some(target("u", "h", None))),
            ("ssh://git@[::1]:22/r", Some(target("git", "::1", Some(22)))),
            ("ssh://git@[::1]/r", Some(target("git", "::1", None))),
            ("https://github.com/gsuquet/loom.git", None),
            ("file:///srv/r.git", None),
            ("/srv/backup/loom", None),
            ("../sibling", None),
            ("./a:b", None),
            ("-oProxyCommand=x:repo", None),
            ("git@-oProxyCommand=x:repo", None),
            ("ssh://h:notaport/r", None),
        ];
        for (url, expected) in cases {
            assert_eq!(ssh_target(url), expected, "{url}");
        }
        let repos = vec![
            repo(
                "/a",
                ContextAssignment::Unassigned,
                &["git@github.com:a/b.git", "https://x/y"],
            ),
            repo(
                "/b",
                ContextAssignment::Unassigned,
                &["git@github.com:c/d.git", "ssh://git@gitlab.example:2222/e"],
            ),
        ];
        let targets: Vec<String> = ssh_targets(&repos)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(targets, ["git@github.com", "git@gitlab.example:2222"]);
        assert_eq!(
            target("git", "h", Some(2222)).ssh_args(),
            [
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=5",
                "-T",
                "-p",
                "2222",
                "git@h"
            ]
        );
    }

    #[test]
    fn ssh_outcomes() {
        let target = target("git", "github.com", None);
        let run = |code: Option<i32>, output: &str| {
            ssh_check(
                &target,
                &SshRun {
                    code,
                    output: output.to_owned(),
                },
            )
        };
        // GitHub exits 1 on success.
        let github = run(
            Some(1),
            "Hi me! You've successfully authenticated, but GitHub does not provide shell access.\n",
        );
        assert_eq!(github.status, CheckStatus::Ok);
        assert_eq!(github.check, "ssh git@github.com");
        assert!(github.detail.starts_with("authenticated: Hi me!"));
        assert_eq!(
            run(Some(0), "Welcome to GitLab, @me!\n").status,
            CheckStatus::Ok
        );
        assert_eq!(run(Some(0), "").detail, "connected");
        let denied = run(
            Some(255),
            "git@github.com: Permission denied (publickey).\n",
        );
        assert_eq!(denied.status, CheckStatus::Fail);
        assert_eq!(
            denied.detail,
            "git@github.com: Permission denied (publickey)."
        );
        assert!(denied.fix.unwrap().contains("ssh -T git@github.com"));
        assert_eq!(run(Some(1), "shell refused\n").status, CheckStatus::Warn);
        assert_eq!(run(None, "").status, CheckStatus::Fail);
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
