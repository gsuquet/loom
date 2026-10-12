//! Update decision: which release, if any, to move to.
//!
//! [`decide`] is a pure function: it takes the running version, the channel,
//! the published releases, the update policy and the versions that failed a
//! health check, and returns an [`UpdateDecision`]. It performs no I/O.
//!
//! # Rules
//!
//! Applied in this order:
//!
//! 1. [`UpdateMode::Off`] returns [`SkipReason::UpdatesOff`], whatever the releases.
//! 2. With a pinned version, the pin is the only target, whatever the channel:
//!    - not among the releases: [`SkipReason::PinnedNotReleased`];
//!    - in `failed_versions`: [`SkipReason::Failed`];
//!    - lower than the current version: [`SkipReason::PinnedBelowCurrent`] (never downgrade);
//!    - equal to the current version: [`UpdateDecision::UpToDate`];
//!    - higher: the target.
//! 3. Without a pin, the candidates are the releases of the channel
//!    ([`Channel::Stable`] drops prereleases, [`Channel::Dev`] keeps them) that
//!    are higher than the current version. Failed versions are dropped and the
//!    highest remaining candidate is the target. No candidate at all gives
//!    [`UpdateDecision::UpToDate`]; candidates that all failed give
//!    [`SkipReason::Failed`] with the highest failed version.
//! 4. A target becomes [`UpdateDecision::Available`] under
//!    [`UpdateMode::Notify`] and [`UpdateDecision::Apply`] under
//!    [`UpdateMode::Auto`].
//!
//! Versions are compared by semver precedence: build metadata is ignored, and
//! a dev prerelease `X.Y.Z-dev.N` sorts before the release `X.Y.Z`.

use std::cmp::Ordering;

use semver::Version;

/// Release channel the installation follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Releases only (tags `vX.Y.Z`); prereleases are ignored.
    Stable,
    /// Releases and dev prereleases (`X.Y.Z-dev.N`).
    Dev,
}

/// What to do when a newer version exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateMode {
    /// Install the newer version.
    Auto,
    /// Report the newer version without installing it.
    Notify,
    /// Do not look for updates.
    Off,
}

/// Update policy: a mode and an optional pinned version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePolicy {
    /// What to do with a newer version.
    pub mode: UpdateMode,
    /// When set, the only version the updater may move to, whatever the channel.
    pub pinned: Option<Version>,
}

/// Outcome of [`decide`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateDecision {
    /// No version to move to.
    UpToDate,
    /// A newer version exists; the policy says to report it only.
    Available {
        /// Version to report.
        version: Version,
    },
    /// A newer version exists; the policy says to install it.
    Apply {
        /// Version to install.
        version: Version,
    },
    /// No update, for the given reason.
    Skipped {
        /// Why no update happens.
        reason: SkipReason,
    },
}

/// Why [`decide`] returned [`UpdateDecision::Skipped`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// The policy mode is [`UpdateMode::Off`].
    UpdatesOff,
    /// The pinned version is not among the published releases.
    PinnedNotReleased {
        /// Pinned version.
        pinned: Version,
    },
    /// The pinned version is lower than the current one; downgrading is a rollback.
    PinnedBelowCurrent {
        /// Pinned version.
        pinned: Version,
    },
    /// The target version failed a health check.
    Failed {
        /// Pinned version, or the highest newer version, that failed.
        version: Version,
    },
}

/// Decides which update action to take. Pure: no I/O. See the module docs for the rules.
#[must_use]
pub fn decide(
    current: &Version,
    channel: Channel,
    releases: &[Version],
    policy: &UpdatePolicy,
    failed_versions: &[Version],
) -> UpdateDecision {
    let on_target: fn(Version) -> UpdateDecision = match policy.mode {
        UpdateMode::Off => return skipped(SkipReason::UpdatesOff),
        UpdateMode::Auto => |version| UpdateDecision::Apply { version },
        UpdateMode::Notify => |version| UpdateDecision::Available { version },
    };
    let target = policy.pinned.as_ref().map_or_else(
        || channel_target(current, channel, releases, failed_versions),
        |pinned| pinned_target(current, releases, pinned, failed_versions),
    );
    match target {
        Target::None => UpdateDecision::UpToDate,
        Target::Skip(reason) => skipped(reason),
        Target::Version(version) => on_target(version),
    }
}

/// Target chosen before the mode is applied.
enum Target {
    None,
    Version(Version),
    Skip(SkipReason),
}

const fn skipped(reason: SkipReason) -> UpdateDecision {
    UpdateDecision::Skipped { reason }
}

fn same(a: &Version, b: &Version) -> bool {
    a.cmp_precedence(b) == Ordering::Equal
}

fn is_failed(version: &Version, failed_versions: &[Version]) -> bool {
    failed_versions.iter().any(|failed| same(failed, version))
}

fn pinned_target(
    current: &Version,
    releases: &[Version],
    pinned: &Version,
    failed_versions: &[Version],
) -> Target {
    let Some(release) = releases.iter().find(|release| same(release, pinned)) else {
        return Target::Skip(SkipReason::PinnedNotReleased {
            pinned: pinned.clone(),
        });
    };
    if is_failed(release, failed_versions) {
        return Target::Skip(SkipReason::Failed {
            version: release.clone(),
        });
    }
    match release.cmp_precedence(current) {
        Ordering::Less => Target::Skip(SkipReason::PinnedBelowCurrent {
            pinned: pinned.clone(),
        }),
        Ordering::Equal => Target::None,
        Ordering::Greater => Target::Version(release.clone()),
    }
}

fn channel_target(
    current: &Version,
    channel: Channel,
    releases: &[Version],
    failed_versions: &[Version],
) -> Target {
    let highest = |candidates: &mut dyn Iterator<Item = &Version>| {
        candidates.max_by(|a, b| a.cmp_precedence(b)).cloned()
    };
    let newer = || {
        releases.iter().filter(move |release| {
            let in_channel = match channel {
                Channel::Stable => release.pre.is_empty(),
                Channel::Dev => true,
            };
            in_channel && release.cmp_precedence(current) == Ordering::Greater
        })
    };
    highest(&mut newer().filter(|r| !is_failed(r, failed_versions))).map_or_else(
        // Every newer candidate failed, or there is none.
        || {
            highest(&mut newer()).map_or(Target::None, |version| {
                Target::Skip(SkipReason::Failed { version })
            })
        },
        Target::Version,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn vs(list: &[&str]) -> Vec<Version> {
        list.iter().map(|s| v(s)).collect()
    }

    /// Expected result before the mode is applied.
    #[derive(Debug)]
    enum Outcome {
        UpToDate,
        Target(&'static str),
        Skip(SkipReason),
    }

    struct Case {
        name: &'static str,
        current: &'static str,
        releases: &'static [&'static str],
        failed: &'static [&'static str],
        pinned: Option<&'static str>,
        stable: Outcome,
        dev: Outcome,
    }

    fn expected(outcome: &Outcome, mode: UpdateMode) -> UpdateDecision {
        match (mode, outcome) {
            (UpdateMode::Off, _) => UpdateDecision::Skipped {
                reason: SkipReason::UpdatesOff,
            },
            (_, Outcome::UpToDate) => UpdateDecision::UpToDate,
            (_, Outcome::Skip(reason)) => UpdateDecision::Skipped {
                reason: reason.clone(),
            },
            (UpdateMode::Notify, Outcome::Target(t)) => UpdateDecision::Available { version: v(t) },
            (UpdateMode::Auto, Outcome::Target(t)) => UpdateDecision::Apply { version: v(t) },
        }
    }

    #[allow(clippy::too_many_lines)] // Data table: one entry per rule.
    fn cases() -> Vec<Case> {
        use Outcome::{Skip, Target, UpToDate};
        vec![
            Case {
                name: "newer release",
                current: "1.0.0",
                releases: &["0.9.0", "1.0.0", "1.1.0", "1.2.0"],
                failed: &[],
                pinned: None,
                stable: Target("1.2.0"),
                dev: Target("1.2.0"),
            },
            Case {
                name: "same version",
                current: "1.0.0",
                releases: &["0.9.0", "1.0.0"],
                failed: &[],
                pinned: None,
                stable: UpToDate,
                dev: UpToDate,
            },
            Case {
                name: "only older releases: never downgrade",
                current: "1.0.0",
                releases: &["0.8.0", "0.9.0"],
                failed: &[],
                pinned: None,
                stable: UpToDate,
                dev: UpToDate,
            },
            Case {
                name: "no release at all",
                current: "1.0.0",
                releases: &[],
                failed: &[],
                pinned: None,
                stable: UpToDate,
                dev: UpToDate,
            },
            Case {
                name: "newer prerelease only",
                current: "1.0.0",
                releases: &["1.0.0", "1.1.0-dev.3", "1.1.0-dev.12"],
                failed: &[],
                pinned: None,
                stable: UpToDate,
                dev: Target("1.1.0-dev.12"),
            },
            Case {
                name: "release supersedes its dev prereleases",
                current: "1.1.0-dev.4",
                releases: &["1.0.0", "1.1.0-dev.4", "1.1.0"],
                failed: &[],
                pinned: None,
                stable: Target("1.1.0"),
                dev: Target("1.1.0"),
            },
            Case {
                name: "dev build ahead of the last release",
                current: "1.1.0-dev.4",
                releases: &["1.0.0", "1.1.0-dev.2", "1.1.0-dev.4"],
                failed: &[],
                pinned: None,
                stable: UpToDate,
                dev: UpToDate,
            },
            Case {
                name: "build metadata is ignored",
                current: "1.0.0+abc",
                releases: &["1.0.0+def"],
                failed: &[],
                pinned: None,
                stable: UpToDate,
                dev: UpToDate,
            },
            Case {
                name: "highest failed: fall back to the next newer one",
                current: "1.0.0",
                releases: &["1.0.0", "1.1.0", "1.2.0-dev.1", "1.2.0"],
                failed: &["1.2.0", "1.2.0-dev.1"],
                pinned: None,
                stable: Target("1.1.0"),
                dev: Target("1.1.0"),
            },
            Case {
                name: "every newer version failed",
                current: "1.0.0",
                releases: &["1.0.0", "1.1.0", "1.2.0-dev.1"],
                failed: &["1.1.0", "1.2.0-dev.1"],
                pinned: None,
                stable: Skip(SkipReason::Failed {
                    version: v("1.1.0"),
                }),
                dev: Skip(SkipReason::Failed {
                    version: v("1.2.0-dev.1"),
                }),
            },
            Case {
                name: "failed version that is not newer is irrelevant",
                current: "1.0.0",
                releases: &["0.9.0", "1.0.0"],
                failed: &["0.9.0"],
                pinned: None,
                stable: UpToDate,
                dev: UpToDate,
            },
            Case {
                name: "pinned newer: pin wins over the highest",
                current: "1.0.0",
                releases: &["1.0.0", "1.1.0", "1.2.0", "1.3.0-dev.1"],
                failed: &[],
                pinned: Some("1.1.0"),
                stable: Target("1.1.0"),
                dev: Target("1.1.0"),
            },
            Case {
                name: "pinned prerelease: pin wins over the channel",
                current: "1.0.0",
                releases: &["1.0.0", "1.1.0-dev.2"],
                failed: &[],
                pinned: Some("1.1.0-dev.2"),
                stable: Target("1.1.0-dev.2"),
                dev: Target("1.1.0-dev.2"),
            },
            Case {
                name: "pinned equals current",
                current: "1.1.0",
                releases: &["1.1.0", "1.2.0"],
                failed: &[],
                pinned: Some("1.1.0"),
                stable: UpToDate,
                dev: UpToDate,
            },
            Case {
                name: "pinned older: never downgrade",
                current: "1.2.0",
                releases: &["1.1.0", "1.2.0"],
                failed: &[],
                pinned: Some("1.1.0"),
                stable: Skip(SkipReason::PinnedBelowCurrent { pinned: v("1.1.0") }),
                dev: Skip(SkipReason::PinnedBelowCurrent { pinned: v("1.1.0") }),
            },
            Case {
                name: "pinned not released",
                current: "1.0.0",
                releases: &["1.0.0", "1.1.0"],
                failed: &[],
                pinned: Some("1.5.0"),
                stable: Skip(SkipReason::PinnedNotReleased { pinned: v("1.5.0") }),
                dev: Skip(SkipReason::PinnedNotReleased { pinned: v("1.5.0") }),
            },
            Case {
                name: "pinned failed",
                current: "1.0.0",
                releases: &["1.0.0", "1.1.0", "1.2.0"],
                failed: &["1.1.0"],
                pinned: Some("1.1.0"),
                stable: Skip(SkipReason::Failed {
                    version: v("1.1.0"),
                }),
                dev: Skip(SkipReason::Failed {
                    version: v("1.1.0"),
                }),
            },
        ]
    }

    #[test]
    fn decision_table() {
        let mut failures = Vec::new();
        for case in cases() {
            let current = v(case.current);
            let releases = vs(case.releases);
            let failed = vs(case.failed);
            let pinned = case.pinned.map(v);
            for (channel, outcome) in [(Channel::Stable, &case.stable), (Channel::Dev, &case.dev)] {
                for mode in [UpdateMode::Auto, UpdateMode::Notify, UpdateMode::Off] {
                    let policy = UpdatePolicy {
                        mode,
                        pinned: pinned.clone(),
                    };
                    let got = decide(&current, channel, &releases, &policy, &failed);
                    let want = expected(outcome, mode);
                    if got != want {
                        failures.push(format!(
                            "{} [{channel:?}, {mode:?}]: got {got:?}, want {want:?}",
                            case.name
                        ));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn release_order_does_not_matter() {
        let policy = UpdatePolicy {
            mode: UpdateMode::Auto,
            pinned: None,
        };
        let releases = vs(&["1.2.0", "0.9.0", "1.1.0-dev.1", "1.1.0"]);
        assert_eq!(
            decide(&v("1.0.0"), Channel::Dev, &releases, &policy, &[]),
            UpdateDecision::Apply {
                version: v("1.2.0")
            }
        );
    }

    #[test]
    fn failed_versions_match_by_precedence() {
        let policy = UpdatePolicy {
            mode: UpdateMode::Notify,
            pinned: None,
        };
        let releases = vs(&["1.0.0", "1.1.0+build.1"]);
        assert_eq!(
            decide(
                &v("1.0.0"),
                Channel::Stable,
                &releases,
                &policy,
                &vs(&["1.1.0"])
            ),
            UpdateDecision::Skipped {
                reason: SkipReason::Failed {
                    version: v("1.1.0+build.1")
                }
            }
        );
    }
}
