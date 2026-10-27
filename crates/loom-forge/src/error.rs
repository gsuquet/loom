//! Errors of the payload parsers, and the error bodies of the forges.

use std::fmt;

use loom_api::Timestamp;
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer};
use thiserror::Error;

/// A forge payload that loom cannot read.
#[derive(Debug, Error)]
pub enum ParseError {
    /// The payload is not JSON, or a field that loom reads is missing or has the wrong type.
    #[error("invalid {payload} payload: {source}")]
    Json {
        /// What the payload is, such as `GitHub pull request`.
        payload: &'static str,
        /// The error of the JSON parser, with its line and column.
        #[source]
        source: serde_json::Error,
    },
    /// The payload is JSON of the expected shape, but a value is not valid.
    #[error("invalid {payload} payload: {reason}")]
    Invalid {
        /// What the payload is.
        payload: &'static str,
        /// What is not valid.
        reason: String,
    },
    /// A GraphQL response with errors.
    #[error("GraphQL errors in the {payload} response: {}", list(errors))]
    Graphql {
        /// What the payload is.
        payload: &'static str,
        /// The errors, as the forge gives them.
        errors: Vec<GraphqlError>,
    },
}

/// One error of a GraphQL response. Code matches on `kind` and on the step, never on
/// `message`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GraphqlError {
    /// The `type` of the error, such as `UNPROCESSABLE` or `NOT_FOUND`, when the forge gives one.
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// The path of the field that failed, such as `addPullRequestReview`.
    #[serde(default, deserialize_with = "path")]
    pub path: Vec<String>,
    /// The message, for humans.
    pub message: String,
}

impl fmt::Display for GraphqlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(kind) = &self.kind {
            write!(f, "{kind}: ")?;
        }
        if !self.path.is_empty() {
            write!(f, "{}: ", self.path.join("."))?;
        }
        f.write_str(&self.message)
    }
}

fn list(errors: &[GraphqlError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// GraphQL paths mix field names and list indexes.
fn path<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let values = Option::<Vec<serde_json::Value>>::deserialize(deserializer)?;
    Ok(values
        .unwrap_or_default()
        .into_iter()
        .map(|value| match value {
            serde_json::Value::String(text) => text,
            other => other.to_string(),
        })
        .collect())
}

/// The body of an error response of a forge REST API (4xx, 5xx). Code matches on the HTTP
/// status, never on these texts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorBody {
    /// The main message.
    pub message: String,
    /// More messages, such as the validation errors of a 422 or a 400.
    pub details: Vec<String>,
}

/// Deserializes `text` as `T`, a payload of kind `payload`.
pub fn json<T: DeserializeOwned>(payload: &'static str, body: &[u8]) -> Result<T, ParseError> {
    serde_json::from_slice(body).map_err(|source| ParseError::Json { payload, source })
}

/// A [`ParseError::Invalid`].
pub fn invalid(payload: &'static str, reason: impl fmt::Display) -> ParseError {
    ParseError::Invalid {
        payload,
        reason: reason.to_string(),
    }
}

/// An RFC 3339 instant of a forge (fractional seconds and offsets allowed), reduced to whole
/// seconds in UTC.
#[derive(Debug, Clone, Copy)]
pub struct ForgeTime(pub Timestamp);

impl<'de> Deserialize<'de> for ForgeTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let instant: jiff::Timestamp = text.parse().map_err(|error| {
            D::Error::custom(format!("`{text}` is not an RFC 3339 instant: {error}"))
        })?;
        u64::try_from(instant.as_second())
            .ok()
            .and_then(Timestamp::from_unix_secs)
            .map(Self)
            .ok_or_else(|| D::Error::custom(format!("`{text}` is before 1970")))
    }
}

#[cfg(test)]
mod tests {
    use super::ForgeTime;

    #[test]
    fn forge_times_are_reduced_to_whole_seconds_in_utc() {
        for (text, expected) in [
            ("\"2026-10-21T16:10:00Z\"", "2026-10-21T16:10:00Z"),
            ("\"2026-10-21T16:10:00.999Z\"", "2026-10-21T16:10:00Z"),
            ("\"2026-10-21T18:10:00+02:00\"", "2026-10-21T16:10:00Z"),
        ] {
            let time: ForgeTime = serde_json::from_str(text).unwrap();
            assert_eq!(time.0.to_string(), expected, "{text}");
        }
        for text in ["\"2026-10-21\"", "\"1969-12-31T23:59:59Z\"", "1"] {
            assert!(serde_json::from_str::<ForgeTime>(text).is_err(), "{text}");
        }
    }
}
