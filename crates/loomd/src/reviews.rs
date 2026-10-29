//! The review poller of loomd (ADR-0016, decisions 1 and 8): one thread polls, for each held
//! context and forge host with a credential, the MRs where I am a requested reviewer and the
//! MRs of its Review tasks, then hands what it fetched to the task thread, which writes the
//! tasks and the mirrors (`tasks::reviews`). The decisions are in `loomd::review_sync`.
//!
//! - Every `[forges."<host>"] poll_interval`, jittered by ±10 %, and on `POST /v1/reviews/sync`.
//! - GitHub: the search, then each MR with its `ETag`: a 304 costs nothing and changes nothing.
//!   GitLab: the list with `updated_after`, then each MR with its `ETag`.
//! - Reviews (my verdict) and versions are read when the MR changed; threads when it changed,
//!   and in full every hour.
//! - A rate limit waits for its reset, another failure backs off as automatic updates do; the
//!   other hosts go on, and the mirrors keep their last snapshot.

use std::collections::{BTreeMap, HashMap};
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use loom_api::{
    ContextName, ForgeKind, ReviewSync, ReviewSyncFailure, ReviewSyncOutcome, ReviewSyncResult,
    Timestamp,
};
use loom_config::config::Config;
use loom_config::forges::{ForgeCredential, ReviewRequests as ConfigRequests};
use loom_config::machine::MachineName;
use loom_forge::client::{
    ApiBase, Conditional, ForgeError, GithubClient, GithubForge as _, GitlabClient,
    GitlabForge as _, GitlabMr, ReviewRequests, Token,
};
use loom_forge::model::{Mr, MrForgeIds, MrKey, MrState, MrSummary, Sha, Thread};
use loom_forge::{MrContexts, mr_context};
use loom_repo::ContextRules;
use loom_store::files::TASKS_DIR;
use loom_store::{Reader, RepoFilter};
use loomd::api::timestamp;
use loomd::cli::VERSION_INFO;
use loomd::forge::check::ForgeTarget;
use loomd::forge::token::TokenResolver;
use loomd::hooks;
use loomd::recurring::{Runner, runner};
use loomd::review_sync::{
    Belongs, MR_FILE, MrBatch, MrUpdate, PollOutcome, THREADS_REFRESH, belongs, clone_of,
    finder_tells, github_verdict, gitlab_verdict, mirror_dir, next_poll, read_mr, tracked_reviews,
};
use loomd::shutdown::{JOIN_DEADLINE, join_within};
use miette::{Context as _, IntoDiagnostic as _, Result};
use tokio::sync::oneshot;
use tracing::{Span, debug, info, info_span, warn};

use crate::tasks::{ReviewsApplied, TaskHandle};
use crate::updater::random_unit;

/// How many other contexts have a credential for the host of `target`, held here or not:
/// another machine can poll them with the same account.
fn host_peers(credentials: &[ForgeCredential<'_>], target: &ForgeTarget) -> usize {
    credentials
        .iter()
        .filter(|other| other.host.as_str() == target.host && *other.context != target.context)
        .count()
}

/// An MR to read: its key, its summary when the search found it, and its Review task.
type Key = (MrKey, Option<MrSummary>, Option<loom_api::TaskId>);

/// The MRs that a poll reads, and the MRs found that it does not read.
struct ToRead {
    mrs: Vec<Key>,
    /// The MRs of another context.
    other_context: usize,
    /// The MRs of no context.
    unassigned: Vec<ReviewSyncFailure>,
}

/// The first poll of each host, after the start: the task store reconciles first.
const START_DELAY: Duration = Duration::from_secs(30);
/// GitLab `updated_after` is the start of the last poll that succeeded, minus this margin: a
/// change made while that poll ran is listed again.
const UPDATED_AFTER_MARGIN: Duration = Duration::from_mins(1);

/// A request to the poller.
enum Request {
    /// Poll the hosts of `context` (every host when `None`) now.
    Sync {
        context: Option<ContextName>,
        reply: oneshot::Sender<ReviewSyncResult>,
        parent: Span,
    },
    Stop,
}

/// The running poller.
pub struct ReviewPoller {
    requests: mpsc::Sender<Request>,
    stopping: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

/// What the API handlers use.
#[derive(Clone)]
pub struct ReviewHandle {
    requests: mpsc::Sender<Request>,
}

/// What the poller needs from the daemon.
pub struct Setup {
    /// Every forge credential of the held contexts.
    pub targets: Vec<ForgeTarget>,
    /// The root of each held context store.
    pub stores: BTreeMap<ContextName, PathBuf>,
    /// This machine.
    pub machine: MachineName,
    /// The user's home, for `~` in the context rules.
    pub user_home: PathBuf,
    /// The tokens in memory, shared with the credential check.
    pub resolver: Arc<TokenResolver>,
    /// The task thread.
    pub tasks: TaskHandle,
}

/// Whether this machine polls `target`: the first of `machines`, or any machine without them.
/// As for schedules: two machines would both create each Review task.
fn polled_here(
    target: &ForgeTarget,
    machines: Option<&[MachineName]>,
    machine: &MachineName,
) -> bool {
    match runner(machines, machine) {
        Runner::This => true,
        Runner::Other(first) => {
            info!(context = %target.context, host = %target.host, runner = %first, "MRs of the context are polled by another machine");
            false
        }
        Runner::Unpinned => {
            warn!(
                context = %target.context,
                host = %target.host,
                "MRs are polled on every machine holding the context: set `machines` so that only its first machine creates their Review tasks"
            );
            true
        }
    }
}

/// Starts the poller thread.
pub fn start(config: &Config, setup: Setup) -> Result<(ReviewPoller, ReviewHandle)> {
    let rules = ContextRules::new(config, &setup.user_home)
        .into_diagnostic()
        .wrap_err("cannot read the context rules for the MR polls")?;
    let start_delay = hooks::duration_hook(
        hooks::REVIEW_START_DELAY_ENV,
        env::var(hooks::REVIEW_START_DELAY_ENV).ok().as_deref(),
    )
    .unwrap_or_else(|message| {
        warn!("{message}");
        None
    })
    .unwrap_or(START_DELAY);
    let now = Instant::now();
    let mut hosts = Vec::new();
    let credentials = config.forge_credentials();
    for target in setup.targets {
        let peers = host_peers(&credentials, &target);
        let machines = config
            .contexts
            .get(&target.context)
            .and_then(|context| context.machines.as_deref());
        if !polled_here(&target, machines, &setup.machine) {
            continue;
        }
        let forge = config
            .forges
            .iter()
            .find(|(host, _)| host.as_str() == target.host);
        let (interval, requests) = forge.map_or_else(
            || {
                (
                    loom_config::forges::DEFAULT_POLL_INTERVAL,
                    ReviewRequests::Teams,
                )
            },
            |(_, forge)| {
                let requests = match forge.review_requests {
                    ConfigRequests::Teams => ReviewRequests::Teams,
                    ConfigRequests::Direct => ReviewRequests::Direct,
                };
                (forge.poll_interval.get(), requests)
            },
        );
        // The `LOOM_TEST_FORGE_API` hook is already applied to the targets.
        let api = target.api.clone();
        hosts.push(Host {
            peers,
            next: now + start_delay,
            api,
            interval,
            requests,
            failures: 0,
            rate_limited: None,
            login: None,
            last_login: None,
            last_success: None,
            updated_after: None,
            mrs: HashMap::new(),
            target,
        });
    }
    if !hosts.is_empty() {
        info!(hosts = hosts.len(), "MR polls loaded");
    }
    let (requests, received) = mpsc::channel();
    let stopping = Arc::new(AtomicBool::new(false));
    let poller = Poller {
        stopping: Arc::clone(&stopping),
        hosts,
        rules,
        stores: setup.stores,
        resolver: setup.resolver,
        tasks: setup.tasks,
        user_agent: format!("loomd/{}", VERSION_INFO.version),
    };
    let thread = thread::Builder::new()
        .name("reviews".to_owned())
        .spawn(move || poller.run(&received))
        .into_diagnostic()
        .wrap_err("cannot start the review poller thread")?;
    Ok((
        ReviewPoller {
            requests: requests.clone(),
            stopping,
            thread,
        },
        ReviewHandle { requests },
    ))
}

impl ReviewPoller {
    /// Stops the thread: a poll that runs stops before its next forge call. Waits for it up to
    /// [`JOIN_DEADLINE`]: a forge call that runs can take longer, and writes nothing.
    pub fn stop(self) {
        self.stopping.store(true, Ordering::Relaxed);
        let _ = self.requests.send(Request::Stop);
        join_within(self.thread, JOIN_DEADLINE, "reviews");
    }
}

impl ReviewHandle {
    /// Polls the hosts of `context`, or of every context, now, and answers what each poll did.
    /// `None` when the poller is gone.
    pub async fn sync(&self, context: Option<ContextName>) -> Option<ReviewSyncResult> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(Request::Sync {
                context,
                reply,
                parent: Span::current(),
            })
            .ok()?;
        answer.await.ok()
    }
}

/// One context and forge host.
struct Host {
    target: ForgeTarget,
    /// How many other contexts have a credential for its host, held here or not.
    peers: usize,
    api: ApiBase,
    interval: Duration,
    requests: ReviewRequests,
    next: Instant,
    /// Failures in a row, other than rate limits.
    failures: u32,
    /// The rate limit holds until then.
    rate_limited: Option<(Instant, Timestamp)>,
    /// The login of the token, read once.
    login: Option<String>,
    /// The last login read, kept after a 401: it tells the account to the other contexts.
    last_login: Option<String>,
    last_success: Option<Timestamp>,
    /// GitLab `updated_after` of the next poll.
    updated_after: Option<Timestamp>,
    /// What the last polls read of each MR, by mirror directory name.
    mrs: HashMap<String, MrCache>,
}

/// What a poll remembers of an MR, in memory: a restart reads every MR in full once.
#[derive(Debug, Default)]
struct MrCache {
    etag: Option<String>,
    head: Option<Sha>,
    /// I am in the reviewers of the MR (not my teams), at its last full read.
    direct: bool,
    /// The project of a GitLab merge request.
    project_id: Option<u64>,
    threads_at: Option<Instant>,
}

struct Poller {
    /// Set when loomd stops: a poll stops before its next forge call.
    stopping: Arc<AtomicBool>,
    hosts: Vec<Host>,
    rules: ContextRules,
    stores: BTreeMap<ContextName, PathBuf>,
    resolver: Arc<TokenResolver>,
    tasks: TaskHandle,
    user_agent: String,
}

/// Why a poll of a host stopped.
enum Stopped {
    Forge(ForgeError),
    Other(String),
}

impl From<ForgeError> for Stopped {
    fn from(error: ForgeError) -> Self {
        Self::Forge(error)
    }
}

/// A failure of one MR stops the poll of its host when it is not the MR's own: the host is
/// unreachable, refuses the token or limits the rate.
const fn stops_the_host(error: &ForgeError) -> bool {
    match error {
        ForgeError::Unauthorized { .. }
        | ForgeError::RateLimited { .. }
        | ForgeError::SsoRequired { .. }
        | ForgeError::Transport(_) => true,
        ForgeError::BadRequest { .. }
        | ForgeError::InsufficientScope { .. }
        | ForgeError::Forbidden { .. }
        | ForgeError::NotFound { .. }
        | ForgeError::ApprovalRefused { .. }
        | ForgeError::Conflict { .. }
        | ForgeError::Unprocessable { .. }
        | ForgeError::Status { .. }
        | ForgeError::Graphql { .. }
        | ForgeError::Parse(_)
        | ForgeError::Unexpected(_) => false,
    }
}

/// What the poll of a host fetched.
struct Fetched {
    updates: Vec<MrUpdate>,
    other_context: usize,
    failed: Vec<ReviewSyncFailure>,
    /// The keys of the MRs without a Review task.
    untracked: Vec<String>,
    /// Why the poll stopped before its last MR: the MRs read before are applied all the same.
    stopped: Option<Stopped>,
}

impl Poller {
    fn stopping(&self) -> bool {
        self.stopping.load(Ordering::Relaxed)
    }

    /// Stops a poll when loomd stops.
    fn go_on(&self) -> Result<(), Stopped> {
        if self.stopping() {
            Err(Stopped::Other("loomd is stopping".to_owned()))
        } else {
            Ok(())
        }
    }

    fn run(mut self, received: &mpsc::Receiver<Request>) {
        loop {
            let wait = self
                .hosts
                .iter()
                .map(|host| host.next.saturating_duration_since(Instant::now()))
                .min()
                .unwrap_or(Duration::from_hours(24));
            match received.recv_timeout(wait) {
                Ok(Request::Sync {
                    context,
                    reply,
                    parent,
                }) => {
                    let _span = info_span!(parent: &parent, "reviews.sync").entered();
                    let mut forges = Vec::new();
                    for index in 0..self.hosts.len() {
                        if self.stopping() {
                            break;
                        }
                        if context
                            .as_ref()
                            .is_none_or(|context| *context == self.hosts[index].target.context)
                        {
                            forges.push(self.poll(index));
                        }
                    }
                    let _ = reply.send(ReviewSyncResult { forges });
                }
                Ok(Request::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {
                    for index in 0..self.hosts.len() {
                        if self.stopping() {
                            return;
                        }
                        if self.hosts[index].next <= Instant::now() {
                            let _ = self.poll(index);
                        }
                    }
                }
            }
        }
    }

    /// Polls host `index` and schedules its next poll.
    fn poll(&mut self, index: usize) -> ReviewSync {
        let started = now();
        let span = info_span!(
            "reviews.poll",
            context = %self.hosts[index].target.context,
            host = %self.hosts[index].target.host
        );
        let _entered = span.enter();
        let host = &self.hosts[index];
        let outcome = match host.rate_limited {
            Some((until, at)) if until > Instant::now() => {
                debug!(until = %at, "rate limited, poll skipped");
                ReviewSyncOutcome::RateLimited { until: at }
            }
            Some(_) | None => self.poll_now(index, started),
        };
        let host = &mut self.hosts[index];
        let (poll, failed) = match &outcome {
            ReviewSyncOutcome::Synced { .. } => (PollOutcome::Done, false),
            ReviewSyncOutcome::RateLimited { until } => (
                PollOutcome::RateLimited(Duration::from_secs(
                    until.unix_secs().saturating_sub(started.unix_secs()),
                )),
                false,
            ),
            ReviewSyncOutcome::Failed { .. } => (
                PollOutcome::Failed {
                    failures: host.failures.saturating_add(1),
                },
                true,
            ),
        };
        host.failures = if failed {
            host.failures.saturating_add(1)
        } else {
            0
        };
        host.next = Instant::now() + next_poll(host.interval, poll, random_unit());
        ReviewSync {
            context: host.target.context.clone(),
            host: host.target.host.clone(),
            kind: host.target.kind,
            last_success: host.last_success,
            outcome,
        }
    }

    fn poll_now(&mut self, index: usize, started: Timestamp) -> ReviewSyncOutcome {
        let target = self.hosts[index].target.clone();
        let token = match self.resolver.resolve(&target.source) {
            Ok(token) => token,
            Err(error) => {
                warn!(%error, "cannot resolve the forge token, MRs not polled");
                return ReviewSyncOutcome::Failed {
                    error: error.to_string(),
                };
            }
        };
        let fetched = match target.kind {
            ForgeKind::Github => {
                let client =
                    GithubClient::new(&self.hosts[index].api, token.clone(), &self.user_agent);
                self.poll_github(index, &client)
            }
            ForgeKind::Gitlab => {
                let client =
                    GitlabClient::new(&self.hosts[index].api, token.clone(), &self.user_agent);
                self.poll_gitlab(index, &client)
            }
        };
        match fetched {
            Ok(mut fetched) => {
                for failure in &mut fetched.failed {
                    failure.error = token.redact(&failure.error);
                    warn!(key = %failure.key, error = %failure.error, "cannot poll an MR, its mirror keeps its last snapshot");
                }
                match fetched.stopped.take() {
                    // One MR that stops the host does not hold back the MRs read before it.
                    Some(stopped) => {
                        let _ = self.hand_over(index, fetched.updates);
                        self.stopped(index, stopped, &token)
                    }
                    None => self.apply(index, fetched, started),
                }
            }
            Err(stopped) => self.stopped(index, stopped, &token),
        }
    }

    /// Hands `updates` of host `index` to the task thread. What the task thread did not write
    /// is read in full next time, or its cached `ETag` would hide the change with a 304.
    fn hand_over(&mut self, index: usize, updates: Vec<MrUpdate>) -> Option<ReviewsApplied> {
        let keys: Vec<String> = updates.iter().map(|update| update.key.dir_name()).collect();
        let host = &mut self.hosts[index];
        let batch = MrBatch {
            context: host.target.context.clone(),
            host: host.target.host.clone(),
            updates,
        };
        let Some(applied) = self.tasks.sync_reviews(batch) else {
            for key in &keys {
                host.mrs.remove(key);
            }
            return None;
        };
        for failure in &applied.failed {
            if let Ok(key) = MrKey::parse(&failure.key) {
                host.mrs.remove(&key.dir_name());
            }
        }
        Some(applied)
    }

    /// Hands the MRs fetched from host `index` to the task thread.
    fn apply(&mut self, index: usize, fetched: Fetched, started: Timestamp) -> ReviewSyncOutcome {
        let mrs = fetched.updates.len() + fetched.failed.len();
        let Some(applied) = self.hand_over(index, fetched.updates) else {
            return ReviewSyncOutcome::Failed {
                error: "loomd is stopping".to_owned(),
            };
        };
        let host = &mut self.hosts[index];
        let mut failed = fetched.failed;
        failed.extend(applied.failed);
        host.last_success = Some(started);
        // An MR that failed before its task exists is listed again only from the same time; an
        // MR with a task is polled one by one anyway.
        if !failed
            .iter()
            .any(|failure| fetched.untracked.contains(&failure.key))
        {
            host.updated_after = Timestamp::from_unix_secs(
                started
                    .unix_secs()
                    .saturating_sub(UPDATED_AFTER_MARGIN.as_secs()),
            );
        }
        host.rate_limited = None;
        info!(
            mrs,
            created = applied.created.len(),
            changed = applied.changed.len(),
            failed = failed.len(),
            "MRs polled"
        );
        ReviewSyncOutcome::Synced {
            mrs,
            created: applied.created,
            changed: applied.changed,
            other_context: fetched.other_context,
            failed,
        }
    }

    /// The outcome of a poll of host `index` that stopped: the mirrors keep their snapshot.
    fn stopped(&mut self, index: usize, stopped: Stopped, token: &Token) -> ReviewSyncOutcome {
        let host = &mut self.hosts[index];
        let error = match stopped {
            Stopped::Forge(ForgeError::RateLimited { wait, .. }) => {
                let now = now();
                let wait = wait.duration(now);
                let until =
                    Timestamp::from_unix_secs(now.unix_secs().saturating_add(wait.as_secs()))
                        .unwrap_or(now);
                warn!(%until, "the forge limits the rate, MRs polled after the reset");
                host.rate_limited = Some((Instant::now() + wait, until));
                return ReviewSyncOutcome::RateLimited { until };
            }
            Stopped::Forge(error @ ForgeError::Unauthorized { .. }) => {
                // Resolved again at the next poll: it may have been renewed.
                self.resolver.invalidate(&host.target.source, token);
                host.login = None;
                error.to_string()
            }
            Stopped::Forge(error) => error.to_string(),
            Stopped::Other(error) => error,
        };
        let error = token.redact(&error);
        warn!(
            error = error.as_str(),
            "MR poll failed, the mirrors keep their last snapshot"
        );
        ReviewSyncOutcome::Failed { error }
    }

    /// The login of the token of host `index`.
    fn login(
        &mut self,
        index: usize,
        client: &dyn loom_forge::client::Forge,
    ) -> Result<String, Stopped> {
        if let Some(login) = &self.hosts[index].login {
            return Ok(login.clone());
        }
        let login = client.user()?.user.login;
        self.hosts[index].login = Some(login.clone());
        self.hosts[index].last_login = Some(login.clone());
        Ok(login)
    }

    /// The MR of the mirror of task `id`, in the store of host `index`.
    fn mirror_mr(&self, index: usize, id: &loom_api::TaskId, key: &MrKey) -> Option<Mr> {
        let root = self.stores.get(&self.hosts[index].target.context)?;
        let dir = mirror_dir(&root.join(TASKS_DIR).join(id.as_str()), key);
        read_mr(&std::fs::read(dir.join(MR_FILE)).ok()?, "")
    }

    /// Where an MR found by the search of host `index` goes.
    fn ours(&self, index: usize, key: &MrKey, repos: &[loom_api::RepoSummary]) -> Belongs {
        let target = &self.hosts[index].target;
        let clone = clone_of(repos, &target.host, key.project());
        let contexts = MrContexts {
            clone: clone.map(|repo| &repo.identity.context),
            rules: &self.rules,
            finder: &target.context,
        };
        let context = mr_context(&target.host, key.project(), &contexts);
        let polled: Vec<Option<&str>> = self
            .hosts
            .iter()
            .enumerate()
            .filter(|(other, host)| *other != index && host.target.host == target.host)
            .map(|(_, host)| host.last_login.as_deref())
            .collect();
        let tells = finder_tells(
            self.hosts[index].last_login.as_deref(),
            self.hosts[index].peers,
            &polled,
        );
        let belongs = belongs(&context, &target.context, !tells);
        if belongs == Belongs::Elsewhere {
            debug!(key = %key, other = %context.context(), "MR of another context, not written here");
        }
        belongs
    }

    /// The MRs found by the search that belong to the context of host `index`, then the MRs of
    /// its Review tasks: `(key, found, task)`.
    fn keys(&self, index: usize, found: Vec<MrSummary>) -> Result<ToRead, Stopped> {
        let target = &self.hosts[index].target;
        let reader = Reader::open(self.tasks.index_db())
            .map_err(|error| Stopped::Other(format!("cannot read the index: {error}")))?;
        let repos = reader
            .list(&RepoFilter::default())
            .map_err(|error| Stopped::Other(format!("cannot read the repositories: {error}")))?;
        let rows = reader
            .tasks(Some(&target.context))
            .map_err(|error| Stopped::Other(format!("cannot read the tasks: {error}")))?;
        let tracked = tracked_reviews(&rows, &target.context, &target.host);
        let mut other_context = 0;
        let mut unassigned = Vec::new();
        let mut keys: Vec<Key> = Vec::new();
        for summary in found {
            if keys.iter().any(|(key, _, _)| *key == summary.key) {
                continue;
            }
            let task = tracked
                .iter()
                .find(|(_, key, _)| *key == summary.key)
                .map(|(id, _, _)| id.clone());
            // A tracked MR stays in its task's context: a task does not move.
            if task.is_none() {
                match self.ours(index, &summary.key, &repos) {
                    Belongs::Here => {}
                    Belongs::Elsewhere => {
                        other_context += 1;
                        continue;
                    }
                    Belongs::Unassigned => {
                        unassigned.push(ReviewSyncFailure {
                            key: summary.key.to_string(),
                            error: format!(
                                "no context for {}/{}: other contexts poll {}, maybe with the same account (or their login is not read yet); add a match.hosts rule or clone the project",
                                target.host,
                                summary.key.project(),
                                target.host
                            ),
                        });
                        continue;
                    }
                }
            }
            keys.push((summary.key.clone(), Some(summary), task));
        }
        for (id, key, closed) in tracked {
            if keys.iter().any(|(known, _, _)| *known == key) {
                continue;
            }
            // A closed task of a merged or closed MR has nothing more to sync until archived.
            if closed
                && self
                    .mirror_mr(index, &id, &key)
                    .is_some_and(|mr| mr.state != MrState::Open)
            {
                continue;
            }
            keys.push((key, None, Some(id)));
        }
        Ok(ToRead {
            mrs: keys,
            other_context,
            unassigned,
        })
    }

    fn poll_github(&mut self, index: usize, client: &GithubClient) -> Result<Fetched, Stopped> {
        self.go_on()?;
        let login = self.login(index, client)?;
        let page = client.review_requests(self.hosts[index].requests)?;
        if page.incomplete_results {
            debug!("GitHub search results are incomplete");
        }
        let incomplete = page.incomplete_results;
        let ToRead {
            mrs: keys,
            other_context,
            unassigned,
        } = self.keys(index, page.items)?;
        let mut untracked = untracked(&keys);
        untracked.extend(unassigned.iter().map(|failure| failure.key.clone()));
        let mut fetched = Fetched {
            updates: Vec::new(),
            other_context,
            failed: unassigned,
            untracked,
            stopped: None,
        };
        for (key, found, _) in keys {
            if let Err(stopped) = self.go_on() {
                fetched.stopped = Some(stopped);
                break;
            }
            // Missing from an incomplete search: unknown.
            let found = match found {
                Some(_) => Some(true),
                None if incomplete => None,
                None => Some(false),
            };
            let cache = self.hosts[index].mrs.entry(key.dir_name()).or_default();
            match github_update(client, &key, found, &login, cache) {
                Ok(update) => fetched.updates.push(update),
                Err(error) if stops_the_host(&error) => {
                    fetched.stopped = Some(error.into());
                    break;
                }
                Err(error) => {
                    self.hosts[index].mrs.remove(&key.dir_name());
                    fetched.failed.push(failure(&key, &error));
                }
            }
        }
        Ok(fetched)
    }

    fn poll_gitlab(&mut self, index: usize, client: &GitlabClient) -> Result<Fetched, Stopped> {
        self.go_on()?;
        let login = self.login(index, client)?;
        let listed = client.review_requests(self.hosts[index].updated_after)?;
        let ToRead {
            mrs: keys,
            other_context,
            unassigned,
        } = self.keys(index, listed)?;
        let mut untracked = untracked(&keys);
        untracked.extend(unassigned.iter().map(|failure| failure.key.clone()));
        let mut fetched = Fetched {
            updates: Vec::new(),
            other_context,
            failed: unassigned,
            untracked,
            stopped: None,
        };
        for (key, found, task) in keys {
            if let Err(stopped) = self.go_on() {
                fetched.stopped = Some(stopped);
                break;
            }
            let mirror_project = || project_of(&self.mirror_mr(index, task.as_ref()?, &key)?);
            let project_id = self.hosts[index]
                .mrs
                .get(&key.dir_name())
                .and_then(|cache| cache.project_id)
                .or_else(|| found.as_ref().and_then(|summary| summary.project_id))
                .or_else(mirror_project);
            let Some(project_id) = project_id else {
                fetched.failed.push(ReviewSyncFailure {
                    key: key.to_string(),
                    error: "the ID of the GitLab project is unknown: it is read again when the MR is listed".to_owned(),
                });
                continue;
            };
            let cache = self.hosts[index].mrs.entry(key.dir_name()).or_default();
            cache.project_id = Some(project_id);
            let mr = GitlabMr {
                project_id,
                iid: key.number(),
            };
            match gitlab_update(client, &key, mr, Some(found.is_some()), &login, cache) {
                Ok(update) => fetched.updates.push(update),
                Err(error) if stops_the_host(&error) => {
                    fetched.stopped = Some(error.into());
                    break;
                }
                Err(error) => {
                    self.hosts[index].mrs.remove(&key.dir_name());
                    fetched.failed.push(failure(&key, &error));
                }
            }
        }
        Ok(fetched)
    }
}

/// The keys of the MRs of `keys` without a Review task.
fn untracked(keys: &[Key]) -> Vec<String> {
    keys.iter()
        .filter(|(_, _, task)| task.is_none())
        .map(|(key, _, _)| key.to_string())
        .collect()
}

/// Whether I am a requested reviewer: `found` by the search (`None`: the search cannot tell),
/// or `direct`ly in the reviewers.
const fn requested(found: Option<bool>, direct: bool) -> Option<bool> {
    match found {
        Some(true) => Some(true),
        _ if direct => Some(true),
        other => other,
    }
}

fn failure(key: &MrKey, error: &ForgeError) -> ReviewSyncFailure {
    ReviewSyncFailure {
        key: key.to_string(),
        error: error.to_string(),
    }
}

/// The project of a GitLab merge request.
const fn project_of(mr: &Mr) -> Option<u64> {
    match mr.forge_ids {
        MrForgeIds::Gitlab { project_id, .. } => Some(project_id),
        MrForgeIds::Github { .. } => None,
    }
}

/// Whether the threads of an MR are due: never pulled, or pulled an hour ago.
fn threads_due(cache: &MrCache) -> bool {
    cache
        .threads_at
        .is_none_or(|at| at.elapsed() >= THREADS_REFRESH)
}

fn is_reviewer(mr: &Mr, login: &str) -> bool {
    mr.reviewers
        .iter()
        .any(|reviewer| reviewer.eq_ignore_ascii_case(login))
}

/// The GitHub pull request `key`: in full when it changed since the `ETag` of `cache`.
fn github_update(
    client: &GithubClient,
    key: &MrKey,
    found: Option<bool>,
    login: &str,
    cache: &mut MrCache,
) -> Result<MrUpdate, ForgeError> {
    let threads = |client: &GithubClient| -> Result<Vec<Thread>, ForgeError> {
        let mut threads = client.review_threads(key)?.threads;
        threads.extend(client.issue_comments(key)?);
        Ok(threads)
    };
    match client.pull(key, cache.etag.as_deref())? {
        Conditional::Modified { value: mr, etag } => {
            let verdict = github_verdict(&client.reviews(key)?, login);
            let threads = threads(client)?;
            cache.etag = etag;
            cache.head = Some(mr.version.head_sha.clone());
            cache.direct = is_reviewer(&mr, login);
            cache.threads_at = Some(Instant::now());
            Ok(MrUpdate {
                key: key.clone(),
                requested: requested(found, cache.direct),
                versions: vec![mr.version.clone()],
                mr: Some(mr),
                threads: Some(threads),
                verdict: Some(verdict),
            })
        }
        Conditional::NotModified => {
            // Hourly, the threads and my verdict, in case they changed without the MR.
            let (threads, verdict) = if threads_due(cache) {
                let threads = threads(client)?;
                let verdict = github_verdict(&client.reviews(key)?, login);
                cache.threads_at = Some(Instant::now());
                (Some(threads), Some(verdict))
            } else {
                (None, None)
            };
            Ok(MrUpdate {
                key: key.clone(),
                requested: requested(found, cache.direct),
                mr: None,
                versions: Vec::new(),
                threads,
                verdict,
            })
        }
    }
}

/// The GitLab merge request `key`: in full when it changed since the `ETag` of `cache`.
fn gitlab_update(
    client: &GitlabClient,
    key: &MrKey,
    reference: GitlabMr,
    found: Option<bool>,
    login: &str,
    cache: &mut MrCache,
) -> Result<MrUpdate, ForgeError> {
    match client.merge_request(reference, cache.etag.as_deref())? {
        Conditional::Modified { value: mr, etag } => {
            let head = mr.version.head_sha.clone();
            let versions = client.versions(reference)?;
            let approvals = client.approvals(reference)?;
            let threads = client.discussions(reference, &head)?;
            cache.etag = etag;
            cache.direct = is_reviewer(&mr, login);
            cache.threads_at = Some(Instant::now());
            cache.head = Some(head.clone());
            Ok(MrUpdate {
                key: key.clone(),
                requested: requested(found, cache.direct),
                mr: Some(mr),
                versions,
                threads: Some(threads),
                verdict: Some(gitlab_verdict(approvals.user_has_approved, &head)),
            })
        }
        Conditional::NotModified => {
            // Hourly, the threads and my approval, in case they changed without the MR.
            let (threads, verdict) = match &cache.head {
                Some(head) if threads_due(cache) => {
                    let threads = client.discussions(reference, head)?;
                    let approvals = client.approvals(reference)?;
                    cache.threads_at = Some(Instant::now());
                    (
                        Some(threads),
                        Some(gitlab_verdict(approvals.user_has_approved, head)),
                    )
                }
                Some(_) | None => (None, None),
            };
            Ok(MrUpdate {
                key: key.clone(),
                requested: requested(found, cache.direct),
                mr: None,
                versions: Vec::new(),
                threads,
                verdict,
            })
        }
    }
}

/// The current time; the epoch when the clock is out of range.
fn now() -> Timestamp {
    timestamp(SystemTime::now()).unwrap_or(Timestamp::EPOCH)
}
