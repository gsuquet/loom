//! Auto-upgrade of loomd (ADR-0011): a thread that checks for releases on schedule, and under
//! the `auto` policy downloads, pre-flights and installs one, starts the watchdog from the
//! saved previous version, and asks the daemon to stop.
//!
//! Decisions are in `loom_update::auto`; this module does the I/O.

use std::env;
use std::fs::{DirBuilder, OpenOptions, Permissions};
use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use loom_api::{Timestamp, UpdateChannel, UpdatePolicyMode, UpdateResult, UpdateState};
use loom_config::home::LOOM_HOME_ENV;
use loom_config::layout::{LOG_DIR_MODE, LOG_FILE_MODE, Layout};
use loom_update::auto::{self, CheckAction};
use loom_update::github::{self, Client};
use loom_update::{Discovery, Lookup, Paths, state};
use loomd::api::timestamp;
use loomd::cli::VERSION_INFO;
use semver::Version;
use tokio::sync::oneshot;
use tracing::field::Empty;
use tracing::{Span, debug, error, info, info_span, warn};

/// Longest sleep between two looks at the clock: the wall clock may jump (suspend, NTP).
const MAX_SLEEP: Duration = Duration::from_secs(60);

/// Effective update settings of this daemon.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Effective policy.
    pub policy: UpdatePolicyMode,
    /// Effective channel; `None`: nothing is checked.
    pub channel: Option<UpdateChannel>,
    /// Interval between checks, before jitter.
    pub interval: Duration,
    /// Watchdog deadline.
    pub deadline: Duration,
}

/// The running updater thread.
pub struct Updater {
    stop: mpsc::Sender<()>,
    /// Held while the binaries are swapped and the watchdog started.
    swapping: Arc<Mutex<()>>,
    _thread: JoinHandle<()>,
}

impl Updater {
    /// Stops checking, and waits for a swap in progress to finish, so that the binaries are
    /// never left half replaced. A download in progress is abandoned with the process.
    pub fn stop(self) {
        let _ = self.stop.send(());
        drop(self.swapping.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

/// Starts the updater. `upgraded` is sent when the binaries were replaced and the watchdog
/// started: the daemon must stop.
pub fn spawn(
    layout: Layout,
    settings: Settings,
    sources: Sources,
    started_at: Timestamp,
    upgraded: oneshot::Sender<()>,
) -> std::io::Result<Updater> {
    let (stop, stopped) = mpsc::channel();
    let swapping = Arc::new(Mutex::new(()));
    let guard = Arc::clone(&swapping);
    let thread = thread::Builder::new()
        .name("updater".to_owned())
        .spawn(move || {
            let worker = Worker {
                layout,
                settings,
                sources,
                swapping: guard,
            };
            if worker.run(started_at.unix_secs(), &stopped) {
                let _ = upgraded.send(());
            }
        })?;
    Ok(Updater {
        stop,
        swapping,
        _thread: thread,
    })
}

struct Worker {
    layout: Layout,
    settings: Settings,
    sources: Sources,
    swapping: Arc<Mutex<()>>,
}

/// Where the updater reads releases, from the environment of the daemon.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    /// `LOOM_RELEASES_URL`: replaces the GitHub API.
    pub releases_url: Option<String>,
    /// `LOOM_GITHUB_TOKEN` or `GITHUB_TOKEN` (no `gh` process from the daemon; the repository
    /// is public).
    pub token: Option<String>,
    /// `LOOM_UPDATE_PUBKEY` (honoured by debug builds only, in `loom_update::public_key`).
    pub public_key: Option<String>,
}

impl Sources {
    /// From the environment of the daemon.
    #[must_use]
    pub fn from_env() -> Self {
        let releases_url = env::var(github::RELEASES_URL_ENV).ok();
        let token = github::resolve_token(
            env::var_os(github::TOKEN_ENVS[0]).as_deref(),
            env::var_os(github::TOKEN_ENVS[1]).as_deref(),
            true,
            || None,
        );
        Self {
            releases_url,
            token,
            public_key: env::var(loom_update::PUBLIC_KEY_ENV).ok(),
        }
    }
}

/// How a check ended: what the updater does next, the `update.outcome` of its span, and the
/// error status.
struct Outcome {
    next: Next,
    name: &'static str,
    error: Option<String>,
}

impl Outcome {
    const fn done(name: &'static str) -> Self {
        Self {
            next: Next::Continue,
            name,
            error: None,
        }
    }

    fn failed(name: &'static str, error: impl Into<String>) -> Self {
        Self {
            next: Next::Continue,
            name,
            error: Some(error.into()),
        }
    }
}

/// What the updater does after a check.
enum Next {
    Continue,
    /// The binaries were replaced: stop the daemon.
    Upgraded,
}

impl Worker {
    /// Checks on schedule until stopped. Returns `true` when the daemon must stop for an
    /// upgrade.
    fn run(&self, started_at: u64, stopped: &mpsc::Receiver<()>) -> bool {
        let Settings {
            policy, channel, ..
        } = self.settings;
        let Some(channel) = channel.filter(|_| auto::checks(policy, channel)) else {
            info!(%policy, "update checks are off");
            return false;
        };
        let Ok(current) = Version::parse(&VERSION_INFO.version) else {
            warn!(version = %VERSION_INFO.version, "update checks are off: this build's version is not semver");
            return false;
        };
        // Also kept in memory: a state that cannot be written must not cause a check loop.
        let mut last_attempt: Option<u64> = None;
        loop {
            let recorded = self.read_state().last_check.map(Timestamp::unix_secs);
            let last = recorded.max(last_attempt);
            let next = auto::next_check(
                last,
                started_at,
                self.settings.interval,
                auto::JITTER,
                random_unit(),
            );
            debug!(next_check = next, "next update check scheduled");
            if !sleep_until(next, stopped) {
                return false;
            }
            last_attempt = Some(unix_now());
            match self.check(&current, channel) {
                Next::Continue => {}
                Next::Upgraded => return true,
            }
        }
    }

    fn read_state(&self) -> UpdateState {
        state::read(&self.layout.update_state).unwrap_or_else(|error| {
            warn!(%error, "cannot read the update state, using an empty one");
            UpdateState::default()
        })
    }

    /// Changes the state; a failure is logged, and the state it would have written returned.
    fn modify(&self, change: impl FnOnce(UpdateState) -> UpdateState) -> UpdateState {
        let mut computed = None;
        let result = state::modify(
            &self.layout,
            |error| warn!(%error, "replacing an invalid update state"),
            |state| {
                let next = change(state);
                computed = Some(next.clone());
                next
            },
        );
        match result {
            Ok(state) => state,
            Err(error) => {
                error!(%error, "cannot write the update state");
                computed.unwrap_or_default()
            }
        }
    }

    /// One check, traced as the root span `update.check` (attributes `update.channel`,
    /// `update.current`, `update.version`, `update.outcome`).
    fn check(&self, current: &Version, channel: UpdateChannel) -> Next {
        let span = info_span!(
            "update.check",
            update.channel = %channel,
            update.current = %current,
            update.version = Empty,
            update.outcome = Empty,
            otel.status_code = Empty,
            otel.status_description = Empty,
        );
        let outcome = span.in_scope(|| self.check_in_span(current, channel, &span));
        span.record("update.outcome", outcome.name);
        if let Some(error) = &outcome.error {
            loom_update::record_error(&span, error);
        }
        outcome.next
    }

    fn check_in_span(&self, current: &Version, channel: UpdateChannel, span: &Span) -> Outcome {
        let Some(now) = now() else {
            warn!("the system clock is outside the range of timestamps: no update check");
            return Outcome::failed(
                "failed",
                "the system clock is outside the range of timestamps",
            );
        };
        let state = self.modify(|state| auto::clear_stale(state, now));
        if let Some(pending) = auto::blocking_pending(&state, now.unix_secs()) {
            info!(from = %pending.from, to = %pending.to, "an upgrade is in progress, skipping this check");
            return Outcome::done("upgrade_in_progress");
        }
        info!(%channel, "checking for a new release");
        let client = self.client();
        let channel_of_decision = auto::decision_channel(channel);
        let discovery = match loom_update::discover(&client, &Lookup::Channel(channel_of_decision))
        {
            Ok(discovery) => discovery,
            Err(failure) => {
                warn!(error = %failure, "update check failed");
                let message = failure.to_string();
                self.modify(|state| auto::after_check_failed(state, message.clone(), now));
                return Outcome::failed("failed", message);
            }
        };
        let decision = loom_update::decide(
            current,
            channel_of_decision,
            &discovery.versions(),
            &auto::decision_policy(self.settings.policy),
            &auto::failed_versions(&state),
        );
        let mut action = CheckAction::Done;
        let state = self.modify(|state| {
            let (next, chosen) = auto::after_check(state, current, &decision, now);
            action = chosen;
            next
        });
        match action {
            CheckAction::Done => match &state.last_result {
                Some(UpdateResult::Available { version, .. }) => {
                    info!(%version, "a new release is available (policy notify)");
                    span.record("update.version", version.as_str());
                    Outcome::done("available")
                }
                Some(UpdateResult::SkippedFailed { version, .. }) => {
                    span.record("update.version", version.as_str());
                    Outcome::done("skipped_failed")
                }
                _ => Outcome::done("up_to_date"),
            },
            CheckAction::Install(version) => {
                span.record("update.version", version.to_string());
                self.upgrade(current, &version, &discovery, now)
            }
        }
    }

    /// Downloads, pre-flights and installs `version`, then starts the watchdog.
    fn upgrade(
        &self,
        current: &Version,
        version: &Version,
        discovery: &Discovery,
        checked_at: Timestamp,
    ) -> Outcome {
        info!(from = %current, to = %version, "upgrading");
        let at = || now().unwrap_or(checked_at);
        let fail = |message: String| {
            warn!(to = %version, "upgrade abandoned: {message}");
            self.modify(|state| auto::after_upgrade_error(state, message.clone(), at()));
            Outcome::failed("failed", message)
        };
        let paths = match self.paths() {
            Ok(paths) => paths,
            Err(message) => return fail(message),
        };
        if let Err(error) = loom_update::install::check_writable(&paths.install_dir) {
            return fail(format!(
                "cannot upgrade {}: the directory is not writable ({})",
                error.path.display(),
                error.source
            ));
        }
        let Some(release) = discovery.release(version) else {
            return fail(format!("no release for version {version}"));
        };
        let staged = match loom_update::download(
            &self.client(),
            release,
            version,
            &loom_update::release::release_target(&VERSION_INFO.target),
            loom_update::public_key(self.sources.public_key.as_deref()),
            &paths.staging_dir,
            &mut |line: &str| debug!("{line}"),
        ) {
            Ok(staged) => staged,
            Err(error) => return fail(error.to_string()),
        };

        let preflight = info_span!(
            "update.preflight",
            update.version = %version,
            update.outcome = Empty,
            otel.status_code = Empty,
            otel.status_description = Empty,
        );
        let checked = preflight.in_scope(|| {
            loom_update::preflight::check(&staged.dir, version, auto::PREFLIGHT_TIMEOUT)
        });
        if let Err(message) = checked {
            preflight.record("update.outcome", "failed");
            loom_update::record_error(&preflight, &message);
            warn!(%version, "pre-flight check failed, version recorded as failed: {message}");
            self.modify(|state| {
                auto::after_preflight_failed(state, version, message.clone(), at())
            });
            return Outcome::failed("preflight_failed", message);
        }
        preflight.record("update.outcome", "passed");
        drop(preflight);
        info!(%version, "pre-flight check passed");

        let swap = info_span!(
            "update.swap",
            update.from = %current,
            update.to = %version,
            update.outcome = Empty,
            otel.status_code = Empty,
            otel.status_description = Empty,
        );
        let outcome = swap.in_scope(|| self.swap(&staged, &paths, current, version, &fail));
        swap.record("update.outcome", outcome.name);
        if let Some(error) = &outcome.error {
            loom_update::record_error(&swap, error);
        }
        outcome
    }

    /// Replaces the binaries and starts the watchdog, with the swap mutex held.
    fn swap(
        &self,
        staged: &loom_update::Staged,
        paths: &Paths,
        current: &Version,
        version: &Version,
        fail: &dyn Fn(String) -> Outcome,
    ) -> Outcome {
        let _swapping = self.swapping.lock().unwrap_or_else(PoisonError::into_inner);
        let swapped = match loom_update::swap(staged, paths, current) {
            Ok(swapped) => swapped,
            Err(error) => return fail(error.to_string()),
        };
        for warning in &swapped.warnings {
            warn!("{warning}");
        }
        if let Err(message) = self.start_watchdog(&swapped.saved_to, current, version, paths) {
            // Without a watchdog nobody would check the new version: put the old one back.
            let restored =
                loom_update::install::replace_binaries(&paths.install_dir, &swapped.saved_to)
                    .map_or_else(
                        |error| format!("; restoring {current} failed too: {error}"),
                        |()| format!("; {current} restored"),
                    );
            return fail(format!("{message}{restored}"));
        }
        info!(from = %current, to = %version, "binaries replaced, stopping so that the new version starts");
        Outcome {
            next: Next::Upgraded,
            name: "swapped",
            error: None,
        }
    }

    /// GitHub client from the sources; the TLS configuration is the one `loom_update` shares.
    fn client(&self) -> Client {
        Client::new(
            github::base_url(self.sources.releases_url.as_deref()),
            self.sources.token.clone(),
            &format!("loomd/{}", VERSION_INFO.version),
        )
    }

    /// Paths of the upgrade: the install directory is the directory of the running `loomd`.
    fn paths(&self) -> Result<Paths, String> {
        Ok(Paths {
            install_dir: running_dir()?,
            versions_dir: self.layout.versions_dir.clone(),
            staging_dir: self.layout.staging_dir.clone(),
            lock: self.layout.lock.clone(),
        })
    }

    /// Starts `<saved>/loom __upgrade-watchdog` detached, stderr appended to the daemon log.
    fn start_watchdog(
        &self,
        saved: &Path,
        current: &Version,
        version: &Version,
        paths: &Paths,
    ) -> Result<(), String> {
        let log = open_log(&self.layout)?;
        let loom = saved.join("loom");
        Command::new(&loom)
            .arg("__upgrade-watchdog")
            .arg("--from")
            .arg(current.to_string())
            .arg("--expect")
            .arg(version.to_string())
            .arg("--deadline")
            .arg(self.settings.deadline.as_secs().to_string())
            .arg("--install-dir")
            .arg(&paths.install_dir)
            .env(LOOM_HOME_ENV, &self.layout.home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            // Own process group: not stopped with loomd (launchd kills the job's group).
            .process_group(0)
            .spawn()
            .map(drop)
            .map_err(|error| format!("cannot start the watchdog {}: {error}", loom.display()))
    }
}

/// Sleeps until `target` (seconds since the epoch). `false` when stopped first.
fn sleep_until(target: u64, stopped: &mpsc::Receiver<()>) -> bool {
    loop {
        let now = unix_now();
        if now >= target {
            return true;
        }
        let wait = Duration::from_secs(target - now).min(MAX_SLEEP);
        match stopped.recv_timeout(wait) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return false,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn now() -> Option<Timestamp> {
    timestamp(SystemTime::now())
}

fn unix_now() -> u64 {
    now().map_or(0, Timestamp::unix_secs)
}

/// A number in `[0, 1]` from the standard library's randomly seeded hasher.
fn random_unit() -> f64 {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos()),
    );
    let high = u32::try_from(hasher.finish() >> 32).unwrap_or(0);
    f64::from(high) / f64::from(u32::MAX)
}

/// Directory of the running `loomd`, symbolic links resolved.
fn running_dir() -> Result<PathBuf, String> {
    let exe = env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(|error| format!("cannot find the running loomd binary: {error}"))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{} has no parent directory", exe.display()))
}

/// `log/loomd.log` opened for appending (directory 0700, file 0600).
fn open_log(layout: &Layout) -> Result<std::fs::File, String> {
    let log: &PathBuf = &layout.daemon_log;
    DirBuilder::new()
        .recursive(true)
        .mode(LOG_DIR_MODE)
        .create(&layout.log_dir)
        .and_then(|()| {
            OpenOptions::new()
                .append(true)
                .create(true)
                .mode(LOG_FILE_MODE)
                .open(log)
        })
        .and_then(|file| {
            file.set_permissions(Permissions::from_mode(LOG_FILE_MODE))
                .map(|()| file)
        })
        .map_err(|error| format!("cannot open {}: {error}", log.display()))
}
