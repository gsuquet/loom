//! Automatic repository updates (ADR-0012): a coordinator thread decides, with
//! [`loomd::schedule::Schedule`], which repositories to update and when; `[repos]
//! fetch_concurrency` worker threads run [`loomd::repo_update::update`] and write the result
//! through the store thread. `POST /v1/repos/sync` goes through the same coordinator, so a
//! repository is never updated twice at once; it ignores the schedule and the backoff.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use loom_api::{LastUpdate, RepoSummary, RepoSync, SyncOutcome, Timestamp};
use loom_repo::{StateOptions, UpdatePolicies};
use loom_store::{Reader, RepoFilter};
use loomd::api::timestamp;
use loomd::repo_update::{FetchStatus, GitRunner, Report, Target, update};
use loomd::schedule::{Candidate, Schedule};
use tokio::sync::oneshot;
use tracing::{Span, debug, info, warn};

use crate::repos::{Command, lock};
use crate::updater::random_unit;

/// Longest time between two reads of the index by the coordinator: new repositories are
/// scheduled within it.
const LIST_EVERY: Duration = Duration::from_secs(10);
/// The same while the index is empty, as on start before the first scan ends.
const LIST_EVERY_WHILE_EMPTY: Duration = Duration::from_secs(1);

/// Most worker threads, whatever `fetch_concurrency` says.
pub const MAX_WORKERS: usize = 64;

/// A repository to update, and who asked.
struct Job {
    repo: RepoSummary,
    host: String,
    /// The sync request waiting for it and the position of the result; `None` for the
    /// schedule.
    sync: Option<(u64, usize)>,
    parent: Span,
}

enum Message {
    Sync {
        repos: Vec<RepoSummary>,
        reply: oneshot::Sender<Vec<RepoSync>>,
        parent: Span,
    },
    Finished {
        job: Box<Job>,
        started: Instant,
        update: Box<LastUpdate>,
        fetch: FetchStatus,
    },
    Stop,
}

/// The running coordinator.
pub struct Updater {
    messages: mpsc::Sender<Message>,
    thread: JoinHandle<()>,
}

/// What the API uses to run updates now.
#[derive(Clone)]
pub struct UpdaterHandle {
    messages: mpsc::Sender<Message>,
    /// The update policies, also used for dry runs.
    pub policies: Arc<UpdatePolicies>,
}

/// Settings of the updates.
pub struct Settings {
    pub interval: Duration,
    pub concurrency: usize,
    pub policies: UpdatePolicies,
    pub runner: GitRunner,
    pub state_options: StateOptions,
}

/// Starts the coordinator and the workers. Results are written through `store`; the index is
/// read from `db`.
pub fn start(
    settings: Settings,
    store: &mpsc::Sender<Command>,
    db: PathBuf,
) -> std::io::Result<(Updater, UpdaterHandle)> {
    let workers = settings.concurrency.clamp(1, MAX_WORKERS);
    if workers < settings.concurrency {
        warn!(
            fetch_concurrency = settings.concurrency,
            workers, "fetch_concurrency capped"
        );
    }
    let (messages, received) = mpsc::channel();
    let (jobs, queued) = mpsc::channel::<Job>();
    let queued = Arc::new(Mutex::new(queued));
    let runner = Arc::new(settings.runner);
    for index in 0..workers {
        let queued = Arc::clone(&queued);
        let runner = Arc::clone(&runner);
        let store = store.clone();
        let messages = messages.clone();
        let policies = settings.policies.clone();
        let options = settings.state_options;
        // Never joined: at shutdown a running git command is left to finish on its own (it has
        // its own process group), rather than interrupted.
        thread::Builder::new()
            .name(format!("repo-update-{index}"))
            .spawn(move || {
                loop {
                    let next = lock(&queued).recv();
                    let Ok(job) = next else { return };
                    let started = Instant::now();
                    let report = run(&job, &policies, &runner, &options);
                    save(&store, &job, &report);
                    let message = Message::Finished {
                        job: Box::new(job),
                        started,
                        update: Box::new(report.update),
                        fetch: report.fetch,
                    };
                    if messages.send(message).is_err() {
                        return;
                    }
                }
            })?;
    }
    let policies = Arc::new(settings.policies);
    let coordinator = Coordinator {
        schedule: Schedule::new(settings.interval),
        slots: workers,
        running: 0,
        jobs,
        db,
        listed: Vec::new(),
        listed_at: None,
        waiting: VecDeque::new(),
        syncs: HashMap::new(),
        next_sync: 0,
    };
    let thread = thread::Builder::new()
        .name("repo-updates".to_owned())
        .spawn(move || coordinator.run(&received))?;
    info!(
        workers,
        interval_secs = settings.interval.as_secs(),
        "repository updates started"
    );
    Ok((
        Updater {
            messages: messages.clone(),
            thread,
        },
        UpdaterHandle { messages, policies },
    ))
}

impl Updater {
    /// Stops the coordinator. Updates already running finish in the background; their results
    /// are written only if the store is still running.
    pub fn stop(self) {
        let _ = self.messages.send(Message::Stop);
        if self.thread.join().is_err() {
            warn!("the repository update thread panicked");
        }
    }
}

impl UpdaterHandle {
    /// Updates `repos` now, after the updates running on them; their results, in the order of
    /// `repos`. `None` when loomd stops first.
    pub async fn sync(&self, repos: Vec<RepoSummary>) -> Option<Vec<RepoSync>> {
        let (reply, answer) = oneshot::channel();
        self.messages
            .send(Message::Sync {
                repos,
                reply,
                parent: Span::current(),
            })
            .ok()?;
        answer.await.ok()
    }
}

/// The update of `job`.
fn run(job: &Job, policies: &UpdatePolicies, runner: &GitRunner, options: &StateOptions) -> Report {
    let _entered = job.parent.clone().entered();
    let identity = &job.repo.identity;
    let policy = policies.policy(&identity.path, &identity.context);
    update(
        Target {
            path: &identity.path,
            slug: &identity.slug,
            primary_remote: identity.primary_remote.as_deref(),
            policy,
        },
        runner,
        options,
        &now,
    )
}

/// Writes the result of `job` through the store thread and waits for it, so that a sync
/// answers after its results are readable.
fn save(store: &mpsc::Sender<Command>, job: &Job, report: &Report) {
    let (reply, answer) = oneshot::channel();
    let command = Command::SaveUpdate {
        path: job.repo.identity.path.clone(),
        update: Box::new(report.update.clone()),
        state: report.state.clone().map(Box::new),
        reply,
    };
    if store.send(command).is_err() {
        debug!("store stopped: update result dropped");
        return;
    }
    match answer.blocking_recv() {
        Ok(Ok(_)) => {}
        Ok(Err((_, error))) => {
            debug!(slug = %job.repo.identity.slug, error = %error.message, "update result not stored");
        }
        Err(_) => debug!("store stopped: update result dropped"),
    }
}

fn now() -> Timestamp {
    timestamp(SystemTime::now()).unwrap_or(Timestamp::EPOCH)
}

/// The key of a repository's host for the backoff: `host[:port]` of its primary remote, its
/// URL when it has no host (a local path), empty without remote.
fn host(repo: &RepoSummary) -> String {
    let identity = &repo.identity;
    let primary = identity
        .primary_remote
        .as_deref()
        .and_then(|name| identity.remotes.iter().find(|remote| remote.name == name));
    primary.map_or_else(String::new, |remote| {
        remote.location.as_ref().map_or_else(
            || remote.url.clone(),
            |location| {
                location.port.map_or_else(
                    || location.host.clone(),
                    |port| format!("{}:{port}", location.host),
                )
            },
        )
    })
}

/// A sync request waiting for its updates.
struct PendingSync {
    results: Vec<Option<RepoSync>>,
    remaining: usize,
    reply: oneshot::Sender<Vec<RepoSync>>,
}

struct Coordinator {
    schedule: Schedule,
    /// Workers.
    slots: usize,
    /// Jobs given to the workers and not finished.
    running: usize,
    jobs: mpsc::Sender<Job>,
    db: PathBuf,
    /// The index as last read.
    listed: Vec<RepoSummary>,
    listed_at: Option<Instant>,
    /// Sync jobs waiting for a worker or for their repository's running update.
    waiting: VecDeque<Job>,
    syncs: HashMap<u64, PendingSync>,
    next_sync: u64,
}

impl Coordinator {
    fn run(mut self, received: &mpsc::Receiver<Message>) {
        loop {
            self.dispatch();
            let wait = self
                .schedule
                .next_due()
                .map_or(LIST_EVERY, |due| {
                    due.saturating_duration_since(Instant::now())
                })
                .min(self.list_every());
            match received.recv_timeout(wait) {
                Ok(Message::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Ok(Message::Sync {
                    repos,
                    reply,
                    parent,
                }) => self.queue_sync(repos, reply, &parent),
                Ok(Message::Finished {
                    job,
                    started,
                    update,
                    fetch,
                }) => self.finished(*job, started, *update, fetch),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn queue_sync(
        &mut self,
        repos: Vec<RepoSummary>,
        reply: oneshot::Sender<Vec<RepoSync>>,
        parent: &Span,
    ) {
        let id = self.next_sync;
        self.next_sync += 1;
        if repos.is_empty() {
            let _ = reply.send(Vec::new());
            return;
        }
        self.syncs.insert(
            id,
            PendingSync {
                results: vec![None; repos.len()],
                remaining: repos.len(),
                reply,
            },
        );
        for (index, repo) in repos.into_iter().enumerate() {
            let host = host(&repo);
            self.waiting.push_back(Job {
                repo,
                host,
                sync: Some((id, index)),
                parent: parent.clone(),
            });
        }
    }

    fn finished(&mut self, job: Job, started: Instant, update: LastUpdate, fetch: FetchStatus) {
        self.running = self.running.saturating_sub(1);
        let path = &job.repo.identity.path;
        self.schedule.finish(
            path,
            &job.host,
            started,
            Instant::now(),
            fetch,
            &mut random_unit,
        );
        if let Some((until, failures)) = self.schedule.host_backoff(&job.host)
            && fetch == FetchStatus::Failed
        {
            warn!(
                host = %job.host,
                failures,
                backoff_secs = until.saturating_duration_since(Instant::now()).as_secs(),
                "fetches of this host back off"
            );
        }
        let Some((id, index)) = job.sync else { return };
        let Some(pending) = self.syncs.get_mut(&id) else {
            return;
        };
        if let Some(result) = pending.results.get_mut(index) {
            *result = Some(RepoSync {
                slug: job.repo.identity.slug,
                result: SyncOutcome::Updated { update },
            });
        }
        pending.remaining = pending.remaining.saturating_sub(1);
        if pending.remaining == 0
            && let Some(pending) = self.syncs.remove(&id)
        {
            let _ = pending
                .reply
                .send(pending.results.into_iter().flatten().collect());
        }
    }

    /// Gives free workers the waiting sync jobs first, then the due repositories.
    fn dispatch(&mut self) {
        let mut kept = VecDeque::new();
        while let Some(job) = self.waiting.pop_front() {
            if self.running < self.slots && !self.schedule.is_running(&job.repo.identity.path) {
                self.start(job);
            } else {
                kept.push_back(job);
            }
        }
        self.waiting = kept;
        if self.running >= self.slots {
            return;
        }
        self.list();
        let unix_now = now().unix_secs();
        let hosts: Vec<String> = self.listed.iter().map(host).collect();
        let candidates: Vec<Candidate<'_>> = self
            .listed
            .iter()
            .zip(&hosts)
            .map(|(repo, host)| Candidate {
                path: &repo.identity.path,
                host,
                last_update: repo
                    .last_update
                    .as_ref()
                    .map(|update| update.time.unix_secs()),
            })
            .collect();
        let due = self.schedule.due(
            &candidates,
            Instant::now(),
            unix_now,
            self.slots - self.running,
            &mut random_unit,
        );
        let by_path: HashMap<&PathBuf, &RepoSummary> = self
            .listed
            .iter()
            .map(|repo| (&repo.identity.path, repo))
            .collect();
        let jobs: Vec<Job> = due
            .iter()
            .filter_map(|path| by_path.get(path))
            .map(|repo| Job {
                repo: (*repo).clone(),
                host: host(repo),
                sync: None,
                parent: Span::none(),
            })
            .collect();
        for job in jobs {
            self.start(job);
        }
    }

    fn start(&mut self, job: Job) {
        self.schedule.start(&job.repo.identity.path);
        self.running += 1;
        if self.jobs.send(job).is_err() {
            warn!("the repository update workers stopped");
        }
    }

    /// Time between two reads of the index.
    const fn list_every(&self) -> Duration {
        if self.listed.is_empty() {
            LIST_EVERY_WHILE_EMPTY
        } else {
            LIST_EVERY
        }
    }

    /// Reads the index again when the last read is older than [`Self::list_every`].
    fn list(&mut self) {
        if self
            .listed_at
            .is_some_and(|at| at.elapsed() < self.list_every())
        {
            return;
        }
        match Reader::open(&self.db).and_then(|reader| reader.list(&RepoFilter::default())) {
            Ok(repos) => {
                self.listed = repos;
                self.listed_at = Some(Instant::now());
            }
            Err(error) => warn!(%error, "cannot list the repositories to update"),
        }
    }
}
