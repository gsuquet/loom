//! Diagnostics: the environment of `loomd` (`GET /v1/diagnostics`) and the report of
//! `loom repo doctor`.

use std::fmt;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What `SSH_AUTH_SOCK` points to in the environment of `loomd`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SshAgentSocket {
    /// `SSH_AUTH_SOCK` is unset or empty.
    Unset,
    /// Nothing exists at the path, or it cannot be examined.
    Missing {
        /// Value of `SSH_AUTH_SOCK`.
        path: PathBuf,
    },
    /// The path exists but is not a unix socket.
    NotASocket {
        /// Value of `SSH_AUTH_SOCK`.
        path: PathBuf,
    },
    /// The path is a unix socket.
    Socket {
        /// Value of `SSH_AUTH_SOCK`.
        path: PathBuf,
    },
}

/// Environment of `loomd` that its git operations depend on. Never carries a secret value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonDiagnostics {
    /// The SSH agent socket `loomd` and its `git` processes see.
    pub ssh_auth_sock: SshAgentSocket,
    /// `HOME` of `loomd` (expands `~` in the configuration); `null` when unset or empty.
    pub home: Option<PathBuf>,
}

/// Outcome of one check of `loom repo doctor`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    /// Nothing to fix.
    Ok,
    /// Something to look at; loom still works.
    Warn,
    /// Something loom needs is broken.
    Fail,
}

impl CheckStatus {
    /// Name, as serialized and printed.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

impl fmt::Display for CheckStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

/// One check of `loom repo doctor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorCheck {
    /// What was checked, such as `roots`, `contexts` or `ssh git@github.com`.
    pub check: String,
    /// Outcome.
    pub status: CheckStatus,
    /// What was found, in English.
    pub detail: String,
    /// How to fix it, in English; absent when there is nothing to fix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

/// Report of `loom repo doctor`: every check, in the order they ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorReport {
    /// The checks.
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    /// Whether a check failed.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == CheckStatus::Fail)
    }
}

#[cfg(test)]
mod tests {
    use super::{CheckStatus, DoctorCheck, DoctorReport, SshAgentSocket};

    #[test]
    fn statuses_serialize_as_their_names() {
        for status in [CheckStatus::Ok, CheckStatus::Warn, CheckStatus::Fail] {
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                serde_json::Value::from(status.to_string())
            );
        }
    }

    #[test]
    fn agent_socket_is_tagged_by_state() {
        assert_eq!(
            serde_json::to_value(SshAgentSocket::NotASocket {
                path: "/tmp/agent".into()
            })
            .unwrap(),
            serde_json::json!({ "state": "not_a_socket", "path": "/tmp/agent" })
        );
        assert_eq!(
            serde_json::to_value(SshAgentSocket::Unset).unwrap(),
            serde_json::json!({ "state": "unset" })
        );
    }

    #[test]
    fn a_report_fails_when_a_check_fails() {
        let check = |status| DoctorCheck {
            check: "x".to_owned(),
            status,
            detail: String::new(),
            fix: None,
        };
        let mut report = DoctorReport {
            checks: vec![check(CheckStatus::Ok), check(CheckStatus::Warn)],
        };
        assert!(!report.failed());
        report.checks.push(check(CheckStatus::Fail));
        assert!(report.failed());
    }
}
