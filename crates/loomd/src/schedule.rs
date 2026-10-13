//! When `loomd` updates each repository (ADR-0012).
//!
//! Every `[repos] fetch_interval`, jittered by ±10 %, with a backoff after failed fetches, per
//! repository and per host. Pure: the time and the randomness are given.
//!
//! A failed fetch delays the next update of its repository, doubling from the interval up to
//! [`MAX_BACKOFF_FACTOR`] times it. It delays every repository of its host the same way, unless
//! a fetch of that host started before it succeeded after it started: the host answers, so the
//! failure is the repository's own. A successful fetch of a host clears the host's backoff.
//! Other hosts are never delayed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::repo_update::FetchStatus;

/// Relative jitter of every delay: ±10 %.
pub const JITTER: f64 = 0.1;
/// The longest backoff, as a multiple of the interval.
pub const MAX_BACKOFF_FACTOR: u32 = 8;

/// A repository of the index, as the schedule sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate<'a> {
    /// Its path, the key.
    pub path: &'a Path,
    /// Host of its primary remote (`host[:port]`, or the URL when it has no host); empty when
    /// it has no remote.
    pub host: &'a str,
    /// Time of its last update (Unix seconds), from the index.
    pub last_update: Option<u64>,
}

/// One repository.
#[derive(Debug, Clone)]
struct Slot {
    next: Instant,
    failures: u32,
    host: String,
}

/// One host.
#[derive(Debug, Clone, Copy, Default)]
struct Host {
    failures: u32,
    until: Option<Instant>,
    last_success: Option<Instant>,
}

/// The update schedule of every repository.
#[derive(Debug, Clone)]
pub struct Schedule {
    interval: Duration,
    repos: HashMap<PathBuf, Slot>,
    hosts: HashMap<String, Host>,
    running: HashSet<PathBuf>,
}

impl Schedule {
    /// An empty schedule with `interval` between two updates of a repository.
    #[must_use]
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            repos: HashMap::new(),
            hosts: HashMap::new(),
            running: HashSet::new(),
        }
    }

    /// The repositories to update now, earliest due first, at most `slots`. `candidates` is the
    /// index: a new repository is due one jittered interval after its last update, or at once
    /// when it has none; a repository no longer listed is forgotten. `now` is the monotonic
    /// time, `unix_now` the wall clock, `random` gives numbers in `[0, 1]`.
    pub fn due(
        &mut self,
        candidates: &[Candidate<'_>],
        now: Instant,
        unix_now: u64,
        slots: usize,
        random: &mut dyn FnMut() -> f64,
    ) -> Vec<PathBuf> {
        let listed: HashSet<&Path> = candidates.iter().map(|candidate| candidate.path).collect();
        self.repos.retain(|path, _| listed.contains(path.as_path()));
        let mut due: Vec<(Instant, PathBuf)> = Vec::new();
        for candidate in candidates {
            let slot = self
                .repos
                .entry(candidate.path.to_path_buf())
                .or_insert_with(|| Slot {
                    next: first_due(
                        candidate.last_update,
                        now,
                        unix_now,
                        self.interval,
                        random(),
                    ),
                    failures: 0,
                    host: String::new(),
                });
            candidate.host.clone_into(&mut slot.host);
            let next = slot.next;
            let host_ready = self
                .hosts
                .get(candidate.host)
                .and_then(|host| host.until)
                .is_none_or(|until| until <= now);
            if next <= now && host_ready && !self.running.contains(candidate.path) {
                due.push((next, candidate.path.to_path_buf()));
            }
        }
        due.sort();
        due.into_iter().take(slots).map(|(_, path)| path).collect()
    }

    /// Marks `path` as being updated, by the schedule or on request.
    pub fn start(&mut self, path: &Path) {
        self.running.insert(path.to_path_buf());
    }

    /// Whether `path` is being updated.
    #[must_use]
    pub fn is_running(&self, path: &Path) -> bool {
        self.running.contains(path)
    }

    /// Records the end of the update of `path`, whose primary remote is on `host`, started at
    /// `started`, ended at `now`, and schedules its next one.
    pub fn finish(
        &mut self,
        path: &Path,
        host: &str,
        started: Instant,
        now: Instant,
        fetch: FetchStatus,
        random: &mut dyn FnMut() -> f64,
    ) {
        self.running.remove(path);
        let interval = self.interval;
        let slot = self
            .repos
            .entry(path.to_path_buf())
            .or_insert_with(|| Slot {
                next: now,
                failures: 0,
                host: host.to_owned(),
            });
        match fetch {
            FetchStatus::NotRun | FetchStatus::Succeeded => slot.failures = 0,
            // Says nothing about the remote: the failures are kept, the next try is one
            // interval away.
            FetchStatus::Busy => {}
            FetchStatus::Failed => slot.failures = slot.failures.saturating_add(1),
        }
        let delay = match fetch {
            FetchStatus::Busy => interval,
            FetchStatus::NotRun | FetchStatus::Succeeded | FetchStatus::Failed => {
                backoff(interval, slot.failures)
            }
        };
        slot.next = now + jittered(delay, random());
        match fetch {
            FetchStatus::NotRun | FetchStatus::Busy => {}
            FetchStatus::Succeeded => {
                self.hosts.insert(
                    host.to_owned(),
                    Host {
                        failures: 0,
                        until: None,
                        last_success: Some(now),
                    },
                );
            }
            FetchStatus::Failed => {
                let entry = self.hosts.entry(host.to_owned()).or_default();
                // Another repository of the host fetched since this one started: the host is up.
                if entry.last_success.is_none_or(|success| success < started) {
                    entry.failures = entry.failures.saturating_add(1);
                    entry.until = Some(now + backoff(interval, entry.failures));
                }
            }
        }
    }

    /// When the next repository becomes due, its host's backoff included; `None` when every
    /// repository is running or none is known.
    #[must_use]
    pub fn next_due(&self) -> Option<Instant> {
        self.repos
            .iter()
            .filter(|(path, _)| !self.running.contains(*path))
            .map(|(_, slot)| {
                let until = self.hosts.get(&slot.host).and_then(|host| host.until);
                until.map_or(slot.next, |until| until.max(slot.next))
            })
            .min()
    }

    /// The backoff of `host`: until when its repositories wait, and after how many failures.
    #[must_use]
    pub fn host_backoff(&self, host: &str) -> Option<(Instant, u32)> {
        self.hosts
            .get(host)
            .and_then(|host| host.until.map(|until| (until, host.failures)))
    }
}

/// When a repository first seen is due: one jittered interval after its last update (at once
/// when that is past), at once when it was never updated.
fn first_due(
    last_update: Option<u64>,
    now: Instant,
    unix_now: u64,
    interval: Duration,
    random: f64,
) -> Instant {
    last_update.map_or(now, |last| {
        let since = Duration::from_secs(unix_now.saturating_sub(last));
        now + jittered(interval, random).saturating_sub(since)
    })
}

/// `interval` times 2 to the power of `failures`, at most [`MAX_BACKOFF_FACTOR`] times;
/// `interval` itself without failure.
#[must_use]
pub fn backoff(interval: Duration, failures: u32) -> Duration {
    let factor = 1_u32
        .checked_shl(failures)
        .unwrap_or(u32::MAX)
        .min(MAX_BACKOFF_FACTOR);
    interval.saturating_mul(factor)
}

/// `delay` scaled by a factor in `[1 - JITTER, 1 + JITTER]`, `random` in `[0, 1]` choosing it.
#[must_use]
pub fn jittered(delay: Duration, random: f64) -> Duration {
    let random = random.clamp(0.0, 1.0);
    delay.mul_f64(JITTER.mul_add(2.0f64.mul_add(random, -1.0), 1.0))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use super::{Candidate, Schedule, backoff, jittered};
    use crate::repo_update::FetchStatus;

    const HOUR: Duration = Duration::from_hours(1);
    const NONE: [PathBuf; 0] = [];

    fn candidate<'a>(path: &'a str, host: &'a str) -> Candidate<'a> {
        Candidate {
            path: Path::new(path),
            host,
            last_update: None,
        }
    }

    /// No jitter: the middle of the range.
    fn middle() -> f64 {
        0.5
    }

    #[test]
    fn backoff_doubles_from_the_interval_up_to_eight_times() {
        assert_eq!(backoff(HOUR, 0), HOUR);
        assert_eq!(backoff(HOUR, 1), HOUR * 2);
        assert_eq!(backoff(HOUR, 2), HOUR * 4);
        assert_eq!(backoff(HOUR, 3), HOUR * 8);
        assert_eq!(backoff(HOUR, 4), HOUR * 8);
        assert_eq!(backoff(HOUR, 40), HOUR * 8);
    }

    #[test]
    fn jitter_stays_within_ten_percent() {
        assert_eq!(jittered(HOUR, 0.5), HOUR);
        assert_eq!(jittered(HOUR, 0.0), Duration::from_mins(54));
        assert_eq!(jittered(HOUR, 1.0), Duration::from_mins(66));
        assert_eq!(jittered(HOUR, 7.0), Duration::from_mins(66));
        assert_eq!(jittered(HOUR, -1.0), Duration::from_mins(54));
    }

    #[test]
    fn new_repositories_are_due_at_once_or_one_interval_after_their_last_update() {
        let now = Instant::now();
        let mut schedule = Schedule::new(HOUR);
        let unix_now = 100_000;
        let candidates = [
            candidate("/r/never", "a"),
            Candidate {
                last_update: Some(unix_now - 600),
                ..candidate("/r/recent", "a")
            },
            Candidate {
                last_update: Some(unix_now - 7_200),
                ..candidate("/r/old", "a")
            },
        ];
        let due = schedule.due(&candidates, now, unix_now, 10, &mut middle);
        assert_eq!(due, [Path::new("/r/never"), Path::new("/r/old")]);
        // One second before `recent` is due.
        let later = now + Duration::from_secs(2_999);
        for path in &due {
            schedule.start(path);
        }
        assert_eq!(
            schedule.due(&candidates, later, unix_now + 2_999, 10, &mut middle),
            NONE
        );
        let due = schedule.due(&candidates, now + HOUR, unix_now + 3_600, 10, &mut middle);
        assert_eq!(due, [Path::new("/r/recent")]);
    }

    #[test]
    fn at_most_the_free_slots_and_never_twice_at_once() {
        let now = Instant::now();
        let mut schedule = Schedule::new(HOUR);
        let candidates = [
            candidate("/r/a", "h"),
            candidate("/r/b", "h"),
            candidate("/r/c", "h"),
        ];
        let due = schedule.due(&candidates, now, 0, 2, &mut middle);
        assert_eq!(due.len(), 2);
        for path in &due {
            schedule.start(path);
            assert!(schedule.is_running(path));
        }
        let rest = schedule.due(&candidates, now, 0, 2, &mut middle);
        assert_eq!(rest, [Path::new("/r/c")]);
        assert_eq!(schedule.due(&candidates, now, 0, 0, &mut middle), NONE);
    }

    #[test]
    fn next_update_is_one_jittered_interval_after_the_end() {
        let now = Instant::now();
        let mut schedule = Schedule::new(HOUR);
        let candidates = [candidate("/r/a", "h")];
        let path = Path::new("/r/a");
        assert_eq!(schedule.due(&candidates, now, 0, 1, &mut middle), [path]);
        schedule.start(path);
        assert_eq!(schedule.next_due(), None);
        schedule.finish(path, "h", now, now, FetchStatus::Succeeded, &mut || 0.0);
        assert!(!schedule.is_running(path));
        assert_eq!(schedule.next_due(), Some(now + Duration::from_mins(54)));
        let before = now + Duration::from_secs(3_239);
        assert_eq!(schedule.due(&candidates, before, 0, 1, &mut middle), NONE);
        let after = now + Duration::from_mins(54);
        assert_eq!(schedule.due(&candidates, after, 0, 1, &mut middle), [path]);
    }

    #[test]
    fn an_unreachable_host_backs_off_without_delaying_other_hosts() {
        let now = Instant::now();
        let mut schedule = Schedule::new(HOUR);
        let candidates = [
            candidate("/r/vpn-1", "gitlab.corp"),
            candidate("/r/vpn-2", "gitlab.corp"),
            candidate("/r/pub", "github.com"),
        ];
        let first = schedule.due(&candidates, now, 0, 1, &mut middle);
        assert_eq!(first.len(), 1);
        let failed = Path::new("/r/vpn-1");
        schedule.start(failed);
        schedule.finish(
            failed,
            "gitlab.corp",
            now,
            now,
            FetchStatus::Failed,
            &mut middle,
        );
        assert_eq!(
            schedule.host_backoff("gitlab.corp"),
            Some((now + HOUR * 2, 1))
        );
        // The other repository of the host waits; the other host does not.
        let due = schedule.due(&candidates, now, 0, 10, &mut middle);
        assert_eq!(due, [Path::new("/r/pub")]);
        assert!(!due.contains(&Path::new("/r/vpn-2").to_path_buf()));

        // Still down after the backoff: doubled, up to 8 times the interval.
        let vpn_2 = &candidates[1..2];
        let mut at = now + HOUR * 2;
        for (failures, factor) in [(2, 4), (3, 8), (4, 8)] {
            let early = at.checked_sub(Duration::from_secs(1)).unwrap();
            assert_eq!(schedule.due(vpn_2, early, 0, 10, &mut middle), NONE);
            let due = schedule.due(vpn_2, at, 0, 10, &mut middle);
            assert_eq!(due, [Path::new("/r/vpn-2")], "{failures}");
            schedule.start(&due[0]);
            schedule.finish(
                &due[0],
                "gitlab.corp",
                at,
                at,
                FetchStatus::Failed,
                &mut middle,
            );
            assert_eq!(
                schedule.host_backoff("gitlab.corp"),
                Some((at + HOUR * factor, failures))
            );
            at += HOUR * factor;
        }

        // Back: a success clears the backoff.
        let due = schedule.due(vpn_2, at, 0, 10, &mut middle);
        assert_ne!(due, NONE);
        let path = due[0].clone();
        schedule.start(&path);
        schedule.finish(
            &path,
            "gitlab.corp",
            at,
            at,
            FetchStatus::Succeeded,
            &mut middle,
        );
        assert_eq!(schedule.host_backoff("gitlab.corp"), None);
    }

    #[test]
    fn a_failure_while_the_host_answers_delays_only_its_repository() {
        let now = Instant::now();
        let mut schedule = Schedule::new(HOUR);
        let candidates = [
            candidate("/r/gone", "github.com"),
            candidate("/r/ok", "github.com"),
        ];
        let due = schedule.due(&candidates, now, 0, 10, &mut middle);
        assert_eq!(due.len(), 2);
        for path in &due {
            schedule.start(path);
        }
        let ended = now + Duration::from_secs(5);
        // `ok` succeeds after `gone` started; `gone` fails afterwards.
        schedule.finish(
            Path::new("/r/ok"),
            "github.com",
            now,
            now + Duration::from_secs(2),
            FetchStatus::Succeeded,
            &mut middle,
        );
        schedule.finish(
            Path::new("/r/gone"),
            "github.com",
            now,
            ended,
            FetchStatus::Failed,
            &mut middle,
        );
        assert_eq!(schedule.host_backoff("github.com"), None);
        // The failed repository backs off on its own: 2 intervals.
        let at = ended + HOUR;
        let due = schedule.due(&candidates, at + Duration::from_secs(5), 0, 10, &mut middle);
        assert_eq!(due, [Path::new("/r/ok")]);
        let due = schedule.due(&candidates, ended + HOUR * 2, 0, 10, &mut middle);
        assert!(due.contains(&Path::new("/r/gone").to_path_buf()), "{due:?}");
    }

    #[test]
    fn forgotten_repositories_leave_the_schedule() {
        let now = Instant::now();
        let mut schedule = Schedule::new(HOUR);
        let due = schedule.due(&[candidate("/r/a", "h")], now, 0, 1, &mut middle);
        assert_eq!(due.len(), 1);
        assert!(schedule.next_due().is_some());
        assert_eq!(schedule.due(&[], now, 0, 1, &mut middle), NONE);
        assert_eq!(schedule.next_due(), None);
    }

    #[test]
    fn skipped_updates_never_touch_the_host() {
        let now = Instant::now();
        let mut schedule = Schedule::new(HOUR);
        let path = Path::new("/r/a");
        schedule.start(path);
        schedule.finish(path, "", now, now, FetchStatus::NotRun, &mut middle);
        assert_eq!(schedule.host_backoff(""), None);
        assert_eq!(schedule.next_due(), Some(now + HOUR));
    }

    #[test]
    fn a_busy_repository_is_retried_at_the_next_interval_without_backoff() {
        let now = Instant::now();
        let mut schedule = Schedule::new(HOUR);
        let path = Path::new("/r/a");
        schedule.start(path);
        schedule.finish(path, "h", now, now, FetchStatus::Busy, &mut middle);
        assert_eq!(schedule.host_backoff("h"), None);
        assert_eq!(schedule.next_due(), Some(now + HOUR));

        // After a failure: the host's backoff is neither extended nor cleared, the repository
        // waits one interval, and its failures are kept for the next one.
        schedule.start(path);
        schedule.finish(path, "h", now, now, FetchStatus::Failed, &mut middle);
        assert_eq!(schedule.host_backoff("h"), Some((now + HOUR * 2, 1)));
        let at = now + HOUR * 2;
        schedule.start(path);
        schedule.finish(path, "h", at, at, FetchStatus::Busy, &mut middle);
        assert_eq!(schedule.host_backoff("h"), Some((now + HOUR * 2, 1)));
        assert_eq!(schedule.next_due(), Some(at + HOUR));
        let later = at + HOUR;
        schedule.start(path);
        schedule.finish(
            path,
            "other",
            later,
            later,
            FetchStatus::Failed,
            &mut middle,
        );
        assert_eq!(schedule.next_due(), Some(later + HOUR * 4));
    }
}
