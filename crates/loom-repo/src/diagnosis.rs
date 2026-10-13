//! Remote diagnosis for `loom repo doctor`: the endpoints of the primary remotes, the
//! repository each one is checked in, and what git's error output says went wrong. Pure.

use std::collections::BTreeMap;
use std::path::PathBuf;

use loom_api::{RemoteProblem, RemoteScheme, RepoSummary, UpdateOutcome, UpdateStep};

/// Where a remote URL leads: its transport and its endpoint.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RemoteEndpoint {
    /// Transport.
    pub scheme: RemoteScheme,
    /// `user@host[:port]` for SSH (the host as written, so that an SSH alias stays an alias),
    /// `scheme://host[:port]` for HTTP(S) and `git://`, the URL otherwise.
    pub endpoint: String,
}

/// The endpoint of the remote URL `url` (as stored: without password). Pure.
#[must_use]
pub fn remote_endpoint(url: &str) -> RemoteEndpoint {
    let url = url.trim();
    let endpoint = |scheme, endpoint: String| RemoteEndpoint { scheme, endpoint };
    if let Some((scheme, rest)) = url.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        let authority = rest.split('/').next().unwrap_or_default();
        let (user, host) = match authority.rsplit_once('@') {
            // A password never reaches the endpoint.
            Some((user, host)) => (user.split(':').next(), host),
            None => (None, authority),
        };
        let host = host.to_ascii_lowercase();
        return match scheme.as_str() {
            "ssh" | "git+ssh" | "ssh+git" => endpoint(
                RemoteScheme::Ssh,
                format!(
                    "{}@{host}",
                    user.filter(|user| !user.is_empty()).unwrap_or("git")
                ),
            ),
            "https" => endpoint(RemoteScheme::Https, format!("https://{host}")),
            "http" => endpoint(RemoteScheme::Http, format!("http://{host}")),
            "git" => endpoint(RemoteScheme::Git, format!("git://{host}")),
            "file" => endpoint(RemoteScheme::Local, url.to_owned()),
            _ => endpoint(RemoteScheme::Other, url.to_owned()),
        };
    }
    // git's rule: scp-like when a `:` comes before any `/`, otherwise a local path.
    match url.split_once(':') {
        Some((authority, _)) if !authority.contains('/') && !authority.is_empty() => {
            let (user, host) = authority
                .rsplit_once('@')
                .map_or(("git", authority), |(user, host)| (user, host));
            endpoint(
                RemoteScheme::Ssh,
                format!("{user}@{}", host.to_ascii_lowercase()),
            )
        }
        _ => endpoint(RemoteScheme::Local, url.to_owned()),
    }
}

/// A remote check to run: `git -C <path> ls-remote --exit-code <remote> HEAD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCheckTarget {
    /// The endpoint checked.
    pub endpoint: RemoteEndpoint,
    /// Root of the repository the check runs in.
    pub path: PathBuf,
    /// Its slug.
    pub slug: String,
    /// Its primary remote.
    pub remote: String,
}

/// One check per distinct endpoint of the primary remotes of `repos`, sorted by scheme, then
/// endpoint. Pure.
///
/// Each runs in one repository of that endpoint: one whose last update did not fail to fetch
/// when there is one (its failure may be the repository's own, such as a deleted project),
/// then the lowest slug.
#[must_use]
pub fn remote_check_targets(repos: &[RepoSummary]) -> Vec<RemoteCheckTarget> {
    let mut chosen: BTreeMap<RemoteEndpoint, (bool, &RepoSummary, &str)> = BTreeMap::new();
    for repo in repos {
        let identity = &repo.identity;
        let Some(remote) = identity
            .primary_remote
            .as_deref()
            .and_then(|name| identity.remotes.iter().find(|remote| remote.name == name))
        else {
            continue;
        };
        let failed = fetch_failed(repo);
        let endpoint = remote_endpoint(&remote.url);
        let better = chosen.get(&endpoint).is_none_or(|(best_failed, best, _)| {
            (failed, identity.slug.as_str()) < (*best_failed, best.identity.slug.as_str())
        });
        if better {
            chosen.insert(endpoint, (failed, repo, remote.name.as_str()));
        }
    }
    chosen
        .into_iter()
        .map(|(endpoint, (_, repo, remote))| RemoteCheckTarget {
            endpoint,
            path: repo.identity.path.clone(),
            slug: repo.identity.slug.clone(),
            remote: remote.to_owned(),
        })
        .collect()
}

/// The last update of `repo` failed to fetch.
fn fetch_failed(repo: &RepoSummary) -> bool {
    repo.last_update.as_ref().is_some_and(|update| {
        update.outcomes.iter().any(|outcome| {
            matches!(
                outcome,
                UpdateOutcome::Failed {
                    step: UpdateStep::Fetch,
                    ..
                }
            )
        })
    })
}

/// What git's standard error `stderr` says about a remote that could not be reached. Pure.
///
/// Authentication first (SSH, then HTTP(S)), then a repository the remote says does not exist,
/// then the network; [`RemoteProblem::Other`] when nothing is recognized.
#[must_use]
pub fn classify_remote_failure(stderr: &str) -> RemoteProblem {
    const SSH_AUTH: [&str; 6] = [
        "permission denied (publickey",
        "permission denied (password",
        "permission denied (keyboard-interactive",
        "host key verification failed",
        "too many authentication failures",
        "no such identity",
    ];
    const HTTPS_AUTH: [&str; 8] = [
        "could not read username",
        "could not read password",
        "terminal prompts disabled",
        "authentication failed for",
        "http basic: access denied",
        "invalid username or password",
        "the requested url returned error: 401",
        "the requested url returned error: 403",
    ];
    const NOT_FOUND: [&str; 5] = [
        "repository not found",
        "could not be found or you don't have permission",
        "does not appear to be a git repository",
        "the requested url returned error: 404",
        "' not found",
    ];
    const NETWORK: [&str; 11] = [
        "could not resolve host",
        "could not resolve hostname",
        "name or service not known",
        "connection timed out",
        "operation timed out",
        "connection refused",
        "network is unreachable",
        "no route to host",
        "failed to connect to",
        "connection reset",
        "connection closed by",
    ];
    let text = stderr.to_ascii_lowercase();
    let any = |patterns: &[&str]| patterns.iter().any(|pattern| text.contains(pattern));
    if any(&SSH_AUTH) {
        RemoteProblem::SshAuth
    } else if any(&HTTPS_AUTH) {
        RemoteProblem::HttpsAuth
    } else if any(&NOT_FOUND) {
        RemoteProblem::NotFound
    } else if any(&NETWORK) {
        RemoteProblem::Network
    } else {
        RemoteProblem::Other
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use loom_api::{
        ContextAssignment, LastUpdate, Remote, RemoteProblem, RemoteScheme, RepoIdentity,
        RepoSummary, RepoUpdatePolicy, Timestamp, UpdateOutcome, UpdateStep,
    };

    use super::{RemoteEndpoint, classify_remote_failure, remote_check_targets, remote_endpoint};

    fn endpoint(scheme: RemoteScheme, endpoint: &str) -> RemoteEndpoint {
        RemoteEndpoint {
            scheme,
            endpoint: endpoint.to_owned(),
        }
    }

    #[test]
    fn endpoints_of_remote_urls() {
        let cases = [
            (
                "git@github.com:gsuquet/loom.git",
                endpoint(RemoteScheme::Ssh, "git@github.com"),
            ),
            (
                "gh-work:org/repo.git",
                endpoint(RemoteScheme::Ssh, "git@gh-work"),
            ),
            (
                "me@Host.Example:repo",
                endpoint(RemoteScheme::Ssh, "me@host.example"),
            ),
            (
                "ssh://git@gitlab.example:2222/g/r.git",
                endpoint(RemoteScheme::Ssh, "git@gitlab.example:2222"),
            ),
            (
                "ssh://gitlab.example/g/r",
                endpoint(RemoteScheme::Ssh, "git@gitlab.example"),
            ),
            ("git+ssh://u@h/r", endpoint(RemoteScheme::Ssh, "u@h")),
            (
                "https://github.com/gsuquet/loom.git",
                endpoint(RemoteScheme::Https, "https://github.com"),
            ),
            (
                "https://me@gitlab.ippon.fr/x/y.git",
                endpoint(RemoteScheme::Https, "https://gitlab.ippon.fr"),
            ),
            (
                "HTTPS://Git.Example:8443/x",
                endpoint(RemoteScheme::Https, "https://git.example:8443"),
            ),
            ("http://h/x", endpoint(RemoteScheme::Http, "http://h")),
            ("git://h/x", endpoint(RemoteScheme::Git, "git://h")),
            (
                "file:///srv/r.git",
                endpoint(RemoteScheme::Local, "file:///srv/r.git"),
            ),
            (
                "/srv/backup/loom",
                endpoint(RemoteScheme::Local, "/srv/backup/loom"),
            ),
            ("./a:b", endpoint(RemoteScheme::Local, "./a:b")),
            (
                "s3://bucket/x",
                endpoint(RemoteScheme::Other, "s3://bucket/x"),
            ),
        ];
        for (url, expected) in cases {
            assert_eq!(remote_endpoint(url), expected, "{url}");
        }
    }

    fn repo(slug: &str, urls: &[(&str, &str)], fetch_failed: bool) -> RepoSummary {
        RepoSummary {
            identity: RepoIdentity {
                path: PathBuf::from(format!("/home/me/{slug}")),
                name: slug.rsplit('/').next().unwrap().to_owned(),
                slug: slug.to_owned(),
                context: ContextAssignment::Unassigned,
                remotes: urls
                    .iter()
                    .map(|(name, url)| Remote {
                        name: (*name).to_owned(),
                        url: (*url).to_owned(),
                        location: None,
                    })
                    .collect(),
                primary_remote: urls.first().map(|(name, _)| (*name).to_owned()),
                forge: None,
            },
            registered: false,
            last_used: None,
            state: None,
            last_update: fetch_failed.then(|| LastUpdate {
                time: Timestamp::EPOCH,
                policy: RepoUpdatePolicy::Ff,
                outcomes: vec![UpdateOutcome::Failed {
                    step: UpdateStep::Fetch,
                    error: "exit status: 128".to_owned(),
                    stderr_tail: "ERROR: Repository not found.\n".to_owned(),
                }],
            }),
        }
    }

    #[test]
    fn one_check_per_endpoint_in_a_repository_that_fetched() {
        let repos = [
            repo(
                "a/deleted",
                &[("origin", "git@github.com:a/gone.git")],
                true,
            ),
            repo("b/loom", &[("origin", "git@github.com:b/loom.git")], false),
            repo("c/tool", &[("origin", "git@github.com:c/tool.git")], false),
            repo(
                "d/work",
                &[
                    ("upstream", "https://gitlab.example/d/work.git"),
                    ("fork", "git@github.com:me/work"),
                ],
                false,
            ),
            repo("e/local", &[], false),
            repo("f/old", &[("origin", "git@github.com:f/old.git")], true),
        ];
        let targets: Vec<(String, String, String)> = remote_check_targets(&repos)
            .into_iter()
            .map(|target| (target.endpoint.endpoint, target.slug, target.remote))
            .collect();
        assert_eq!(
            targets,
            [
                (
                    "git@github.com".to_owned(),
                    "b/loom".to_owned(),
                    "origin".to_owned()
                ),
                (
                    "https://gitlab.example".to_owned(),
                    "d/work".to_owned(),
                    "upstream".to_owned()
                ),
            ]
        );
        // Every repository of the endpoint failed: the lowest slug.
        let targets = remote_check_targets(&[repos[5].clone(), repos[0].clone()]);
        assert_eq!(targets[0].slug, "a/deleted");
        assert_eq!(targets[0].path, PathBuf::from("/home/me/a/deleted"));
    }

    #[test]
    fn failures_are_classified_from_git_output() {
        let cases = [
            (
                "git@github.com: Permission denied (publickey).\r\nfatal: Could not read from remote repository.\n\nPlease make sure you have the correct access rights\nand the repository exists.\n",
                RemoteProblem::SshAuth,
            ),
            (
                "Host key verification failed.\nfatal: Could not read from remote repository.\n",
                RemoteProblem::SshAuth,
            ),
            (
                "fatal: could not read Username for 'https://gitlab.ippon.fr': terminal prompts disabled\n",
                RemoteProblem::HttpsAuth,
            ),
            (
                "remote: HTTP Basic: Access denied\nfatal: Authentication failed for 'https://gitlab.com/x/y.git/'\n",
                RemoteProblem::HttpsAuth,
            ),
            (
                "ERROR: Repository not found.\nfatal: Could not read from remote repository.\n\nPlease make sure you have the correct access rights\nand the repository exists.\n",
                RemoteProblem::NotFound,
            ),
            (
                "remote: \nremote: ========================================================================\nremote: \nremote: ERROR: The project you were looking for could not be found or you don't have permission to view it.\nfatal: Could not read from remote repository.\n",
                RemoteProblem::NotFound,
            ),
            (
                "remote: Repository not found.\nfatal: repository 'https://github.com/x/y.git/' not found\n",
                RemoteProblem::NotFound,
            ),
            (
                "fatal: '/srv/gone.git' does not appear to be a git repository\nfatal: Could not read from remote repository.\n",
                RemoteProblem::NotFound,
            ),
            (
                "ssh: Could not resolve hostname gitlab.corp: nodename nor servname provided, or not known\nfatal: Could not read from remote repository.\n",
                RemoteProblem::Network,
            ),
            (
                "fatal: unable to access 'https://gitlab.corp/x.git/': Failed to connect to gitlab.corp port 443 after 75000 ms: Couldn't connect to server\n",
                RemoteProblem::Network,
            ),
            (
                "ssh: connect to host gitlab.corp port 22: Operation timed out\n",
                RemoteProblem::Network,
            ),
            (
                "fatal: protocol error: bad pack header\n",
                RemoteProblem::Other,
            ),
            ("", RemoteProblem::Other),
        ];
        for (stderr, expected) in cases {
            assert_eq!(classify_remote_failure(stderr), expected, "{stderr}");
        }
    }
}
