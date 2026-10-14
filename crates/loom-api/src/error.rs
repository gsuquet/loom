//! Error body returned by the loomd API.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Error returned by the loomd API instead of a response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApiError {
    /// Machine-readable category; clients branch on it, never on `message`.
    pub code: ErrorCode,
    /// Human-readable explanation, in English.
    pub message: String,
    /// Structured context for the error, such as the name of an invalid field. Its shape depends
    /// on `code` and the endpoint; absent when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ApiError {
    /// Builds an error without details.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }
}

/// Category of an `ApiError`. Closed set: a new code is an API change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The request is malformed: unparsable body, unknown or missing field, invalid value.
    /// Retrying the same request fails the same way.
    InvalidRequest,
    /// The addressed resource or endpoint does not exist.
    NotFound,
    /// The client asked for an API version this daemon does not serve.
    VersionMismatch,
    /// The daemon cannot serve the request right now (starting, shutting down, a dependency is
    /// down). The same request may succeed later.
    Unavailable,
    /// The daemon failed while handling a valid request: a bug or an unexpected I/O failure.
    Internal,
    /// A task ID prefix matches several tasks; `details` lists them.
    AmbiguousPrefix,
    /// The task cannot be changed: unknown kind, format or status, or an invalid `task.md`.
    ReadOnly,
    /// The task cannot move from its status to the requested one.
    IllegalTransition,
    /// An undo found a field changed since the change it reverts.
    Conflict,
    /// The task has no change to undo.
    NothingToUndo,
}

impl ErrorCode {
    /// Name used in the JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::NotFound => "not_found",
            Self::VersionMismatch => "version_mismatch",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
            Self::AmbiguousPrefix => "ambiguous_prefix",
            Self::ReadOnly => "read_only",
            Self::IllegalTransition => "illegal_transition",
            Self::Conflict => "conflict",
            Self::NothingToUndo => "nothing_to_undo",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::{ApiError, ErrorCode};
    use serde_json::json;

    const ALL_CODES: [ErrorCode; 10] = [
        ErrorCode::InvalidRequest,
        ErrorCode::NotFound,
        ErrorCode::VersionMismatch,
        ErrorCode::Unavailable,
        ErrorCode::Internal,
        ErrorCode::AmbiguousPrefix,
        ErrorCode::ReadOnly,
        ErrorCode::IllegalTransition,
        ErrorCode::Conflict,
        ErrorCode::NothingToUndo,
    ];

    #[test]
    fn codes_serialize_in_snake_case_and_round_trip() {
        for code in ALL_CODES {
            let value = serde_json::to_value(code).unwrap();
            assert_eq!(value, json!(code.as_str()));
            assert_eq!(code.to_string(), code.as_str());
            assert_eq!(serde_json::from_value::<ErrorCode>(value).unwrap(), code);
        }
        assert_eq!(
            serde_json::to_value(ErrorCode::InvalidRequest).unwrap(),
            json!("invalid_request")
        );
    }

    #[test]
    fn rejects_unknown_codes() {
        for text in ["InvalidRequest", "forbidden", ""] {
            assert!(
                serde_json::from_value::<ErrorCode>(json!(text)).is_err(),
                "{text:?}"
            );
        }
    }

    #[test]
    fn error_without_details_omits_the_field_and_round_trips() {
        let error = ApiError::new(ErrorCode::NotFound, "no such task");
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(
            value,
            json!({ "code": "not_found", "message": "no such task" })
        );
        assert_eq!(serde_json::from_value::<ApiError>(value).unwrap(), error);
    }

    #[test]
    fn error_with_details_round_trips() {
        let error = ApiError {
            details: Some(json!({ "field": "priority", "expected": "integer" })),
            ..ApiError::new(ErrorCode::InvalidRequest, "invalid priority")
        };
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["details"]["field"], json!("priority"));
        assert_eq!(serde_json::from_value::<ApiError>(value).unwrap(), error);
    }

    #[test]
    fn accepts_unknown_fields_for_forward_compatibility() {
        let value = json!({ "code": "internal", "message": "boom", "trace_id": "abc" });
        assert_eq!(
            serde_json::from_value::<ApiError>(value).unwrap(),
            ApiError::new(ErrorCode::Internal, "boom")
        );
    }

    #[test]
    fn requires_code_and_message() {
        assert!(serde_json::from_value::<ApiError>(json!({ "code": "internal" })).is_err());
        assert!(serde_json::from_value::<ApiError>(json!({ "message": "boom" })).is_err());
    }
}
