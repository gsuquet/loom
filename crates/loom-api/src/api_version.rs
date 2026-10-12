//! Version of the loomd API.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Version of the loomd API a message belongs to.
///
/// A breaking change to any API type adds a variant; compatible changes (new optional fields,
/// new endpoints) keep the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ApiVersion {
    /// First version of the API.
    #[serde(rename = "v1")]
    V1,
}

impl ApiVersion {
    /// Version spoken by this build.
    pub const CURRENT: Self = Self::V1;

    /// Name used in the JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "v1",
        }
    }
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::ApiVersion;
    use serde_json::json;

    #[test]
    fn serializes_as_its_name_and_round_trips() {
        let value = serde_json::to_value(ApiVersion::V1).unwrap();
        assert_eq!(value, json!("v1"));
        assert_eq!(value, json!(ApiVersion::V1.as_str()));
        assert_eq!(
            serde_json::from_value::<ApiVersion>(value).unwrap(),
            ApiVersion::V1
        );
    }

    #[test]
    fn rejects_unknown_versions() {
        for text in ["v2", "V1", "1", ""] {
            assert!(
                serde_json::from_value::<ApiVersion>(json!(text)).is_err(),
                "{text:?}"
            );
        }
    }

    #[test]
    fn current_is_v1() {
        assert_eq!(ApiVersion::CURRENT, ApiVersion::V1);
        assert_eq!(ApiVersion::CURRENT.to_string(), "v1");
    }
}
