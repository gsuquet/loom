//! State of a running loomd, returned by its status endpoint.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ApiVersion, Timestamp, VersionInfo};

/// State of a running loomd.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatusResponse {
    /// API version the daemon serves.
    pub api_version: ApiVersion,
    /// Build information of the daemon binary.
    pub daemon: VersionInfo,
    /// Process ID of the daemon.
    pub pid: u32,
    /// Instant the daemon started.
    pub started_at: Timestamp,
    /// Whole seconds elapsed since `started_at`.
    pub uptime_secs: u64,
    /// Absolute path of the loom home directory the daemon uses.
    pub loom_home: PathBuf,
    /// Absolute path of the unix socket the daemon listens on.
    pub socket_path: PathBuf,
    /// Absolute path of the configuration file the daemon loaded.
    pub config_path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::StatusResponse;
    use crate::{ApiVersion, Channel, Profile, Timestamp, VersionInfo};
    use serde_json::json;

    fn sample() -> StatusResponse {
        StatusResponse {
            api_version: ApiVersion::V1,
            daemon: VersionInfo {
                version: "0.1.0-dev.42".to_owned(),
                commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                commit_date: "2026-10-12T10:00:00+02:00".to_owned(),
                target: "aarch64-apple-darwin".to_owned(),
                channel: Channel::Dev,
                profile: Profile::Release,
            },
            pid: 4242,
            started_at: Timestamp::from_unix_secs(1_791_792_000).unwrap(),
            uptime_secs: 90,
            loom_home: "/home/me/.local/share/loom".into(),
            socket_path: "/run/user/1000/loom/loomd.sock".into(),
            config_path: "/home/me/.config/loom/config.toml".into(),
        }
    }

    #[test]
    fn serializes_every_field() {
        assert_eq!(
            serde_json::to_value(sample()).unwrap(),
            json!({
                "api_version": "v1",
                "daemon": {
                    "version": "0.1.0-dev.42",
                    "commit": "0123456789abcdef0123456789abcdef01234567",
                    "commit_date": "2026-10-12T10:00:00+02:00",
                    "target": "aarch64-apple-darwin",
                    "channel": "dev",
                    "profile": "release",
                },
                "pid": 4242,
                "started_at": "2026-10-12T08:00:00Z",
                "uptime_secs": 90,
                "loom_home": "/home/me/.local/share/loom",
                "socket_path": "/run/user/1000/loom/loomd.sock",
                "config_path": "/home/me/.config/loom/config.toml",
            })
        );
    }

    #[test]
    fn round_trips_through_json() {
        let text = serde_json::to_string(&sample()).unwrap();
        assert_eq!(
            serde_json::from_str::<StatusResponse>(&text).unwrap(),
            sample()
        );
    }

    #[test]
    fn accepts_unknown_fields_for_forward_compatibility() {
        let mut value = serde_json::to_value(sample()).unwrap();
        value["added_in_a_later_release"] = json!(true);
        assert_eq!(
            serde_json::from_value::<StatusResponse>(value).unwrap(),
            sample()
        );
    }

    #[test]
    fn rejects_missing_fields_and_invalid_values() {
        let mut missing = serde_json::to_value(sample()).unwrap();
        missing.as_object_mut().unwrap().remove("pid");
        assert!(serde_json::from_value::<StatusResponse>(missing).is_err());

        let mut offset = serde_json::to_value(sample()).unwrap();
        offset["started_at"] = json!("2026-10-12T10:00:00+02:00");
        assert!(serde_json::from_value::<StatusResponse>(offset).is_err());

        let mut version = serde_json::to_value(sample()).unwrap();
        version["api_version"] = json!("v2");
        assert!(serde_json::from_value::<StatusResponse>(version).is_err());
    }
}
