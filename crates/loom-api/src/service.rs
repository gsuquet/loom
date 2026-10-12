//! State of the `loomd` user service, reported by `loom service status`.

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
    /// The `loomd` process of the service is running.
    pub running: bool,
}

#[cfg(test)]
mod tests {
    use super::{ServicePlatform, ServiceStatus};
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
}
