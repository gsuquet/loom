//! Output of `loom upgrade` and `loom rollback` (`--json`).

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Channel;

/// Output of `loom upgrade`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UpgradeReport {
    /// Version of the `loom` that runs.
    pub current_version: String,
    /// Where the new version comes from.
    pub source: UpgradeSource,
    /// Which version, if any, to move to.
    pub decision: UpgradeDecision,
    /// `true` with `--check`: the decision only, nothing downloaded or installed.
    pub check: bool,
    /// `true` with `--dry-run`: downloaded and verified, nothing installed.
    pub dry_run: bool,
    /// What was installed, or would be with `--dry-run`. Absent with `--check` and when the
    /// decision installs nothing.
    pub install: Option<InstallReport>,
}

/// Where `loom upgrade` takes the new version from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpgradeSource {
    /// A GitHub release.
    Release {
        /// Channel followed: `--channel`, or the channel of the build. Absent with `--version`.
        channel: Option<Channel>,
        /// Version given with `--version`.
        pinned: Option<String>,
    },
    /// A local archive or directory given with `--from`. Never verified.
    Local {
        /// The path given to `--from`, made absolute.
        path: PathBuf,
    },
}

/// Which version, if any, `loom upgrade` moves to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpgradeDecision {
    /// No version to move to.
    UpToDate,
    /// A newer version exists. The policy says to report it only.
    Available {
        /// Version to report.
        version: String,
    },
    /// Install this version.
    Apply {
        /// Version to install.
        version: String,
    },
    /// No upgrade, for the given reason.
    Skipped {
        /// Why no upgrade happens.
        reason: UpgradeSkipReason,
    },
}

/// Why no upgrade happens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpgradeSkipReason {
    /// Updates are turned off.
    UpdatesOff,
    /// The pinned version (`--version`) is not a published release.
    PinnedNotReleased {
        /// Pinned version.
        pinned: String,
    },
    /// The pinned version is lower than the current one: use `loom rollback`.
    PinnedBelowCurrent {
        /// Pinned version.
        pinned: String,
    },
    /// The target version failed a health check.
    Failed {
        /// Version that failed.
        version: String,
    },
}

/// Output of `loom rollback`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RollbackReport {
    /// Version of the `loom` that runs, replaced by the rollback.
    pub current_version: String,
    /// `true` with `--dry-run`: nothing installed.
    pub dry_run: bool,
    /// What was restored, or would be with `--dry-run`.
    pub install: InstallReport,
}

/// Installation of a new `loom` and `loomd`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InstallReport {
    /// Version installed, or that would be with `--dry-run`.
    pub version: String,
    /// Directory that holds `loom` and `loomd`: the directory of the `loom` that runs.
    pub install_dir: PathBuf,
    /// `true` when the signature of `SHA256SUMS` and the archive checksum were verified.
    /// `false` for `--from` and `loom rollback` (local files).
    pub verified: bool,
    /// `true` when the binaries were replaced. `false` with `--dry-run`.
    pub installed: bool,
    /// Directory where the replaced version was saved (`<loom home>/versions/<version>`).
    pub saved_to: Option<PathBuf>,
    /// Saved versions deleted to keep the most recent ones.
    pub pruned: Vec<String>,
    /// What happened to a loomd that runs.
    pub daemon: DaemonRestart,
}

/// What `loom upgrade` and `loom rollback` did to a loomd that runs, after they replaced the
/// binaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DaemonRestart {
    /// Nothing was replaced (`--dry-run`), so loomd was left alone.
    Skipped,
    /// No loomd holds the lock of the loom home.
    NotRunning,
    /// `SIGTERM` was sent. A service manager restarts loomd on the new binary, otherwise the
    /// next `loom` command starts it.
    Signalled {
        /// PID of the loomd that was signalled.
        pid: u32,
    },
    /// loomd runs, but `SIGTERM` could not be sent: it continues to run the replaced binary.
    Failed {
        /// PID read from the lock file, when there is one.
        pid: Option<u32>,
        /// Why.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{DaemonRestart, InstallReport, UpgradeDecision, UpgradeReport, UpgradeSource};
    use crate::Channel;
    use serde_json::json;

    #[test]
    fn serializes_tagged_enums_in_snake_case() {
        let report = UpgradeReport {
            current_version: "0.1.0".to_owned(),
            source: UpgradeSource::Release {
                channel: Some(Channel::Dev),
                pinned: None,
            },
            decision: UpgradeDecision::Apply {
                version: "0.2.0-dev.3".to_owned(),
            },
            check: false,
            dry_run: false,
            install: Some(InstallReport {
                version: "0.2.0-dev.3".to_owned(),
                install_dir: "/home/me/.local/bin".into(),
                verified: true,
                installed: true,
                saved_to: Some("/home/me/.loom/versions/0.1.0".into()),
                pruned: vec!["0.0.9".to_owned()],
                daemon: DaemonRestart::Signalled { pid: 42 },
            }),
        };
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            json!({
                "current_version": "0.1.0",
                "source": { "kind": "release", "channel": "dev", "pinned": null },
                "decision": { "kind": "apply", "version": "0.2.0-dev.3" },
                "check": false,
                "dry_run": false,
                "install": {
                    "version": "0.2.0-dev.3",
                    "install_dir": "/home/me/.local/bin",
                    "verified": true,
                    "installed": true,
                    "saved_to": "/home/me/.loom/versions/0.1.0",
                    "pruned": ["0.0.9"],
                    "daemon": { "kind": "signalled", "pid": 42 },
                },
            })
        );
    }
}
