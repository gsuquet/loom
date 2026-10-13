//! The repository index of loomd (ADR-0012): a store thread owns the only connection that
//! writes `index.db` and runs the scans, registrations, removals and state writes one at a
//! time; the API handlers read through their own connections.
//!
//! Repository states are read by a small pool of state threads, off the store thread and off
//! the API, then written through the store thread. A state is refreshed after a scan (added
//! and updated repositories), for every repository after the scan on start and then every
//! `[repos] fetch_interval`, on registration, and on `GET /v1/repos/{context}/{name}?refresh=true`
//! (read on a blocking thread of the API, then written through the store thread).
//!
//! Decisions are in `loom_repo` and `loom_store`; this module does the I/O.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Read as _;
use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use axum::http::StatusCode;
use loom_api::{
    ApiError, LastUpdate, RepoState, RepoSummary, RepoSync, ScanIssue, ScanResult, SyncOutcome,
    SyncRequest, SyncResult, Timestamp,
};
use loom_config::config::Config;
use loom_config::repos::{ForgeConfig, ForgeHost};
use loom_repo::scan::Exclusions;
use loom_repo::state::unreadable;
use loom_repo::{
    ContextRules, ScanDiff, ScanOptions, StateOptions, UpdatePolicies, diff, find_root, git_dirs,
    plan, read_repos, scan_roots, scan_with_registered, state,
};
use loom_store::{Reader, RepoFilter, Store, StoreError};
use loomd::api::{ScanQuery, internal, invalid_request, repo_not_found, timestamp, unavailable};
use miette::{Context as _, IntoDiagnostic as _, Result};
use tokio::sync::oneshot;
use tracing::field::Empty;
use tracing::{Span, debug, info, info_span, warn};

use crate::updates::{self, Updater, UpdaterHandle};

/// Longest wait for `ssh -G <alias>`.
const SSH_TIMEOUT: Duration = Duration::from_secs(2);

/// SSH alias resolution given to the scans.
type Resolver = Box<dyn Fn(&str) -> Option<String> + Send>;

/// An API error with its HTTP status.
pub type Failure = (StatusCode, ApiError);

type Reply<T> = oneshot::Sender<Result<T, Failure>>;

/// Most state threads: each may walk a large working tree for its whole budget.
const MAX_STATE_THREADS: usize = 4;

/// A request to the store thread, with the span of the API request that sent it.
pub enum Command {
    /// `initial`: the scan on start, after which every state is refreshed.
    Scan {
        reply: Option<Reply<ScanResult>>,
        initial: bool,
        mode: ScanQuery,
        parent: Span,
    },
    /// Writes the state of the repository at `path`, then answers with the repository.
    SaveState {
        path: PathBuf,
        state: Box<RepoState>,
        reply: Option<Reply<RepoSummary>>,
    },
    /// Writes the result of an update of the repository at `path`, and the state read after
    /// it, then answers with the repository.
    SaveUpdate {
        path: PathBuf,
        update: Box<LastUpdate>,
        state: Option<Box<RepoState>>,
        reply: Reply<RepoSummary>,
    },
    Register {
        path: PathBuf,
        on_use: bool,
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

/// The running store thread, state threads and updater.
pub struct RepoIndex {
    commands: mpsc::Sender<Command>,
    thread: JoinHandle<()>,
    states: StatePool,
    updater: Updater,
}

/// What the API handlers use: the command channel, the database path for reads, and how
/// states are read.
#[derive(Clone)]
pub struct RepoHandle {
    commands: mpsc::Sender<Command>,
    db: PathBuf,
    state_options: StateOptions,
    updates: UpdaterHandle,
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
    let roots: Vec<String> = discovery
        .roots
        .iter()
        .map(|root| root.display().to_string())
        .collect();
    let (commands, received) = mpsc::channel();
    let state_options = StateOptions::default();
    let threads = thread::available_parallelism()
        .map_or(1, NonZero::get)
        .min(MAX_STATE_THREADS);
    let (refresher, states) = StatePool::start(threads, &commands, state_options)
        .into_diagnostic()
        .wrap_err("cannot start the repository state threads")?;
    let state_interval = config.repos.fetch_interval.get();
    info!(
        ?roots,
        state_threads = threads,
        state_interval_secs = state_interval.as_secs(),
        "repository index started"
    );
    // The scan on start, queued before any request.
    let _ = commands.send(Command::Scan {
        reply: None,
        initial: true,
        mode: ScanQuery::default(),
        parent: Span::none(),
    });
    let thread = thread::Builder::new()
        .name("repo-index".to_owned())
        .spawn(move || {
            Worker {
                store,
                discovery,
                refresher,
                state_interval,
            }
            .run(&received);
        })
        .into_diagnostic()
        .wrap_err("cannot start the repository index thread")?;
    let (updater, update_handle) = updates::start(
        updates::Settings {
            interval: state_interval,
            concurrency: usize::try_from(config.repos.fetch_concurrency).unwrap_or(usize::MAX),
            policies: UpdatePolicies::new(config, home),
            runner: loomd::repo_update::GitRunner::from_env(),
            state_options,
        },
        &commands,
        db.to_path_buf(),
    )
    .into_diagnostic()
    .wrap_err("cannot start the repository update threads")?;
    let handle = RepoHandle {
        commands: commands.clone(),
        db: db.to_path_buf(),
        state_options,
        updates: update_handle,
    };
    Ok((
        RepoIndex {
            commands,
            thread,
            states,
            updater,
        },
        handle,
    ))
}

impl RepoIndex {
    /// Stops the store thread after the command it is running; queued commands are dropped and
    /// their requests answered `unavailable`. The state threads finish the state they are
    /// reading (at most its budget) and drop the queued ones.
    pub fn stop(self) {
        // First: its workers write through the store thread.
        self.updater.stop();
        self.states.stopping.store(true, Ordering::Release);
        let _ = self.commands.send(Command::Stop);
        if self.thread.join().is_err() {
            warn!("the repository index thread panicked");
        }
        // The store thread dropped the job queue: the state threads see it closed.
        for thread in self.states.threads {
            if thread.join().is_err() {
                warn!("a repository state thread panicked");
            }
        }
    }
}

/// Queues repositories for the state threads, each path at most once at a time.
struct Refresher {
    jobs: mpsc::Sender<PathBuf>,
    queued: Arc<Mutex<HashSet<PathBuf>>>,
}

impl Refresher {
    fn enqueue(&self, path: PathBuf) {
        if lock(&self.queued).insert(path.clone()) {
            let _ = self.jobs.send(path);
        }
    }
}

/// The state threads.
struct StatePool {
    threads: Vec<JoinHandle<()>>,
    stopping: Arc<AtomicBool>,
}

impl StatePool {
    /// Starts `size` threads reading the states of the queued repositories and sending them to
    /// the store thread through `commands`.
    fn start(
        size: usize,
        commands: &mpsc::Sender<Command>,
        options: StateOptions,
    ) -> std::io::Result<(Refresher, Self)> {
        let (jobs, received) = mpsc::channel::<PathBuf>();
        let received = Arc::new(Mutex::new(received));
        let queued = Arc::new(Mutex::new(HashSet::new()));
        let stopping = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::with_capacity(size);
        for index in 0..size {
            let received = Arc::clone(&received);
            let queued = Arc::clone(&queued);
            let stopping = Arc::clone(&stopping);
            let commands = commands.clone();
            threads.push(
                thread::Builder::new()
                    .name(format!("repo-state-{index}"))
                    .spawn(move || {
                        loop {
                            // Released before reading: the other threads take the next jobs.
                            let next = lock(&received).recv();
                            let Ok(path) = next else { return };
                            if stopping.load(Ordering::Acquire) {
                                return;
                            }
                            // Removed first: a change during the read queues it again.
                            lock(&queued).remove(&path);
                            let state = read_state(&path, &options);
                            let command = Command::SaveState {
                                path,
                                state: Box::new(state),
                                reply: None,
                            };
                            if commands.send(command).is_err() {
                                return;
                            }
                        }
                    })?,
            );
        }
        Ok((Refresher { jobs, queued }, Self { threads, stopping }))
    }
}

/// `mutex` locked; a thread that panicked holding it left consistent data (a set, a queue).
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The state of the repository at `path` (its root), now.
fn read_state(path: &Path, options: &StateOptions) -> RepoState {
    let started = Instant::now();
    let now = timestamp(SystemTime::now()).unwrap_or(Timestamp::EPOCH);
    let state = match git_dirs(path) {
        Ok((git_dir, common_dir)) => state(&git_dir, &common_dir, path, now, options),
        Err(error) => unreadable(now, error.to_string()),
    };
    debug!(
        path = %path.display(),
        duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        error = state.error.as_deref().unwrap_or_default(),
        "repository state read"
    );
    state
}

impl RepoHandle {
    /// Scans the roots and the registered repositories. A plain scan requested while a plain
    /// scan runs waits for that scan and returns its result; a rebuild or a dry run runs on
    /// its own.
    pub async fn scan(&self, mode: ScanQuery) -> Result<ScanResult, Failure> {
        self.send(|reply| Command::Scan {
            reply: Some(reply),
            initial: false,
            mode,
            parent: Span::current(),
        })
        .await
    }

    /// Reads the state of the repository with `slug` now, on a blocking thread (bounded by the
    /// state budget), writes it through the store thread, and returns the repository.
    pub async fn refresh(&self, slug: String) -> Result<RepoSummary, Failure> {
        let repo = self.get(slug).await?;
        let path = repo.identity.path;
        let options = self.state_options;
        let read_path = path.clone();
        let state = tokio::task::spawn_blocking(move || read_state(&read_path, &options))
            .await
            .map_err(|error| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    internal(format!("state read failed: {error}")),
                )
            })?;
        let slug = repo.identity.slug;
        self.send(|reply| Command::SaveState {
            path,
            state: Box::new(state),
            reply: Some(reply),
        })
        .await
        .map_err(|(status, error)| {
            // Removed while its state was read.
            if status == StatusCode::NOT_FOUND {
                (status, repo_not_found(&slug))
            } else {
                (status, error)
            }
        })
    }

    /// Updates the repositories of `request` now (every repository when it names none), or
    /// with `dry_run`, says what an update would do from their state read now. Results sorted
    /// by slug.
    pub async fn sync(&self, request: SyncRequest) -> Result<SyncResult, Failure> {
        let mut repos = if request.repos.is_empty() {
            self.list(RepoFilter::default()).await?
        } else {
            let mut repos = Vec::with_capacity(request.repos.len());
            for slug in request.repos {
                repos.push(self.get(slug).await?);
            }
            repos
        };
        repos.sort_by(|a, b| a.identity.slug.cmp(&b.identity.slug));
        repos.dedup_by(|a, b| a.identity.slug == b.identity.slug);
        if request.dry_run {
            let policies = std::sync::Arc::clone(&self.updates.policies);
            let options = self.state_options;
            let planned = tokio::task::spawn_blocking(move || {
                repos
                    .into_iter()
                    .map(|repo| planned(repo, &policies, &options))
                    .collect()
            })
            .await
            .map_err(|error| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    internal(format!("dry run failed: {error}")),
                )
            })?;
            return Ok(SyncResult { repos: planned });
        }
        let repos = self.updates.sync(repos).await.ok_or_else(|| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                unavailable("loomd is stopping"),
            )
        })?;
        Ok(SyncResult { repos })
    }

    /// Registers the repository holding `path` (absolute, normalized). `on_use`: registration
    /// on use, see [`loom_api::RegisterRequest::on_use`].
    pub async fn register(&self, path: PathBuf, on_use: bool) -> Result<RepoSummary, Failure> {
        self.send(|reply| Command::Register {
            path,
            on_use,
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

/// What an update of `repo` would do, from its state read now.
fn planned(repo: RepoSummary, policies: &UpdatePolicies, options: &StateOptions) -> RepoSync {
    let identity = repo.identity;
    let policy = policies.policy(&identity.path, &identity.context);
    let state = read_state(&identity.path, options);
    RepoSync {
        result: SyncOutcome::Planned {
            policy,
            actions: plan(policy, identity.primary_remote.as_deref(), &state),
        },
        slug: identity.slug,
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
    refresher: Refresher,
    /// Time between two refreshes of every state.
    state_interval: Duration,
}

impl Worker {
    /// Runs the commands in order until `Stop` or until every sender is gone, and refreshes
    /// every state each `state_interval`.
    fn run(mut self, received: &mpsc::Receiver<Command>) {
        let mut pending = VecDeque::new();
        let mut next_refresh = Instant::now() + self.state_interval;
        loop {
            let command = if let Some(command) = pending.pop_front() {
                command
            } else {
                let wait = next_refresh.saturating_duration_since(Instant::now());
                match received.recv_timeout(wait) {
                    Ok(command) => command,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        self.refresh_all();
                        next_refresh = Instant::now() + self.state_interval;
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            };
            match command {
                Command::Scan {
                    reply,
                    mut initial,
                    mode,
                    parent,
                } => {
                    let result = self.scan(mode, &parent);
                    let mut replies: Vec<_> = reply.into_iter().collect();
                    // Plain scans asked for while this plain one ran get its result.
                    while mode == ScanQuery::default()
                        && let Ok(command) = received.try_recv()
                    {
                        match command {
                            Command::Scan {
                                reply,
                                initial: also_initial,
                                mode:
                                    ScanQuery {
                                        rebuild: false,
                                        dry_run: false,
                                    },
                                ..
                            } => {
                                replies.extend(reply);
                                initial |= also_initial;
                            }
                            other => pending.push_back(other),
                        }
                    }
                    for reply in replies {
                        let _ = reply.send(result.clone());
                    }
                    if initial {
                        self.refresh_all();
                    }
                }
                Command::SaveState { path, state, reply } => {
                    let saved = self.save_state(&path, &state);
                    if let Some(reply) = reply {
                        let _ = reply.send(saved);
                    }
                }
                Command::SaveUpdate {
                    path,
                    update,
                    state,
                    reply,
                } => {
                    let _ = reply.send(self.save_update(&path, &update, state.as_deref()));
                }
                Command::Register {
                    path,
                    on_use,
                    reply,
                    parent,
                } => {
                    let _ = reply.send(self.register(&path, on_use, &parent));
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

    /// Queues every repository of the index for a state refresh.
    fn refresh_all(&self) {
        match self.store.repos() {
            Ok(repos) => {
                debug!(repos = repos.len(), "refreshing every repository state");
                for repo in repos {
                    self.refresher.enqueue(repo.identity.path);
                }
            }
            Err(error) => warn!(%error, "cannot list the repositories to refresh their states"),
        }
    }

    /// Writes the state of the repository at `path`; the repository, or `not_found` when it
    /// was removed since its state was read.
    fn save_state(&mut self, path: &Path, state: &RepoState) -> Result<RepoSummary, Failure> {
        let saved = self
            .store
            .save_state(path, state)
            .map_err(|error| store_failure(&error))?;
        let repo = if saved {
            self.store
                .get_by_path(path)
                .map_err(|error| store_failure(&error))?
        } else {
            None
        };
        repo.ok_or_else(|| {
            debug!(path = %path.display(), "state of a removed repository dropped");
            (
                StatusCode::NOT_FOUND,
                repo_not_found(&path.display().to_string()),
            )
        })
    }

    /// Writes the last update of the repository at `path`, and its state when given.
    fn save_update(
        &mut self,
        path: &Path,
        update: &LastUpdate,
        state: Option<&RepoState>,
    ) -> Result<RepoSummary, Failure> {
        let saved = self
            .store
            .save_update(path, update, state)
            .map_err(|error| store_failure(&error))?;
        let repo = if saved {
            self.store
                .get_by_path(path)
                .map_err(|error| store_failure(&error))?
        } else {
            None
        };
        repo.ok_or_else(|| {
            debug!(path = %path.display(), "update of a removed repository dropped");
            (
                StatusCode::NOT_FOUND,
                repo_not_found(&path.display().to_string()),
            )
        })
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
    /// transaction. A rebuild ignores the index: it replaces it with what the roots hold. A dry
    /// run applies nothing.
    fn scan(&mut self, mode: ScanQuery, parent: &Span) -> Result<ScanResult, Failure> {
        let span = info_span!(
            parent: parent,
            "repo.scan",
            roots = self.discovery.roots.len(),
            rebuild = mode.rebuild,
            dry_run = mode.dry_run,
            added = Empty,
            updated = Empty,
            removed = Empty,
            errors = Empty,
            duration_ms = Empty,
        );
        let _entered = span.enter();
        let started = Instant::now();
        let registered = if mode.rebuild {
            Vec::new()
        } else {
            self.store
                .registered_paths()
                .map_err(|error| store_failure(&error))?
        };
        let previous: Vec<_> = self
            .store
            .repos()
            .map_err(|error| store_failure(&error))?
            .into_iter()
            .map(|repo| repo.identity)
            .collect();
        let options = self.options();
        let (report, scanned) = scan_with_registered(&self.discovery.roots, &registered, &options);
        let mut changes = if mode.rebuild {
            // Every repository found is new; every repository known is dropped.
            ScanDiff {
                removed: previous,
                ..diff(&[], &scanned, &report)
            }
        } else {
            diff(&previous, &scanned, &report)
        };
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
        if !mode.dry_run {
            let applied = if mode.rebuild {
                self.store.replace_all(&changes.added, now)
            } else {
                self.store.apply_scan(&changes, now)
            };
            applied.map_err(|error| store_failure(&error))?;
            for repo in changes.added.iter().chain(&changes.updated) {
                self.refresher.enqueue(repo.path.clone());
            }
        }
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
            rebuild = mode.rebuild,
            dry_run = mode.dry_run,
            "repositories scanned"
        );
        Ok(result)
    }

    /// Registers the repository holding `path`. On use, a repository already in the index is
    /// only marked used.
    fn register(
        &mut self,
        path: &Path,
        on_use: bool,
        parent: &Span,
    ) -> Result<RepoSummary, Failure> {
        let span = info_span!(
            parent: parent,
            "repo.register",
            path = %path.display(),
            on_use,
            slug = Empty
        );
        let _entered = span.enter();
        let bad = |message: String| (StatusCode::BAD_REQUEST, invalid_request(message));
        let root = find_root(path)
            .map_err(|error| bad(format!("cannot examine {}: {error}", path.display())))?
            .ok_or_else(|| bad(format!("no git repository at or above {}", path.display())))?;
        if on_use && let Some(known) = self.indexed_path(&root)? {
            let repo = self.mark_used(&known)?;
            span.record("slug", repo.identity.slug.as_str());
            debug!(slug = %repo.identity.slug, "repository used");
            return Ok(repo);
        }
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
        self.refresher.enqueue(summary.identity.path.clone());
        if on_use {
            return self.mark_used(&summary.identity.path);
        }
        Ok(summary)
    }

    /// The path under which the index knows the repository at `root`: `root` itself, or the
    /// path of a repository with the same name and the same canonical path (`root` reached
    /// through other symbolic links, such as the physical working directory of a shell).
    fn indexed_path(&self, root: &Path) -> Result<Option<PathBuf>, Failure> {
        if self
            .store
            .get_by_path(root)
            .map_err(|error| store_failure(&error))?
            .is_some()
        {
            return Ok(Some(root.to_path_buf()));
        }
        let (Some(name), Ok(canonical)) = (root.file_name(), fs::canonicalize(root)) else {
            return Ok(None);
        };
        let repos = self.store.repos().map_err(|error| store_failure(&error))?;
        Ok(repos
            .into_iter()
            .map(|repo| repo.identity.path)
            .filter(|path| path.file_name() == Some(name))
            .find(|path| fs::canonicalize(path).is_ok_and(|other| other == canonical)))
    }

    /// Sets `last_used` of the repository at `path` to now; the repository.
    fn mark_used(&mut self, path: &Path) -> Result<RepoSummary, Failure> {
        let now = now()?;
        self.store
            .set_last_used(path, now)
            .map_err(|error| store_failure(&error))?;
        self.store
            .get_by_path(path)
            .map_err(|error| store_failure(&error))?
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    repo_not_found(&path.display().to_string()),
                )
            })
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
