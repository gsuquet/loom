//! The repository index of loomd (ADR-0012): a store thread owns the only connection that
//! writes `index.db` and runs the scans, registrations and removals one at a time; the API
//! handlers read through their own connections.
//!
//! Decisions are in `loom_repo` and `loom_store`; this module does the I/O.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use axum::http::StatusCode;
use loom_api::{ApiError, RepoSummary, ScanIssue, ScanResult, Timestamp};
use loom_config::config::Config;
use loom_config::repos::{ForgeConfig, ForgeHost};
use loom_repo::scan::Exclusions;
use loom_repo::{
    ContextRules, ScanOptions, diff, find_root, read_repos, scan_roots, scan_with_registered,
};
use loom_store::{Reader, RepoFilter, Store, StoreError};
use loomd::api::{internal, invalid_request, repo_not_found, timestamp, unavailable};
use miette::{Context as _, IntoDiagnostic as _, Result};
use tokio::sync::oneshot;
use tracing::field::Empty;
use tracing::{Span, debug, info, info_span, warn};

/// Longest wait for `ssh -G <alias>`.
const SSH_TIMEOUT: Duration = Duration::from_secs(2);

/// SSH alias resolution given to the scans.
type Resolver = Box<dyn Fn(&str) -> Option<String> + Send>;

/// An API error with its HTTP status.
pub type Failure = (StatusCode, ApiError);

type Reply<T> = oneshot::Sender<Result<T, Failure>>;

/// A request to the store thread, with the span of the API request that sent it.
enum Command {
    Scan {
        reply: Option<Reply<ScanResult>>,
        parent: Span,
    },
    Register {
        path: PathBuf,
        reply: Reply<RepoSummary>,
        parent: Span,
    },
    Forget {
        slug: String,
        reply: Reply<RepoSummary>,
        parent: Span,
    },
    Stop,
}

/// The running store thread.
pub struct RepoIndex {
    commands: mpsc::Sender<Command>,
    thread: JoinHandle<()>,
}

/// What the API handlers use: the command channel and the database path for reads.
#[derive(Clone)]
pub struct RepoHandle {
    commands: mpsc::Sender<Command>,
    db: PathBuf,
}

/// How scans walk and identify repositories, from the configuration read at start.
struct Discovery {
    roots: Vec<PathBuf>,
    max_depth: u32,
    exclusions: Exclusions,
    rules: ContextRules,
    forges: BTreeMap<ForgeHost, ForgeConfig>,
    ssh: Resolver,
}

/// Opens `db` (created and migrated when needed), then starts the store thread, which scans
/// at once. The configuration is read once: a change needs a restart.
pub fn start(db: &Path, config: &Config, home: &Path) -> Result<(RepoIndex, RepoHandle)> {
    let store = Store::open(db).into_diagnostic()?;
    let discovery = Discovery {
        roots: scan_roots(config, home),
        max_depth: config.repos.max_depth,
        exclusions: Exclusions::new(&config.repos.exclude).into_diagnostic()?,
        rules: ContextRules::new(config, home).into_diagnostic()?,
        forges: config.forges.clone(),
        ssh: ssh_resolver(),
    };
    let (commands, received) = mpsc::channel();
    // The scan on start, queued before any request.
    let _ = commands.send(Command::Scan {
        reply: None,
        parent: Span::none(),
    });
    let thread = thread::Builder::new()
        .name("repo-index".to_owned())
        .spawn(move || Worker { store, discovery }.run(&received))
        .into_diagnostic()
        .wrap_err("cannot start the repository index thread")?;
    let handle = RepoHandle {
        commands: commands.clone(),
        db: db.to_path_buf(),
    };
    Ok((RepoIndex { commands, thread }, handle))
}

impl RepoIndex {
    /// Stops the store thread after the command it is running; queued commands are dropped and
    /// their requests answered `unavailable`.
    pub fn stop(self) {
        let _ = self.commands.send(Command::Stop);
        if self.thread.join().is_err() {
            warn!("the repository index thread panicked");
        }
    }
}

impl RepoHandle {
    /// Scans the roots and the registered repositories. A request made while a scan runs
    /// waits for that scan and returns its result.
    pub async fn scan(&self) -> Result<ScanResult, Failure> {
        self.send(|reply| Command::Scan {
            reply: Some(reply),
            parent: Span::current(),
        })
        .await
    }

    /// Registers the repository holding `path` (absolute, normalized).
    pub async fn register(&self, path: PathBuf) -> Result<RepoSummary, Failure> {
        self.send(|reply| Command::Register {
            path,
            reply,
            parent: Span::current(),
        })
        .await
    }

    /// Removes the repository with `slug` from the index.
    pub async fn forget(&self, slug: String) -> Result<RepoSummary, Failure> {
        self.send(|reply| Command::Forget {
            slug,
            reply,
            parent: Span::current(),
        })
        .await
    }

    /// Repositories matching `filter`, read on a blocking thread through a read connection.
    pub async fn list(&self, filter: RepoFilter) -> Result<Vec<RepoSummary>, Failure> {
        self.read(move |reader| reader.list(&filter)).await
    }

    /// The repository with `slug`.
    pub async fn get(&self, slug: String) -> Result<RepoSummary, Failure> {
        match self
            .read(move |reader| reader.get(&slug).map(|repo| (slug, repo)))
            .await?
        {
            (_, Some(repo)) => Ok(repo),
            (slug, None) => Err((StatusCode::NOT_FOUND, repo_not_found(&slug))),
        }
    }

    async fn send<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, Failure> {
        let (reply, answer) = oneshot::channel();
        let stopped = || {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                unavailable("loomd is stopping"),
            )
        };
        self.commands.send(command(reply)).map_err(|_| stopped())?;
        answer.await.map_err(|_| stopped())?
    }

    async fn read<T: Send + 'static>(
        &self,
        query: impl FnOnce(&Reader) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, Failure> {
        let db = self.db.clone();
        let read = tokio::task::spawn_blocking(move || query(&Reader::open(&db)?)).await;
        match read {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(store_failure(&error)),
            Err(error) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                internal(format!("index read failed: {error}")),
            )),
        }
    }
}

fn store_failure(error: &StoreError) -> Failure {
    warn!(%error, "repository index error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        internal(error.to_string()),
    )
}

struct Worker {
    store: Store,
    discovery: Discovery,
}

impl Worker {
    /// Runs the commands in order until `Stop` or until every sender is gone.
    fn run(mut self, received: &mpsc::Receiver<Command>) {
        let mut pending = VecDeque::new();
        loop {
            let command = match pending.pop_front() {
                Some(command) => command,
                None => match received.recv() {
                    Ok(command) => command,
                    Err(_) => return,
                },
            };
            match command {
                Command::Scan { reply, parent } => {
                    let result = self.scan(&parent);
                    let mut replies: Vec<_> = reply.into_iter().collect();
                    // Scans asked for while this one ran get its result.
                    while let Ok(command) = received.try_recv() {
                        match command {
                            Command::Scan {
                                reply: Some(reply), ..
                            } => replies.push(reply),
                            Command::Scan { reply: None, .. } => {}
                            other => pending.push_back(other),
                        }
                    }
                    for reply in replies {
                        let _ = reply.send(result.clone());
                    }
                }
                Command::Register {
                    path,
                    reply,
                    parent,
                } => {
                    let _ = reply.send(self.register(&path, &parent));
                }
                Command::Forget {
                    slug,
                    reply,
                    parent,
                } => {
                    let _ = reply.send(self.forget(&slug, &parent));
                }
                Command::Stop => return,
            }
        }
    }

    fn options(&self) -> ScanOptions<'_> {
        ScanOptions {
            max_depth: self.discovery.max_depth,
            exclusions: self.discovery.exclusions.clone(),
            rules: &self.discovery.rules,
            forges: &self.discovery.forges,
            resolve_alias: &*self.discovery.ssh,
        }
    }

    /// Scans the roots and the registered repositories, and applies the changes in one
    /// transaction.
    fn scan(&mut self, parent: &Span) -> Result<ScanResult, Failure> {
        let span = info_span!(
            parent: parent,
            "repo.scan",
            roots = self.discovery.roots.len(),
            added = Empty,
            updated = Empty,
            removed = Empty,
            errors = Empty,
            duration_ms = Empty,
        );
        let _entered = span.enter();
        let started = Instant::now();
        let registered = self
            .store
            .registered_paths()
            .map_err(|error| store_failure(&error))?;
        let previous: Vec<_> = self
            .store
            .repos()
            .map_err(|error| store_failure(&error))?
            .into_iter()
            .map(|repo| repo.identity)
            .collect();
        let options = self.options();
        let (report, scanned) = scan_with_registered(&self.discovery.roots, &registered, &options);
        let mut changes = diff(&previous, &scanned, &report);
        let mut errors: Vec<ScanIssue> = report
            .errors
            .iter()
            .map(|error| ScanIssue {
                path: error.path().to_path_buf(),
                message: error.to_string(),
            })
            .collect();
        for list in [&mut changes.added, &mut changes.updated] {
            list.retain(|repo| {
                let utf8 = repo.path.to_str().is_some();
                if !utf8 {
                    errors.push(ScanIssue {
                        path: repo.path.clone(),
                        message: "path is not valid UTF-8: not indexed".to_owned(),
                    });
                }
                utf8
            });
        }
        let now = now()?;
        self.store
            .apply_scan(&changes, now)
            .map_err(|error| store_failure(&error))?;
        let result = ScanResult {
            added: count(changes.added.len()),
            updated: count(changes.updated.len()),
            removed: count(changes.removed.len()),
            errors,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        };
        span.record("added", result.added);
        span.record("updated", result.updated);
        span.record("removed", result.removed);
        span.record("errors", count(result.errors.len()));
        span.record("duration_ms", result.duration_ms);
        for issue in &result.errors {
            debug!(path = %issue.path.display(), message = %issue.message, "scan error");
        }
        info!(
            added = result.added,
            updated = result.updated,
            removed = result.removed,
            errors = result.errors.len(),
            duration_ms = result.duration_ms,
            "repositories scanned"
        );
        Ok(result)
    }

    /// Registers the repository holding `path`.
    fn register(&mut self, path: &Path, parent: &Span) -> Result<RepoSummary, Failure> {
        let span =
            info_span!(parent: parent, "repo.register", path = %path.display(), slug = Empty);
        let _entered = span.enter();
        let bad = |message: String| (StatusCode::BAD_REQUEST, invalid_request(message));
        let root = find_root(path)
            .map_err(|error| bad(format!("cannot examine {}: {error}", path.display())))?
            .ok_or_else(|| bad(format!("no git repository at or above {}", path.display())))?;
        let options = self.options();
        let mut report = read_repos(std::slice::from_ref(&root), &options);
        let Some(repo) = report.repos.pop() else {
            let reason = report
                .errors
                .first()
                .map_or_else(|| "it vanished".to_owned(), ToString::to_string);
            return Err(bad(format!(
                "cannot read the repository {}: {reason}",
                root.display()
            )));
        };
        // The path came from JSON: valid UTF-8, as the store requires.
        let summary = self
            .store
            .register(repo.identity, now()?)
            .map_err(|error| store_failure(&error))?;
        span.record("slug", summary.identity.slug.as_str());
        info!(slug = %summary.identity.slug, path = %summary.identity.path.display(), "repository registered");
        Ok(summary)
    }

    fn forget(&mut self, slug: &str, parent: &Span) -> Result<RepoSummary, Failure> {
        let span = info_span!(parent: parent, "repo.forget", slug);
        let _entered = span.enter();
        match self.store.forget(slug) {
            Ok(Some(repo)) => {
                info!(slug, path = %repo.identity.path.display(), "repository forgotten");
                Ok(repo)
            }
            Ok(None) => Err((StatusCode::NOT_FOUND, repo_not_found(slug))),
            Err(error) => Err(store_failure(&error)),
        }
    }
}

fn now() -> Result<Timestamp, Failure> {
    timestamp(SystemTime::now())
        .ok_or_else(|| (StatusCode::INTERNAL_SERVER_ERROR, loomd::api::clock_error()))
}

fn count(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

/// SSH host alias resolution with `ssh -G <alias>`, cached per alias for the daemon's
/// lifetime. Only names without a dot are looked up (a name with a dot is a host name), and
/// only names of letters, digits, `-` and `_` not starting with `-`, so that a remote URL can
/// never pass an option to `ssh`. Failures resolve to `None` and are logged at debug.
fn ssh_resolver() -> Resolver {
    let cache: RefCell<HashMap<String, Option<String>>> = RefCell::default();
    Box::new(move |alias: &str| {
        let candidate = !alias.is_empty()
            && !alias.starts_with('-')
            && alias
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
        if !candidate {
            return None;
        }
        if let Some(known) = cache.borrow().get(alias) {
            return known.clone();
        }
        let resolved = ssh_g(alias)
            .map_err(|error| debug!(alias, %error, "cannot resolve the SSH alias"))
            .ok()
            .flatten();
        cache
            .borrow_mut()
            .insert(alias.to_owned(), resolved.clone());
        resolved
    })
}

/// The `hostname` that `ssh -G <alias>` prints, lowercased; `None` when it prints none.
fn ssh_g(alias: &str) -> Result<Option<String>, String> {
    let mut child = Process::new("ssh")
        .arg("-G")
        .arg(alias)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot run ssh: {error}"))?;
    let mut stdout = child.stdout.take().ok_or_else(|| "no stdout".to_owned())?;
    // Read on a thread so that a full pipe never blocks the child past the deadline.
    let reader = thread::spawn(move || {
        let mut text = String::new();
        stdout.read_to_string(&mut text).map(|_| text)
    });
    let deadline = Instant::now() + SSH_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("ssh -G timed out after {SSH_TIMEOUT:?}"));
            }
            Err(error) => return Err(format!("cannot wait for ssh: {error}")),
        }
    };
    let text = reader
        .join()
        .map_err(|_| "the ssh output reader panicked".to_owned())?
        .map_err(|error| format!("cannot read the ssh output: {error}"))?;
    if !status.success() {
        return Err(format!("ssh -G exited with {status}"));
    }
    Ok(parse_hostname(&text))
}

/// The value of the `hostname` line of `ssh -G` output, lowercased.
fn parse_hostname(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("hostname "))
        .map(|host| host.trim().to_ascii_lowercase())
        .filter(|host| !host.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{parse_hostname, ssh_resolver};

    #[test]
    fn hostname_line_is_parsed() {
        let output = "user me\nhostname Git.Example.COM\nport 22\n";
        assert_eq!(parse_hostname(output).as_deref(), Some("git.example.com"));
        assert_eq!(parse_hostname("user me\n"), None);
        assert_eq!(parse_hostname("hostname \n"), None);
    }

    #[test]
    fn names_with_a_dot_or_unsafe_characters_are_not_looked_up() {
        let resolve = ssh_resolver();
        for host in ["github.com", "-oProxyCommand=x", "a b", "", "x;y"] {
            assert_eq!(resolve(host), None, "{host}");
        }
    }
}
