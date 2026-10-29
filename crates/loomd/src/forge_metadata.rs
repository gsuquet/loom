//! The forge poll of the repo cards (docs/design/reviews.md, Repo forge metadata): a thread
//! reads the index every few seconds, plans with [`loomd::forge::metadata::plan`], and reads
//! the projects of each credential every `[forges."<host>"] poll_interval` (jittered ±10 %),
//! and at once for a repository without forge metadata yet. Results are written through the
//! store thread.

use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use loom_api::Timestamp;
use loom_store::{Reader, RepoFilter};
use loomd::api::timestamp;
use loomd::forge::check::ForgeTarget;
use loomd::forge::metadata::{HttpProjects, plan, read_target, select};
use loomd::forge::token::TokenResolver;
use loomd::schedule::jittered;
use tracing::{debug, info, warn};

use crate::repos::Command;
use crate::updater::random_unit;

/// Time between two reads of the index: a new repository is read within it.
const TICK: Duration = Duration::from_secs(10);

/// What the forge poll reads with.
pub struct Settings {
    /// The credentials of the held contexts.
    pub targets: Vec<ForgeTarget>,
    /// `poll_interval` of each target, in the same order.
    pub intervals: Vec<Duration>,
    /// The tokens, shared with the credential check.
    pub resolver: Arc<TokenResolver>,
    /// The `User-Agent` of the forge calls.
    pub user_agent: String,
}

/// The running poll thread.
pub struct Poller {
    stop: mpsc::Sender<()>,
}

impl Poller {
    /// Stops the thread after the reads it is making. Not joined: a forge call can wait for
    /// its timeout, and its result is written only if the store still runs.
    pub fn stop(self) {
        let _ = self.stop.send(());
    }
}

/// Starts the poll thread. Results are written through `store`; the index is read from `db`.
pub fn start(
    settings: Settings,
    store: &mpsc::Sender<Command>,
    db: PathBuf,
) -> std::io::Result<Poller> {
    let (stop, stopped) = mpsc::channel();
    let store = store.clone();
    info!(
        credentials = settings.targets.len(),
        "forge metadata poll started"
    );
    thread::Builder::new()
        .name("forge-metadata".to_owned())
        .spawn(move || run(&settings, &store, &db, &stopped))?;
    Ok(Poller { stop })
}

fn run(
    settings: &Settings,
    store: &mpsc::Sender<Command>,
    db: &std::path::Path,
    stopped: &mpsc::Receiver<()>,
) {
    let api = HttpProjects {
        user_agent: settings.user_agent.clone(),
    };
    // Every credential is due on the first tick.
    let mut next_poll: Vec<Instant> = vec![Instant::now(); settings.targets.len()];
    // After a rate limit: nothing for the credential, new repositories included, until then.
    let mut paused_until: Vec<Option<Instant>> = vec![None; settings.targets.len()];
    let mut reader: Option<Reader> = None;
    loop {
        match stopped.recv_timeout(TICK) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if reader.is_none() {
            reader = Reader::open(db)
                .inspect_err(|error| warn!(%error, "forge metadata: cannot open the index"))
                .ok();
        }
        let Some(open) = &reader else { continue };
        let repos = match open.list(&RepoFilter::default()) {
            Ok(repos) => repos,
            Err(error) => {
                warn!(%error, "forge metadata: cannot read the index");
                reader = None;
                continue;
            }
        };
        let plan = plan(&repos, &settings.targets);
        for path in plan.clears.iter().cloned() {
            debug!(path = %path.display(), "no forge credential: forge metadata removed");
            if store
                .send(Command::SaveForge {
                    path,
                    metadata: None,
                })
                .is_err()
            {
                return;
            }
        }
        for (index, target) in settings.targets.iter().enumerate() {
            let started = Instant::now();
            if paused_until[index].is_some_and(|until| started < until) {
                continue;
            }
            let due = started >= next_poll[index];
            let fetches = select(&plan, index, due);
            if fetches.is_empty() {
                if due {
                    next_poll[index] = started + jittered(settings.intervals[index], random_unit());
                }
                continue;
            }
            let now = timestamp(SystemTime::now()).unwrap_or(Timestamp::EPOCH);
            let reads = read_target(target, &fetches, &settings.resolver, &api, now);
            for (path, metadata) in reads.results {
                let command = Command::SaveForge {
                    path,
                    metadata: Some(Box::new(metadata)),
                };
                if store.send(command).is_err() {
                    return;
                }
            }
            if due {
                next_poll[index] = started + jittered(settings.intervals[index], random_unit());
            }
            paused_until[index] = reads.rate_limited.map(|wait| {
                let resume = Instant::now() + wait.duration(now);
                // The projects not read are read with the next full poll, after the wait.
                next_poll[index] = next_poll[index].max(resume);
                resume
            });
        }
    }
}
