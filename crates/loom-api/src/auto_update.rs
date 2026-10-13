//! Auto-upgrade of loomd: its policy, its state file (`state/update.json`) and the `update`
//! object of the status response (ADR-0011).

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Timestamp;

/// What loomd does when a newer release exists (`[update] policy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum UpdatePolicyMode {
    /// Install it, with a pre-flight check and a watchdog that rolls back a failed release.
    Auto,
    /// Record it as available, shown by `loom status`; install nothing.
    Notify,
    /// Never check: no network access.
    Off,
}

impl UpdatePolicyMode {
    /// Name used in the configuration and the outputs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Notify => "notify",
            Self::Off => "off",
        }
    }
}

impl fmt::Display for UpdatePolicyMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Release channel loomd follows (`[update] channel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    /// Tagged releases only.
    Stable,
    /// Tagged releases and dev prereleases.
    Dev,
}

impl UpdateChannel {
    /// Name used in the configuration and the outputs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Dev => "dev",
        }
    }
}

impl fmt::Display for UpdateChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where the effective release channel comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChannelSource {
    /// `[update] channel` of `config.toml`.
    Config,
    /// Chosen explicitly with `loom upgrade --channel` (or `install.sh --channel`), kept in
    /// `state/update.json`.
    Chosen,
    /// The channel of the running build.
    Build,
}

impl ChannelSource {
    /// Name used in the outputs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Chosen => "chosen",
            Self::Build => "build",
        }
    }
}

impl fmt::Display for ChannelSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A release channel chosen explicitly, and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChosenChannel {
    /// The channel.
    pub name: UpdateChannel,
    /// Instant it was chosen.
    pub chosen_at: Timestamp,
}

/// Content of `<loom home>/state/update.json`.
///
/// Every field may be missing (an empty state). Unknown fields are accepted and kept when the
/// file is rewritten, so binaries of different versions share the file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateState {
    /// Release channel chosen explicitly with `loom upgrade --channel`: used when
    /// `[update] channel` is not set, instead of the build channel.
    #[serde(default)]
    pub channel: Option<ChosenChannel>,
    /// Instant of the last check, successful or not.
    #[serde(default)]
    pub last_check: Option<Timestamp>,
    /// Newer version found by the last check under the `notify` policy.
    #[serde(default)]
    pub available: Option<String>,
    /// Upgrade in progress, from the download to the watchdog's verdict.
    #[serde(default)]
    pub pending: Option<PendingUpgrade>,
    /// Versions that failed the pre-flight check or the watchdog: never applied automatically
    /// again.
    #[serde(default)]
    pub failed_versions: Vec<String>,
    /// Outcome of the last check or upgrade.
    #[serde(default)]
    pub last_result: Option<UpdateResult>,
    /// Fields this version does not know, written by another version: kept as they are when
    /// the file is rewritten.
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

/// An upgrade in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PendingUpgrade {
    /// Version running when the upgrade started.
    pub from: String,
    /// Version being installed.
    pub to: String,
    /// Instant the upgrade started (before the download).
    pub started_at: Timestamp,
}

/// Outcome of a check or an upgrade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdateResult {
    /// No newer version.
    UpToDate {
        /// Instant of the check.
        at: Timestamp,
    },
    /// A newer version exists; the policy is `notify`.
    Available {
        /// Instant of the check.
        at: Timestamp,
        /// Newer version.
        version: String,
    },
    /// Every newer version is in `failed_versions`.
    SkippedFailed {
        /// Instant of the check.
        at: Timestamp,
        /// Highest newer version, which failed before.
        version: String,
    },
    /// The new version answered the watchdog: the upgrade is complete.
    Upgraded {
        /// Instant of the verdict.
        at: Timestamp,
        /// Version replaced.
        from: String,
        /// Version installed.
        to: String,
    },
    /// The staged loomd failed the pre-flight check; nothing was installed.
    PreflightFailed {
        /// Instant of the verdict.
        at: Timestamp,
        /// Version rejected.
        version: String,
        /// Why.
        message: String,
    },
    /// The new loomd did not answer the watchdog in time: the previous version was restored.
    RolledBack {
        /// Instant of the rollback.
        at: Timestamp,
        /// Version restored.
        from: String,
        /// Version rejected.
        to: String,
        /// Why.
        message: String,
    },
    /// An upgrade in progress was abandoned (watchdog killed, machine restarted): found stale.
    Interrupted {
        /// Instant it was found stale.
        at: Timestamp,
        /// Version running when it started.
        from: String,
        /// Version it was installing.
        to: String,
    },
    /// The check or the upgrade failed before any swap: network, verification, install
    /// directory not writable, I/O.
    Failed {
        /// Instant of the failure.
        at: Timestamp,
        /// Why.
        message: String,
    },
}

/// Update state of a running loomd: its effective configuration and `state/update.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateStatus {
    /// Effective policy: `[update] policy`, or the default of the build channel.
    pub policy: UpdatePolicyMode,
    /// Effective channel: `[update] channel`, otherwise the channel chosen with
    /// `loom upgrade --channel`, otherwise the build channel. Absent for a local build without
    /// either: nothing is checked.
    pub channel: Option<UpdateChannel>,
    /// Where `channel` comes from; absent when `channel` is. Optional when reading, so that a
    /// client reads a daemon older than this field.
    #[serde(default)]
    pub channel_source: Option<ChannelSource>,
    /// Interval between two checks, in whole seconds, before jitter.
    pub check_interval_secs: u64,
    /// Instant of the last check.
    pub last_check: Option<Timestamp>,
    /// Newer version available (`notify`); absent when it is not newer than the daemon.
    pub available: Option<String>,
    /// Upgrade in progress.
    pub pending: Option<PendingUpgrade>,
    /// Versions never applied automatically again.
    pub failed_versions: Vec<String>,
    /// Outcome of the last check or upgrade.
    pub last_result: Option<UpdateResult>,
}

#[cfg(test)]
mod tests {
    use super::{
        ChosenChannel, PendingUpgrade, UpdateChannel, UpdatePolicyMode, UpdateResult, UpdateState,
    };
    use crate::Timestamp;
    use serde_json::json;

    fn at() -> Timestamp {
        Timestamp::from_unix_secs(1_791_792_000).unwrap()
    }

    #[test]
    fn empty_and_unknown_fields_parse() {
        assert_eq!(
            serde_json::from_str::<UpdateState>("{}").unwrap(),
            UpdateState::default()
        );
        let state: UpdateState =
            serde_json::from_value(json!({ "failed_versions": ["0.2.0"], "later": 1 })).unwrap();
        assert_eq!(state.failed_versions, ["0.2.0"]);
        assert_eq!(state.channel, None, "a state older than the field");
    }

    /// A version that does not know a field keeps it when it rewrites the file: a rollback to
    /// it does not lose what a newer version recorded.
    #[test]
    fn unknown_fields_survive_a_rewrite() {
        let written = json!({
            "failed_versions": [],
            "later": { "nested": [1, 2] },
            "other_later": "x",
        });
        let state: UpdateState = serde_json::from_value(written).unwrap();
        let rewritten = serde_json::to_value(&state).unwrap();
        assert_eq!(rewritten["later"], json!({ "nested": [1, 2] }));
        assert_eq!(rewritten["other_later"], json!("x"));
    }

    #[test]
    fn chosen_channel_round_trips() {
        let state = UpdateState {
            channel: Some(ChosenChannel {
                name: UpdateChannel::Dev,
                chosen_at: at(),
            }),
            ..UpdateState::default()
        };
        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(
            value["channel"],
            json!({ "name": "dev", "chosen_at": "2026-10-12T08:00:00Z" })
        );
        assert_eq!(serde_json::from_value::<UpdateState>(value).unwrap(), state);
    }

    #[test]
    fn serializes_tagged_results() {
        let state = UpdateState {
            channel: None,
            other: serde_json::Map::new(),
            last_check: Some(at()),
            available: None,
            pending: Some(PendingUpgrade {
                from: "0.1.0".to_owned(),
                to: "0.2.0".to_owned(),
                started_at: at(),
            }),
            failed_versions: vec![],
            last_result: Some(UpdateResult::RolledBack {
                at: at(),
                from: "0.1.0".to_owned(),
                to: "0.2.0".to_owned(),
                message: "no answer".to_owned(),
            }),
        };
        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(
            value["last_result"],
            json!({
                "kind": "rolled_back",
                "at": "2026-10-12T08:00:00Z",
                "from": "0.1.0",
                "to": "0.2.0",
                "message": "no answer",
            })
        );
        assert_eq!(serde_json::from_value::<UpdateState>(value).unwrap(), state);
    }

    #[test]
    fn policy_names() {
        for (mode, name) in [
            (UpdatePolicyMode::Auto, "auto"),
            (UpdatePolicyMode::Notify, "notify"),
            (UpdatePolicyMode::Off, "off"),
        ] {
            assert_eq!(mode.to_string(), name);
            assert_eq!(serde_json::to_value(mode).unwrap(), json!(name));
        }
    }
}
