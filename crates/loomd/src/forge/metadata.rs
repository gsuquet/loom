//! Forge metadata of the repo cards (docs/design/reviews.md, Repo forge metadata): which forge
//! project each repository reads, and the reads of one credential.
//!
//! [`plan`] is pure. [`read_target`] calls the forge through [`ProjectApi`], so that tests use
//! a fake; [`HttpProjects`] is the real client.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use loom_api::{ForgeKind, ForgeMetadata, ForgeProject, MaybeKnown, RepoSummary, Timestamp};
use loom_forge::client::{
    ForgeError, GithubClient, GithubForge as _, GitlabClient, GitlabForge as _, RateLimitWait,
};
use tracing::{info_span, warn};

use super::Token;
use super::check::ForgeTarget;
use super::token::TokenResolver;

/// One forge project to read, and the repositories whose primary remote is that project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetch {
    /// Index of the credential in the targets given to [`plan`].
    pub target: usize,
    /// Path of the project on the forge, such as `acme/shop` or `group/sub/project`.
    pub project: String,
    /// The repositories, sorted. More than one for clones and worktrees of one project.
    pub repos: Vec<PathBuf>,
    /// One of them has no forge metadata yet, or metadata of another project (its primary
    /// remote changed).
    pub new: bool,
}

/// What a forge poll does for the repositories of the index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// The projects to read, by credential, then project.
    pub fetches: Vec<Fetch>,
    /// Repositories with forge metadata but no credential for their host any more: their
    /// metadata is removed.
    pub clears: Vec<PathBuf>,
}

/// Which project each repository reads with which credential. Pure.
///
/// A repository reads the project of its primary remote when its context has a credential for
/// the host of that remote; otherwise its metadata, if any, is removed.
#[must_use]
pub fn plan(repos: &[RepoSummary], targets: &[ForgeTarget]) -> Plan {
    let mut fetches: BTreeMap<(usize, String), Fetch> = BTreeMap::new();
    let mut clears = Vec::new();
    for repo in repos {
        let identity = &repo.identity;
        let location = identity
            .primary_remote
            .as_deref()
            .and_then(|name| identity.remotes.iter().find(|remote| remote.name == name))
            .and_then(|remote| remote.location.as_ref());
        let context = identity.context.context();
        let target = location.and_then(|location| {
            targets.iter().position(|target| {
                Some(&target.context) == context
                    && target.host == location.host
                    && identity.forge == Some(target.kind)
            })
        });
        match (target, location) {
            (Some(target), Some(location)) => {
                let fetch = fetches
                    .entry((target, location.path.clone()))
                    .or_insert_with(|| Fetch {
                        target,
                        project: location.path.clone(),
                        repos: Vec::new(),
                        new: false,
                    });
                fetch.repos.push(identity.path.clone());
                fetch.new |= outdated(repo, &location.path);
            }
            (None, _) | (_, None) => {
                if repo.forge_metadata.is_some() {
                    clears.push(identity.path.clone());
                }
            }
        }
    }
    let mut fetches: Vec<Fetch> = fetches.into_values().collect();
    for fetch in &mut fetches {
        fetch.repos.sort();
    }
    clears.sort();
    Plan { fetches, clears }
}

/// The repository has no forge metadata, or the metadata of another project than `project`.
fn outdated(repo: &RepoSummary, project: &str) -> bool {
    repo.forge_metadata
        .as_ref()
        .is_none_or(|metadata| match &metadata.project {
            MaybeKnown::Known(known) => !known
                .web_url
                .to_ascii_lowercase()
                .ends_with(&format!("/{}", project.to_ascii_lowercase())),
            MaybeKnown::Unknown => false,
        })
}

/// The fetches of the credential `target` to read now: all of them when its poll is `due`,
/// otherwise the [`Fetch::new`] ones not in `read` (read already, the write not seen yet).
/// Pure.
#[must_use]
pub fn select<'a>(
    plan: &'a Plan,
    target: usize,
    due: bool,
    read: &BTreeSet<(usize, String)>,
) -> Vec<&'a Fetch> {
    plan.fetches
        .iter()
        .filter(|fetch| {
            fetch.target == target
                && (due || (fetch.new && !read.contains(&(target, fetch.project.clone()))))
        })
        .collect()
}

/// The next poll of a credential and the end of its pause, after reads at `started`. Pure.
///
/// `next_poll` moves one `interval` (jittered by the caller) after `started` when the poll was
/// `due`. After a rate limit, nothing is read before `resume`, the next poll included.
#[must_use]
pub fn reschedule(
    next_poll: Instant,
    started: Instant,
    due: bool,
    interval: Duration,
    resume: Option<Instant>,
) -> (Instant, Option<Instant>) {
    let next = if due { started + interval } else { next_poll };
    (resume.map_or(next, |resume| next.max(resume)), resume)
}

/// The forge call that reads one project.
pub trait ProjectApi {
    /// The project at `project` on the host of `target`, read with `token`.
    ///
    /// # Errors
    ///
    /// [`ForgeError`].
    fn project(
        &self,
        target: &ForgeTarget,
        token: &Token,
        project: &str,
    ) -> Result<ForgeProject, ForgeError>;
}

/// [`ProjectApi`] over HTTP: GitHub GraphQL, GitLab REST.
#[derive(Debug, Clone)]
pub struct HttpProjects {
    /// The `User-Agent` of the calls.
    pub user_agent: String,
}

impl ProjectApi for HttpProjects {
    fn project(
        &self,
        target: &ForgeTarget,
        token: &Token,
        project: &str,
    ) -> Result<ForgeProject, ForgeError> {
        match target.kind {
            ForgeKind::Github => {
                GithubClient::new(&target.api, token.clone(), &self.user_agent).repository(project)
            }
            ForgeKind::Gitlab => {
                GitlabClient::new(&target.api, token.clone(), &self.user_agent).project(project)
            }
        }
    }
}

/// The result of the reads of one credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetReads {
    /// The forge metadata to store, for each repository read.
    pub results: Vec<(PathBuf, ForgeMetadata)>,
    /// The forge limits the rate: the reads stopped, the repositories not read keep their
    /// metadata. Read again after this wait.
    pub rate_limited: Option<RateLimitWait>,
}

/// Reads the projects of `fetches` with the credential `target`, one after the other.
///
/// - The token cannot be resolved: every project is unknown, with the reason.
/// - A 401 resolves the token again, once, and the read is made again.
/// - The forge cannot be reached, or refuses the new token too: this project and the next ones
///   are unknown, without a call each (a forge that is down would make every call wait for its
///   timeout).
/// - The forge limits the rate: the reads stop; the projects not read are not in the results.
/// - Any other failure: this project is unknown, with the reason.
pub fn read_target(
    target: &ForgeTarget,
    fetches: &[&Fetch],
    resolver: &TokenResolver,
    api: &dyn ProjectApi,
    now: Timestamp,
) -> TargetReads {
    let span = info_span!(
        "forge.metadata",
        context = target.context.as_str(),
        host = target.host.as_str(),
        projects = fetches.len()
    );
    let _entered = span.enter();
    let mut reads = TargetReads {
        results: Vec::new(),
        rate_limited: None,
    };
    // The error of a token command can show its output: it stays in the log.
    let no_token =
        || "cannot resolve the forge token: `loom repo doctor` gives the reason".to_owned();
    let unknown = |error: String| ForgeMetadata {
        refreshed_at: now,
        project: MaybeKnown::Unknown,
        error: Some(error),
    };
    let mut token = match resolver.resolve(&target.source) {
        Ok(token) => token,
        Err(error) => {
            warn!(%error, "cannot resolve the forge token");
            let metadata = unknown(no_token());
            reads.results = results(fetches, &metadata);
            return reads;
        }
    };
    let mut retried = false;
    for (index, fetch) in fetches.iter().enumerate() {
        let mut result = api.project(target, &token, &fetch.project);
        if matches!(result, Err(ForgeError::Unauthorized { .. })) && !retried {
            retried = true;
            resolver.invalidate(&target.source, &token);
            match resolver.resolve(&target.source) {
                Ok(again) => {
                    token = again;
                    result = api.project(target, &token, &fetch.project);
                }
                Err(error) => {
                    warn!(%error, "cannot resolve the forge token again");
                    let metadata = unknown(no_token());
                    reads.results.extend(results(&fetches[index..], &metadata));
                    return reads;
                }
            }
        }
        match result {
            Ok(project) => reads.results.extend(results(
                &[*fetch],
                &ForgeMetadata {
                    refreshed_at: now,
                    project: MaybeKnown::Known(project),
                    error: None,
                },
            )),
            Err(ForgeError::RateLimited { wait, message, .. }) => {
                warn!(
                    message,
                    "the forge limits the rate: forge metadata not read"
                );
                reads.rate_limited = Some(wait);
                return reads;
            }
            // Refused after a new token, or not reachable: the next calls would fail the same.
            Err(error @ (ForgeError::Transport(_) | ForgeError::Unauthorized { .. })) => {
                warn!(%error, "forge metadata unknown for the rest of this poll");
                let metadata = unknown(error.to_string());
                reads.results.extend(results(&fetches[index..], &metadata));
                return reads;
            }
            Err(error) => {
                warn!(project = fetch.project, %error, "cannot read the forge project");
                reads
                    .results
                    .extend(results(&[*fetch], &unknown(error.to_string())));
            }
        }
    }
    reads
}

/// `metadata` for every repository of `fetches`.
fn results(fetches: &[&Fetch], metadata: &ForgeMetadata) -> Vec<(PathBuf, ForgeMetadata)> {
    fetches
        .iter()
        .flat_map(|fetch| fetch.repos.iter())
        .map(|path| (path.clone(), metadata.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::time::Duration;

    use loom_api::{
        ContextAssignment, CredentialOrigin, ForgeKind, ForgeMetadata, ForgeProject,
        ForgeVisibility, MaybeKnown, Remote, RemoteUrl, RepoIdentity, RepoSummary, Timestamp,
    };
    use loom_config::forges::TokenSource;
    use loom_forge::client::{ForgeError, RateLimitWait};

    use std::collections::BTreeSet;
    use std::time::Instant;

    use super::{Fetch, Plan, ProjectApi, plan, read_target, reschedule, select};
    use crate::forge::Token;
    use crate::forge::api_base;
    use crate::forge::check::ForgeTarget;
    use crate::forge::token::TokenResolver;

    fn target(context: &str, host: &str, kind: ForgeKind) -> ForgeTarget {
        ForgeTarget {
            context: context.parse().unwrap(),
            host: host.to_owned(),
            kind,
            api: api_base(kind, host),
            origin: CredentialOrigin::Host,
            source: TokenSource::Command("printf token-1".to_owned()),
        }
    }

    fn repo(path: &str, context: Option<&str>, url: Option<(&str, &str)>) -> RepoSummary {
        let forge = url.and_then(|(host, _)| match host {
            "github.com" => Some(ForgeKind::Github),
            "gitlab.example.com" => Some(ForgeKind::Gitlab),
            _ => None,
        });
        RepoSummary {
            identity: RepoIdentity {
                path: PathBuf::from(path),
                name: path.rsplit('/').next().unwrap().to_owned(),
                slug: path.trim_start_matches('/').to_owned(),
                context: context.map_or(ContextAssignment::Unassigned, |context| {
                    ContextAssignment::Default {
                        context: context.parse().unwrap(),
                    }
                }),
                remotes: url
                    .map(|(host, project)| Remote {
                        name: "origin".to_owned(),
                        url: format!("git@{host}:{project}.git"),
                        location: Some(RemoteUrl {
                            host: host.to_owned(),
                            port: None,
                            path: project.to_owned(),
                        }),
                    })
                    .into_iter()
                    .collect(),
                primary_remote: url.map(|_| "origin".to_owned()),
                forge,
            },
            registered: false,
            last_used: None,
            state: None,
            last_update: None,
            forge_metadata: None,
        }
    }

    fn at(secs: u64) -> Timestamp {
        Timestamp::from_unix_secs(secs).unwrap()
    }

    fn project(archived: bool) -> ForgeProject {
        ForgeProject {
            id: 1_296_269,
            default_branch: Some("main".to_owned()),
            visibility: ForgeVisibility::Private,
            archived,
            web_url: "https://github.com/acme/shop".to_owned(),
            open_mrs: Some(3),
        }
    }

    fn known(project: ForgeProject) -> ForgeMetadata {
        ForgeMetadata {
            refreshed_at: at(100),
            project: MaybeKnown::Known(project),
            error: None,
        }
    }

    #[test]
    fn repositories_read_the_project_of_their_primary_remote_with_their_context_credential() {
        let targets = [
            target("personal", "github.com", ForgeKind::Github),
            target("work", "github.com", ForgeKind::Github),
            target("work", "gitlab.example.com", ForgeKind::Gitlab),
        ];
        let mut stale = repo("/p/old", Some("personal"), Some(("bitbucket.org", "a/b")));
        stale.forge_metadata = Some(known(project(false)));
        let mut unassigned = repo("/x/shop", None, Some(("github.com", "acme/shop")));
        unassigned.forge_metadata = Some(known(project(false)));
        let mut known_shop = repo(
            "/p/shop",
            Some("personal"),
            Some(("github.com", "acme/shop")),
        );
        known_shop.forge_metadata = Some(known(project(false)));
        let repos = [
            known_shop,
            // A second clone of the same project: one read.
            repo(
                "/p/shop-2",
                Some("personal"),
                Some(("github.com", "acme/shop")),
            ),
            repo("/w/shop", Some("work"), Some(("github.com", "acme/shop"))),
            repo(
                "/w/checkout",
                Some("work"),
                Some(("gitlab.example.com", "company-a/sub/checkout")),
            ),
            // No credential for this host, no metadata: nothing to do.
            repo(
                "/p/blog",
                Some("personal"),
                Some(("gitlab.example.com", "me/blog")),
            ),
            // No remote.
            repo("/p/notes", Some("personal"), None),
            stale,
            unassigned,
        ];
        assert_eq!(
            plan(&repos, &targets),
            Plan {
                fetches: vec![
                    Fetch {
                        target: 0,
                        project: "acme/shop".to_owned(),
                        repos: vec![PathBuf::from("/p/shop"), PathBuf::from("/p/shop-2")],
                        new: true,
                    },
                    Fetch {
                        target: 1,
                        project: "acme/shop".to_owned(),
                        repos: vec![PathBuf::from("/w/shop")],
                        new: true,
                    },
                    Fetch {
                        target: 2,
                        project: "company-a/sub/checkout".to_owned(),
                        repos: vec![PathBuf::from("/w/checkout")],
                        new: true,
                    },
                ],
                clears: vec![PathBuf::from("/p/old"), PathBuf::from("/x/shop")],
            }
        );
        // No credential at all: every metadata is removed.
        assert_eq!(
            plan(&repos, &[]).clears,
            [
                PathBuf::from("/p/old"),
                PathBuf::from("/p/shop"),
                PathBuf::from("/x/shop")
            ]
        );
        assert!(!plan(&repos[..1], &targets).fetches[0].new);
    }

    #[test]
    fn a_due_poll_reads_every_project_otherwise_only_the_new_ones() {
        let mut old = fetch("acme/old", &["/p/old"]);
        old.target = 1;
        let mut new = fetch("acme/new", &["/p/new"]);
        new.target = 1;
        new.new = true;
        let plan = Plan {
            fetches: vec![fetch("acme/shop", &["/p/shop"]), old, new],
            clears: vec![],
        };
        let none = BTreeSet::new();
        let projects = |target, due| {
            select(&plan, target, due, &none)
                .iter()
                .map(|fetch| fetch.project.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(projects(1, true), ["acme/old", "acme/new"]);
        assert_eq!(projects(1, false), ["acme/new"]);
        assert_eq!(projects(0, false), Vec::<String>::new());
        assert_eq!(projects(2, true), Vec::<String>::new());
        // Read already, its write not seen yet: not read again before the poll.
        let read = BTreeSet::from([(1, "acme/new".to_owned())]);
        assert_eq!(select(&plan, 1, false, &read), Vec::<&Fetch>::new());
        assert_eq!(select(&plan, 1, true, &read).len(), 2);
    }

    #[test]
    fn a_changed_remote_reads_the_new_project_at_once() {
        let targets = [target("personal", "github.com", ForgeKind::Github)];
        let mut moved = repo(
            "/p/shop",
            Some("personal"),
            Some(("github.com", "acme/shop-v2")),
        );
        moved.forge_metadata = Some(known(project(false)));
        let mut same = repo("/p/x", Some("personal"), Some(("github.com", "ACME/Shop")));
        same.forge_metadata = Some(known(project(false)));
        let mut failed = repo("/p/y", Some("personal"), Some(("github.com", "acme/y")));
        failed.forge_metadata = Some(ForgeMetadata {
            refreshed_at: at(100),
            project: MaybeKnown::Unknown,
            error: Some("down".to_owned()),
        });
        let new: Vec<bool> = plan(&[moved, same, failed], &targets)
            .fetches
            .iter()
            .map(|fetch| fetch.new)
            .collect();
        // Sorted by project: `ACME/Shop`, `acme/shop-v2`, `acme/y`.
        assert_eq!(new, [false, true, false]);
    }

    #[test]
    fn schedule_after_a_poll_a_new_read_and_a_rate_limit() {
        let start = Instant::now();
        let interval = Duration::from_mins(5);
        let later = start + Duration::from_secs(30);
        // A due poll: one interval after it started.
        assert_eq!(
            reschedule(start, start, true, interval, None),
            (start + interval, None)
        );
        // Reads of new repositories only: the poll stays where it was.
        assert_eq!(
            reschedule(start + interval, later, false, interval, None),
            (start + interval, None)
        );
        // A rate limit that ends after the next poll moves the poll.
        let resume = start + Duration::from_mins(20);
        assert_eq!(
            reschedule(start, start, true, interval, Some(resume)),
            (resume, Some(resume))
        );
        // One that ends before it keeps the poll.
        let soon = start + Duration::from_mins(1);
        assert_eq!(
            reschedule(start, start, true, interval, Some(soon)),
            (start + interval, Some(soon))
        );
    }

    /// Answers each call with the next result, and records the projects and tokens.
    struct FakeApi {
        answers: RefCell<VecDeque<Result<ForgeProject, ForgeError>>>,
        calls: RefCell<Vec<(String, String)>>,
    }

    impl FakeApi {
        fn new(answers: Vec<Result<ForgeProject, ForgeError>>) -> Self {
            Self {
                answers: RefCell::new(answers.into()),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl ProjectApi for FakeApi {
        fn project(
            &self,
            _target: &ForgeTarget,
            token: &Token,
            project: &str,
        ) -> Result<ForgeProject, ForgeError> {
            self.calls
                .borrow_mut()
                .push((project.to_owned(), token.expose().to_owned()));
            self.answers
                .borrow_mut()
                .pop_front()
                .expect("an answer for each call")
        }
    }

    fn fetch(project: &str, repos: &[&str]) -> Fetch {
        Fetch {
            target: 0,
            project: project.to_owned(),
            repos: repos.iter().map(PathBuf::from).collect(),
            new: false,
        }
    }

    fn resolver() -> TokenResolver {
        TokenResolver::new(Duration::from_secs(5), Box::new(|_| None))
    }

    fn github() -> ForgeTarget {
        target("personal", "github.com", ForgeKind::Github)
    }

    #[test]
    fn known_and_archived_projects_and_a_failure_of_one_project() {
        let fetches = [
            fetch("acme/shop", &["/p/shop", "/p/shop-2"]),
            fetch("acme/gone", &["/p/gone"]),
            fetch("acme/old", &["/p/old"]),
        ];
        let api = FakeApi::new(vec![
            Ok(project(false)),
            Err(ForgeError::NotFound {
                message: "Not Found".to_owned(),
            }),
            Ok(project(true)),
        ]);
        let reads = read_target(
            &github(),
            &fetches.iter().collect::<Vec<_>>(),
            &resolver(),
            &api,
            at(100),
        );
        assert_eq!(reads.rate_limited, None);
        assert_eq!(
            reads.results,
            vec![
                (PathBuf::from("/p/shop"), known(project(false))),
                (PathBuf::from("/p/shop-2"), known(project(false))),
                (
                    PathBuf::from("/p/gone"),
                    ForgeMetadata {
                        refreshed_at: at(100),
                        project: MaybeKnown::Unknown,
                        error: Some("not found on the forge (404): Not Found".to_owned()),
                    }
                ),
                (PathBuf::from("/p/old"), known(project(true))),
            ]
        );
        assert_eq!(api.calls.borrow().len(), 3);
    }

    #[test]
    fn a_forge_down_makes_the_rest_unknown_without_more_calls() {
        let fetches = [
            fetch("acme/shop", &["/p/shop"]),
            fetch("acme/a", &["/p/a"]),
            fetch("acme/b", &["/p/b"]),
        ];
        let api = FakeApi::new(vec![
            Ok(project(false)),
            Err(ForgeError::Transport("connection refused".to_owned())),
        ]);
        let reads = read_target(
            &github(),
            &fetches.iter().collect::<Vec<_>>(),
            &resolver(),
            &api,
            at(100),
        );
        let down = ForgeMetadata {
            refreshed_at: at(100),
            project: MaybeKnown::Unknown,
            error: Some("no usable answer from the forge: connection refused".to_owned()),
        };
        assert_eq!(
            reads.results,
            vec![
                (PathBuf::from("/p/shop"), known(project(false))),
                (PathBuf::from("/p/a"), down.clone()),
                (PathBuf::from("/p/b"), down),
            ]
        );
        assert_eq!(api.calls.borrow().len(), 2);
    }

    #[test]
    fn a_rate_limit_stops_the_reads_and_keeps_the_rest() {
        let fetches = [fetch("acme/shop", &["/p/shop"]), fetch("acme/a", &["/p/a"])];
        let wait = RateLimitWait::Until(at(4_000));
        let api = FakeApi::new(vec![Err(ForgeError::RateLimited {
            status: 403,
            wait,
            message: "API rate limit exceeded".to_owned(),
        })]);
        let reads = read_target(
            &github(),
            &fetches.iter().collect::<Vec<_>>(),
            &resolver(),
            &api,
            at(100),
        );
        assert_eq!(reads.results, vec![]);
        assert_eq!(reads.rate_limited, Some(wait));
    }

    #[test]
    fn no_token_makes_every_project_unknown_without_a_call() {
        let mut target = github();
        target.source = TokenSource::Env("LOOM_TEST_NO_SUCH_TOKEN".to_owned());
        let fetches = [fetch("acme/shop", &["/p/shop"]), fetch("acme/a", &["/p/a"])];
        let api = FakeApi::new(vec![]);
        let reads = read_target(
            &target,
            &fetches.iter().collect::<Vec<_>>(),
            &resolver(),
            &api,
            at(100),
        );
        assert_eq!(reads.results.len(), 2);
        for (_, metadata) in &reads.results {
            assert_eq!(metadata.project, MaybeKnown::Unknown);
            assert!(
                metadata
                    .error
                    .as_deref()
                    .unwrap()
                    .starts_with("cannot resolve the forge token: `loom repo doctor`"),
                "{metadata:?}"
            );
        }
        assert!(api.calls.borrow().is_empty());
    }

    #[test]
    fn a_401_resolves_the_token_again_once() {
        let dir = tempfile::TempDir::new().unwrap();
        let counter = dir.path().join("n");
        // A new token at each run: token-1, then token-11.
        let mut target = github();
        target.source = TokenSource::Command(format!(
            "printf 1 >> {0}; printf token-; cat {0}",
            counter.display()
        ));
        let unauthorized = || ForgeError::Unauthorized {
            message: "Bad credentials".to_owned(),
        };
        let fetches = [
            fetch("acme/shop", &["/p/shop"]),
            fetch("acme/a", &["/p/a"]),
            fetch("acme/b", &["/p/b"]),
        ];
        let api = FakeApi::new(vec![
            Err(unauthorized()),
            Ok(project(false)),
            Err(unauthorized()),
        ]);
        let reads = read_target(
            &target,
            &fetches.iter().collect::<Vec<_>>(),
            &resolver(),
            &api,
            at(100),
        );
        assert_eq!(
            *api.calls.borrow(),
            [
                ("acme/shop".to_owned(), "token-1".to_owned()),
                ("acme/shop".to_owned(), "token-11".to_owned()),
                // Not resolved again: refused twice in one poll is a bad token, and
                // `acme/b` is not called.
                ("acme/a".to_owned(), "token-11".to_owned()),
            ]
        );
        assert_eq!(reads.results[0].1, known(project(false)));
        for (_, metadata) in &reads.results[1..] {
            assert_eq!(metadata.project, MaybeKnown::Unknown);
            assert_eq!(
                metadata.error.as_deref(),
                Some("the forge refused the token (401): Bad credentials")
            );
        }
        assert_eq!(reads.results.len(), 3);
    }
}
