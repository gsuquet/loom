//! Decisions of the auto-upgrade in loomd (ADR-0011), without I/O: effective policy and
//! channel, check schedule, state transitions, the `update` object of the status response.
//!
//! Time is passed in as whole seconds since the Unix epoch, randomness as a number in
//! `[0, 1)`.

use std::cmp::Ordering;
use std::time::Duration;

use loom_api::{
    Channel as BuildChannel, PendingUpgrade, Timestamp, UpdateChannel, UpdatePolicyMode,
    UpdateResult, UpdateState, UpdateStatus,
};
use semver::Version;

use crate::decision::{Channel, SkipReason, UpdateDecision, UpdateMode, UpdatePolicy};

/// Fraction of the interval added or removed at random from each delay.
pub const JITTER: f64 = 0.1;
/// Longest delay between the start of loomd and its first check, before jitter.
pub const INITIAL_DELAY: Duration = Duration::from_mins(5);
/// Default time the watchdog waits for the new loomd to answer.
pub const HEALTH_DEADLINE: Duration = Duration::from_secs(60);
/// Longest time the watchdog waits for a service manager to restart loomd before starting it.
pub const MAX_RESTART_GRACE: Duration = Duration::from_secs(15);
/// Time the pre-flight waits for the staged loomd to answer.
pub const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(15);
/// Age after which a `pending` upgrade is abandoned.
pub const PENDING_STALE_AFTER: Duration = Duration::from_secs(3_600);

/// Effective policy: the configured one, otherwise `auto` for a release build (`stable` or
/// `dev`) and `notify` for a local build.
#[must_use]
pub const fn effective_policy(
    configured: Option<UpdatePolicyMode>,
    build: BuildChannel,
) -> UpdatePolicyMode {
    match (configured, build) {
        (Some(policy), _) => policy,
        (None, BuildChannel::Stable | BuildChannel::Dev) => UpdatePolicyMode::Auto,
        (None, BuildChannel::Local) => UpdatePolicyMode::Notify,
    }
}

/// Effective channel: the configured one, otherwise the build channel. A local build has none:
/// nothing is checked.
#[must_use]
pub const fn effective_channel(
    configured: Option<UpdateChannel>,
    build: BuildChannel,
) -> Option<UpdateChannel> {
    match (configured, build) {
        (Some(channel), _) => Some(channel),
        (None, BuildChannel::Stable) => Some(UpdateChannel::Stable),
        (None, BuildChannel::Dev) => Some(UpdateChannel::Dev),
        (None, BuildChannel::Local) => None,
    }
}

/// Whether loomd checks for updates at all: a policy other than `off` and a channel.
#[must_use]
pub const fn checks(policy: UpdatePolicyMode, channel: Option<UpdateChannel>) -> bool {
    match (policy, channel) {
        (UpdatePolicyMode::Off, _) | (_, None) => false,
        (UpdatePolicyMode::Auto | UpdatePolicyMode::Notify, Some(_)) => true,
    }
}

/// Channel of the decision.
#[must_use]
pub const fn decision_channel(channel: UpdateChannel) -> Channel {
    match channel {
        UpdateChannel::Stable => Channel::Stable,
        UpdateChannel::Dev => Channel::Dev,
    }
}

/// Policy of the decision: the mode, never a pinned version.
#[must_use]
pub const fn decision_policy(policy: UpdatePolicyMode) -> UpdatePolicy {
    UpdatePolicy {
        mode: match policy {
            UpdatePolicyMode::Auto => UpdateMode::Auto,
            UpdatePolicyMode::Notify => UpdateMode::Notify,
            UpdatePolicyMode::Off => UpdateMode::Off,
        },
        pinned: None,
    }
}

/// `duration` changed by up to `±jitter` of itself, rounded to whole seconds: `random` 0 gives the shortest, 0.5 the
/// duration itself, values towards 1 the longest. `random` is clamped to `[0, 1]`.
#[must_use]
pub fn jittered(duration: Duration, jitter: f64, random: f64) -> Duration {
    let factor = jitter.mul_add(2.0f64.mul_add(random.clamp(0.0, 1.0), -1.0), 1.0);
    // Whole seconds: `0.9 * 3600` is not exactly 3240 in binary floating point.
    Duration::try_from_secs_f64((duration.as_secs_f64() * factor.max(0.0)).round())
        .unwrap_or(duration)
}

/// Instant of the next check, in seconds since the epoch.
///
/// `last_check + interval`, but never before `started_at + min(INITIAL_DELAY, interval)`, so
/// that a (re)start never checks at once; both delays jittered with the same `random`.
#[must_use]
pub fn next_check(
    last_check: Option<u64>,
    started_at: u64,
    interval: Duration,
    jitter: f64,
    random: f64,
) -> u64 {
    let initial = jittered(INITIAL_DELAY.min(interval), jitter, random).as_secs();
    let earliest = started_at.saturating_add(initial);
    last_check.map_or(earliest, |last| {
        last.saturating_add(jittered(interval, jitter, random).as_secs())
            .max(earliest)
    })
}

/// Grace period the watchdog leaves a service manager to restart loomd: a quarter of the
/// deadline, at most [`MAX_RESTART_GRACE`].
#[must_use]
pub fn restart_grace(deadline: Duration) -> Duration {
    (deadline / 4).min(MAX_RESTART_GRACE)
}

/// Whether `pending` started more than [`PENDING_STALE_AFTER`] before `now`.
#[must_use]
pub const fn is_stale(pending: &PendingUpgrade, now: u64) -> bool {
    now.saturating_sub(pending.started_at.unix_secs()) > PENDING_STALE_AFTER.as_secs()
}

/// The pending upgrade that blocks another one at `now`: a `pending` that is not stale.
#[must_use]
pub fn blocking_pending(state: &UpdateState, now: u64) -> Option<&PendingUpgrade> {
    state
        .pending
        .as_ref()
        .filter(|pending| !is_stale(pending, now))
}

/// `failed_versions` as versions; entries that are not semver are skipped.
#[must_use]
pub fn failed_versions(state: &UpdateState) -> Vec<Version> {
    state
        .failed_versions
        .iter()
        .filter_map(|text| Version::parse(text).ok())
        .collect()
}

/// `state` with `version` in `failed_versions` (once, by semver precedence).
#[must_use]
pub fn with_failed(mut state: UpdateState, version: &Version) -> UpdateState {
    let known = failed_versions(&state)
        .iter()
        .any(|failed| failed.cmp_precedence(version) == Ordering::Equal);
    if !known {
        state.failed_versions.push(version.to_string());
    }
    state
}

/// What to do after a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckAction {
    /// Nothing more: the state records the outcome.
    Done,
    /// Install this version (policy `auto`): `pending` is set.
    Install(Version),
}

/// State after a check that returned `decision` at `now`, and the action to take. A stale
/// `pending` must have been cleared first ([`clear_stale`]).
#[must_use]
pub fn after_check(
    mut state: UpdateState,
    current: &Version,
    decision: &UpdateDecision,
    at: Timestamp,
) -> (UpdateState, CheckAction) {
    state.last_check = Some(at);
    let (available, result, action) = match decision {
        UpdateDecision::Available { version } => (
            Some(version.to_string()),
            UpdateResult::Available {
                at,
                version: version.to_string(),
            },
            CheckAction::Done,
        ),
        UpdateDecision::Skipped {
            reason: SkipReason::Failed { version },
        } => (
            None,
            UpdateResult::SkippedFailed {
                at,
                version: version.to_string(),
            },
            CheckAction::Done,
        ),
        // `Skipped` other than `Failed` is unreachable without a pin and with a policy other
        // than `off`; recorded as up to date.
        UpdateDecision::UpToDate
        | UpdateDecision::Skipped {
            reason:
                SkipReason::UpdatesOff
                | SkipReason::PinnedNotReleased { .. }
                | SkipReason::PinnedBelowCurrent { .. },
        } => (None, UpdateResult::UpToDate { at }, CheckAction::Done),
        UpdateDecision::Apply { version } => {
            state.pending = Some(PendingUpgrade {
                from: current.to_string(),
                to: version.to_string(),
                started_at: at,
            });
            state.available = None;
            return (state, CheckAction::Install(version.clone()));
        }
    };
    state.available = available;
    state.last_result = Some(result);
    (state, action)
}

/// State after a check that failed at `at` (network, API): recorded, retried next interval.
#[must_use]
pub fn after_check_failed(mut state: UpdateState, message: String, at: Timestamp) -> UpdateState {
    state.last_check = Some(at);
    state.last_result = Some(UpdateResult::Failed { at, message });
    state
}

/// State after an upgrade failed before the swap (download, verification, not writable,
/// install error): `pending` cleared, the version not marked failed (it may work next time).
#[must_use]
pub fn after_upgrade_error(mut state: UpdateState, message: String, at: Timestamp) -> UpdateState {
    state.pending = None;
    state.last_result = Some(UpdateResult::Failed { at, message });
    state
}

/// State after `version` failed the pre-flight: `pending` cleared, `version` failed.
#[must_use]
pub fn after_preflight_failed(
    mut state: UpdateState,
    version: &Version,
    message: String,
    at: Timestamp,
) -> UpdateState {
    state.pending = None;
    state.last_result = Some(UpdateResult::PreflightFailed {
        at,
        version: version.to_string(),
        message,
    });
    with_failed(state, version)
}

/// State after the watchdog saw `to` answer: the upgrade is complete.
#[must_use]
pub fn after_upgraded(
    mut state: UpdateState,
    from: &Version,
    to: &Version,
    at: Timestamp,
) -> UpdateState {
    state.pending = None;
    state.last_result = Some(UpdateResult::Upgraded {
        at,
        from: from.to_string(),
        to: to.to_string(),
    });
    state
}

/// State after the watchdog restored `from` because `to` did not answer.
#[must_use]
pub fn after_rolled_back(
    mut state: UpdateState,
    from: &Version,
    to: &Version,
    message: String,
    at: Timestamp,
) -> UpdateState {
    state.pending = None;
    state.last_result = Some(UpdateResult::RolledBack {
        at,
        from: from.to_string(),
        to: to.to_string(),
        message,
    });
    with_failed(state, to)
}

/// `state` without a stale `pending`, recorded as interrupted. Unchanged otherwise.
#[must_use]
pub fn clear_stale(mut state: UpdateState, at: Timestamp) -> UpdateState {
    if let Some(pending) = state
        .pending
        .take_if(|pending| is_stale(pending, at.unix_secs()))
    {
        state.last_result = Some(UpdateResult::Interrupted {
            at,
            from: pending.from,
            to: pending.to,
        });
    }
    state
}

/// The `update` object of the status response. `available` is dropped when it is not newer
/// than `running` (installed since, by hand).
#[must_use]
pub fn status(
    state: UpdateState,
    policy: UpdatePolicyMode,
    channel: Option<UpdateChannel>,
    interval: Duration,
    running: Option<&Version>,
) -> UpdateStatus {
    let newer = |available: &String| match (Version::parse(available), running) {
        (Ok(available), Some(running)) => available.cmp_precedence(running) == Ordering::Greater,
        _ => true,
    };
    UpdateStatus {
        policy,
        channel,
        check_interval_secs: interval.as_secs(),
        last_check: state.last_check,
        available: state.available.filter(newer),
        pending: state.pending,
        failed_versions: state.failed_versions,
        last_result: state.last_result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn at(secs: u64) -> Timestamp {
        Timestamp::from_unix_secs(secs).unwrap()
    }

    const HOUR: u64 = 3_600;

    #[test]
    fn default_policy_depends_on_the_build_channel() {
        use BuildChannel::{Dev, Local, Stable};
        use UpdatePolicyMode::{Auto, Notify, Off};
        assert_eq!(effective_policy(None, Stable), Auto);
        assert_eq!(effective_policy(None, Dev), Auto);
        assert_eq!(effective_policy(None, Local), Notify);
        for build in [Stable, Dev, Local] {
            for configured in [Auto, Notify, Off] {
                assert_eq!(effective_policy(Some(configured), build), configured);
            }
        }
    }

    #[test]
    fn default_channel_is_the_build_channel_and_local_has_none() {
        assert_eq!(
            effective_channel(None, BuildChannel::Stable),
            Some(UpdateChannel::Stable)
        );
        assert_eq!(
            effective_channel(None, BuildChannel::Dev),
            Some(UpdateChannel::Dev)
        );
        assert_eq!(effective_channel(None, BuildChannel::Local), None);
        assert_eq!(
            effective_channel(Some(UpdateChannel::Dev), BuildChannel::Local),
            Some(UpdateChannel::Dev)
        );
    }

    #[test]
    fn off_or_no_channel_never_checks() {
        use UpdatePolicyMode::{Auto, Notify, Off};
        let stable = Some(UpdateChannel::Stable);
        assert!(checks(Auto, stable));
        assert!(checks(Notify, stable));
        assert!(!checks(Off, stable));
        assert!(!checks(Auto, None));
        assert!(!checks(Notify, None));
    }

    /// Policy x decision table: the decision of `decide` under each mode, then the state.
    #[test]
    fn policy_and_decision_table() {
        let current = v("1.0.0");
        let releases = [v("1.0.0"), v("1.1.0")];
        let rows = [
            (
                UpdatePolicyMode::Auto,
                CheckAction::Install(v("1.1.0")),
                None,
            ),
            (
                UpdatePolicyMode::Notify,
                CheckAction::Done,
                Some("1.1.0".to_owned()),
            ),
        ];
        for (policy, action, available) in rows {
            let decision = crate::decide(
                &current,
                Channel::Stable,
                &releases,
                &decision_policy(policy),
                &[],
            );
            let (state, got) = after_check(UpdateState::default(), &current, &decision, at(10));
            assert_eq!(got, action, "{policy}");
            assert_eq!(state.available, available, "{policy}");
            assert_eq!(state.last_check, Some(at(10)));
            assert_eq!(state.pending.is_some(), policy == UpdatePolicyMode::Auto);
        }
    }

    #[test]
    fn up_to_date_clears_available() {
        let state = UpdateState {
            available: Some("1.1.0".to_owned()),
            ..UpdateState::default()
        };
        let (state, action) = after_check(state, &v("1.1.0"), &UpdateDecision::UpToDate, at(10));
        assert_eq!(action, CheckAction::Done);
        assert_eq!(state.available, None);
        assert_eq!(
            state.last_result,
            Some(UpdateResult::UpToDate { at: at(10) })
        );
    }

    #[test]
    fn failed_versions_are_never_applied_again() {
        let current = v("1.0.0");
        let state = with_failed(UpdateState::default(), &v("1.1.0"));
        let decision = crate::decide(
            &current,
            Channel::Stable,
            &[v("1.1.0")],
            &decision_policy(UpdatePolicyMode::Auto),
            &failed_versions(&state),
        );
        let (state, action) = after_check(state, &current, &decision, at(5));
        assert_eq!(action, CheckAction::Done);
        assert_eq!(state.pending, None);
        assert_eq!(
            state.last_result,
            Some(UpdateResult::SkippedFailed {
                at: at(5),
                version: "1.1.0".to_owned()
            })
        );
    }

    #[test]
    fn with_failed_adds_once_and_skips_garbage_on_read() {
        let mut state = UpdateState::default();
        state.failed_versions.push("garbage".to_owned());
        let state = with_failed(with_failed(state, &v("1.1.0")), &v("1.1.0+build"));
        assert_eq!(state.failed_versions, ["garbage", "1.1.0"]);
        assert_eq!(failed_versions(&state), [v("1.1.0")]);
    }

    #[test]
    fn upgrade_transitions() {
        let (from, to) = (v("1.0.0"), v("1.1.0"));
        let decision = UpdateDecision::Apply {
            version: to.clone(),
        };
        let (pending, _) = after_check(UpdateState::default(), &from, &decision, at(1));
        assert_eq!(
            pending.pending,
            Some(PendingUpgrade {
                from: "1.0.0".to_owned(),
                to: "1.1.0".to_owned(),
                started_at: at(1),
            })
        );

        let done = after_upgraded(pending.clone(), &from, &to, at(2));
        assert_eq!(done.pending, None);
        assert_eq!(done.failed_versions, Vec::<String>::new());
        assert!(matches!(
            done.last_result,
            Some(UpdateResult::Upgraded { .. })
        ));

        let rolled = after_rolled_back(pending.clone(), &from, &to, "dead".to_owned(), at(3));
        assert_eq!(rolled.pending, None);
        assert_eq!(rolled.failed_versions, ["1.1.0"]);
        assert!(matches!(
            rolled.last_result,
            Some(UpdateResult::RolledBack { ref message, .. }) if message == "dead"
        ));

        let rejected = after_preflight_failed(pending.clone(), &to, "exit 1".to_owned(), at(4));
        assert_eq!(rejected.pending, None);
        assert_eq!(rejected.failed_versions, ["1.1.0"]);

        let errored = after_upgrade_error(pending, "HTTP 500".to_owned(), at(5));
        assert_eq!(errored.pending, None);
        assert!(errored.failed_versions.is_empty(), "may work next time");
        assert_eq!(
            errored.last_result,
            Some(UpdateResult::Failed {
                at: at(5),
                message: "HTTP 500".to_owned()
            })
        );

        let failed_check = after_check_failed(UpdateState::default(), "offline".to_owned(), at(6));
        assert_eq!(failed_check.last_check, Some(at(6)));
    }

    #[test]
    fn stale_pending_is_cleared_and_recorded() {
        let state = UpdateState {
            pending: Some(PendingUpgrade {
                from: "1.0.0".to_owned(),
                to: "1.1.0".to_owned(),
                started_at: at(1_000),
            }),
            ..UpdateState::default()
        };
        let fresh = 1_000 + HOUR;
        assert!(blocking_pending(&state, fresh).is_some());
        assert_eq!(clear_stale(state.clone(), at(fresh)), state);

        let late = fresh + 1;
        assert!(blocking_pending(&state, late).is_none());
        let cleared = clear_stale(state, at(late));
        assert_eq!(cleared.pending, None);
        assert_eq!(
            cleared.last_result,
            Some(UpdateResult::Interrupted {
                at: at(late),
                from: "1.0.0".to_owned(),
                to: "1.1.0".to_owned(),
            })
        );
    }

    #[test]
    fn jitter_stays_within_ten_percent() {
        let hour = Duration::from_secs(HOUR);
        assert_eq!(jittered(hour, JITTER, 0.0), Duration::from_mins(54));
        assert_eq!(jittered(hour, JITTER, 0.5), hour);
        assert_eq!(jittered(hour, JITTER, 1.0), Duration::from_mins(66));
        // Out-of-range randomness is clamped.
        assert_eq!(jittered(hour, JITTER, -3.0), Duration::from_mins(54));
        assert_eq!(jittered(hour, JITTER, 7.0), Duration::from_mins(66));
    }

    #[test]
    fn first_check_is_minutes_after_start_never_at_boot() {
        let six_hours = Duration::from_secs(6 * HOUR);
        let start = 1_000_000;
        assert_eq!(next_check(None, start, six_hours, JITTER, 0.5), start + 300);
        assert_eq!(next_check(None, start, six_hours, JITTER, 0.0), start + 270);
        assert_eq!(next_check(None, start, six_hours, JITTER, 1.0), start + 330);
        // A check overdue before a restart still waits for the initial delay.
        assert_eq!(
            next_check(Some(start - 10 * HOUR), start, six_hours, JITTER, 0.5),
            start + 300
        );
        // A short interval (test hook) shortens the initial delay.
        assert_eq!(
            next_check(None, start, Duration::from_secs(2), JITTER, 0.5),
            start + 2
        );
    }

    #[test]
    fn later_checks_follow_the_last_one() {
        let six_hours = Duration::from_secs(6 * HOUR);
        let start = 1_000_000;
        let last = start + HOUR;
        assert_eq!(
            next_check(Some(last), start, six_hours, JITTER, 0.5),
            last + 6 * HOUR
        );
        assert_eq!(
            next_check(Some(last), start, six_hours, JITTER, 0.0),
            last + 6 * HOUR - 2_160
        );
        assert_eq!(
            next_check(Some(last), start, six_hours, JITTER, 1.0),
            last + 6 * HOUR + 2_160
        );
    }

    #[test]
    fn grace_is_a_quarter_of_the_deadline_capped() {
        assert_eq!(restart_grace(HEALTH_DEADLINE), Duration::from_secs(15));
        assert_eq!(
            restart_grace(Duration::from_secs(8)),
            Duration::from_secs(2)
        );
        assert_eq!(restart_grace(Duration::from_secs(600)), MAX_RESTART_GRACE);
    }

    #[test]
    fn status_hides_an_available_version_already_installed() {
        let state = UpdateState {
            available: Some("1.1.0".to_owned()),
            failed_versions: vec!["1.2.0".to_owned()],
            ..UpdateState::default()
        };
        let interval = Duration::from_secs(6 * HOUR);
        let shown = |running: &str| {
            status(
                state.clone(),
                UpdatePolicyMode::Notify,
                Some(UpdateChannel::Stable),
                interval,
                Some(&v(running)),
            )
        };
        assert_eq!(shown("1.0.0").available.as_deref(), Some("1.1.0"));
        assert_eq!(shown("1.1.0").available, None);
        let status = shown("1.0.0");
        assert_eq!(status.check_interval_secs, 6 * HOUR);
        assert_eq!(status.failed_versions, ["1.2.0"]);
        assert_eq!(status.policy, UpdatePolicyMode::Notify);
    }
}
