//! State of the `loomd` user service, reported by `loom service status`, and the report of
//! `loom service restart`.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Service manager family the `loomd` user service is installed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ServicePlatform {
    /// macOS: a launchd user agent.
    Macos,
    /// Linux: a systemd user unit.
    Linux,
}

impl ServicePlatform {
    /// Lowercase name, as serialized.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Linux => "linux",
        }
    }
}

/// State of the `loomd` user service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceStatus {
    /// Service manager family.
    pub platform: ServicePlatform,
    /// Absolute path of the launchd property list or the systemd unit file.
    pub unit_path: PathBuf,
    /// The file at `unit_path` exists.
    pub installed: bool,
    /// The service manager knows the service: loaded in launchd, or enabled in systemd.
    pub loaded: bool,
    /// The `loomd` process of the service runs.
    pub running: bool,
}

/// Who restarted (or started) loomd for `loom service restart`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RestartVia {
    /// The launchd user agent: `launchctl kickstart -k`.
    Launchd,
    /// The systemd user unit: `systemctl --user restart`.
    Systemd,
    /// No service: `SIGTERM` to the loomd that holds the lock, then a new loomd started detached.
    Signal,
    /// No service and no loomd that runs: a new loomd started detached.
    Start,
}

impl RestartVia {
    /// Lowercase name, as serialized.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Launchd => "launchd",
            Self::Systemd => "systemd",
            Self::Signal => "signal",
            Self::Start => "start",
        }
    }
}

/// Output of `loom service restart`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonRestartReport {
    /// A loomd held the lock of the loom home when the command started.
    pub was_running: bool,
    /// PID of the loomd that ran before the restart, `null` if there was none.
    pub previous_pid: Option<u32>,
    /// PID of the loomd that answers now.
    pub pid: u32,
    /// Version of the loomd that answers now.
    pub version: String,
    /// Who restarted or started loomd.
    pub via: RestartVia,
    /// Milliseconds from the start of the restart to the first answer of the new loomd.
    pub elapsed_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::{DaemonRestartReport, RestartVia, ServicePlatform, ServiceStatus};
    use serde_json::json;

    fn sample() -> ServiceStatus {
        ServiceStatus {
            platform: ServicePlatform::Macos,
            unit_path: "/Users/me/Library/LaunchAgents/io.github.gsuquet.loomd.plist".into(),
            installed: true,
            loaded: true,
            running: false,
        }
    }

    #[test]
    fn serializes_every_field() {
        assert_eq!(
            serde_json::to_value(sample()).unwrap(),
            json!({
                "platform": "macos",
                "unit_path": "/Users/me/Library/LaunchAgents/io.github.gsuquet.loomd.plist",
                "installed": true,
                "loaded": true,
                "running": false,
            })
        );
    }

    #[test]
    fn platform_names() {
        for platform in [ServicePlatform::Macos, ServicePlatform::Linux] {
            assert_eq!(
                serde_json::to_value(platform).unwrap(),
                json!(platform.as_str())
            );
        }
        assert!(serde_json::from_value::<ServicePlatform>(json!("windows")).is_err());
    }

    #[test]
    fn accepts_unknown_fields_for_forward_compatibility() {
        let mut value = serde_json::to_value(sample()).unwrap();
        value["added_in_a_later_release"] = json!(true);
        assert_eq!(
            serde_json::from_value::<ServiceStatus>(value).unwrap(),
            sample()
        );
    }

    #[test]
    fn rejects_missing_fields() {
        let mut missing = serde_json::to_value(sample()).unwrap();
        missing.as_object_mut().unwrap().remove("running");
        assert!(serde_json::from_value::<ServiceStatus>(missing).is_err());
    }

    #[test]
    fn restart_report_serializes_every_field() {
        let report = DaemonRestartReport {
            was_running: true,
            previous_pid: Some(812),
            pid: 9431,
            version: "0.3.0-dev.12".to_owned(),
            via: RestartVia::Launchd,
            elapsed_ms: 640,
        };
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            json!({
                "was_running": true,
                "previous_pid": 812,
                "pid": 9431,
                "version": "0.3.0-dev.12",
                "via": "launchd",
                "elapsed_ms": 640,
            })
        );
        let not_running = DaemonRestartReport {
            was_running: false,
            previous_pid: None,
            via: RestartVia::Start,
            ..report
        };
        let value = serde_json::to_value(&not_running).unwrap();
        assert_eq!(value["previous_pid"], json!(null));
        assert_eq!(
            serde_json::from_value::<DaemonRestartReport>(value).unwrap(),
            not_running
        );
    }

    #[test]
    fn restart_via_names() {
        for via in [
            RestartVia::Launchd,
            RestartVia::Systemd,
            RestartVia::Signal,
            RestartVia::Start,
        ] {
            assert_eq!(serde_json::to_value(via).unwrap(), json!(via.as_str()));
        }
        assert!(serde_json::from_value::<RestartVia>(json!("kill")).is_err());
    }
}
