//! Diagnostics: the environment of `loomd` (`GET /v1/diagnostics`) and the report of
//! `loom repo doctor`.

use std::fmt;
use std::path::PathBuf;

use loom_core::ContextName;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ForgeKind;

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
    /// `HOME` of `loomd` (expands `~` in the configuration). `null` if unset or empty.
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
    /// Something to look at. loom still works.
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

/// Transport of a remote, as `POST /v1/diagnostics/remote-check` groups remotes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RemoteScheme {
    /// `ssh://`, `git+ssh://`, `ssh+git://` or scp-like `[user@]host:path`.
    Ssh,
    /// `https://`.
    Https,
    /// `http://`.
    Http,
    /// `git://`.
    Git,
    /// A local path or a `file://` URL.
    Local,
    /// Any other transport (`ext::`, a remote helper…).
    Other,
}

impl RemoteScheme {
    /// Name, as serialized and printed.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ssh => "ssh",
            Self::Https => "https",
            Self::Http => "http",
            Self::Git => "git",
            Self::Local => "local",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for RemoteScheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why git could not reach a remote, read from its error output.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RemoteProblem {
    /// SSH refused the key, or the host key is unknown.
    SshAuth,
    /// HTTP(S) authentication failed, or git has no credential and cannot prompt.
    HttpsAuth,
    /// The remote answered that the repository does not exist (moved, deleted, access lost).
    NotFound,
    /// The host could not be reached: name resolution, connection, timeout.
    Network,
    /// Anything else.
    Other,
}

impl RemoteProblem {
    /// Name, as serialized.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SshAuth => "ssh_auth",
            Self::HttpsAuth => "https_auth",
            Self::NotFound => "not_found",
            Self::Network => "network",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for RemoteProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Result of `git ls-remote` against one remote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemoteCheckOutcome {
    /// The remote answered.
    Reachable,
    /// git failed, timed out or could not start.
    Failed {
        /// What kind of failure, from git's error output.
        problem: RemoteProblem,
        /// What went wrong: exit status, timeout, or why git could not start.
        error: String,
        /// The end of git's standard error, at most 2,000 bytes.
        stderr_tail: String,
    },
}

/// One remote endpoint checked by `POST /v1/diagnostics/remote-check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RemoteCheck {
    /// The endpoint: `user@host[:port]` for SSH (an SSH alias as written), `scheme://host[:port]`
    /// for HTTP(S) and `git://`, the URL of a local or other remote.
    pub endpoint: String,
    /// Its transport.
    pub scheme: RemoteScheme,
    /// Slug of the repository the check ran in.
    pub via: String,
    /// Its path.
    pub path: PathBuf,
    /// Name of that repository's primary remote.
    pub remote: String,
    /// What happened.
    pub result: RemoteCheckOutcome,
}

/// Answer of `POST /v1/diagnostics/remote-check`: one check per distinct endpoint and scheme of
/// the primary remotes of the index, sorted by scheme, then endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RemoteCheckResult {
    /// The checks.
    pub remotes: Vec<RemoteCheck>,
}

/// The table that gives the credential of a context for a forge host.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CredentialOrigin {
    /// `[contexts.<ctx>.forges."<host>"]`.
    Context,
    /// `[forges."<host>"]`.
    Host,
}

/// Where the token comes from. The command text is not given: it can contain a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TokenSourceKind {
    /// `token_cmd`.
    Command,
    /// `token_env`.
    Env {
        /// Name of the environment variable.
        name: String,
    },
}

/// Why `loomd` cannot get the token.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TokenProblem {
    /// The shell cannot find the program of `token_cmd` (exit status 127).
    CommandNotFound,
    /// `token_cmd` exited with a status that is not zero, or cannot start.
    CommandFailed,
    /// `token_cmd` did not stop in time. Its process group was stopped.
    Timeout,
    /// `token_cmd` wrote no token, or the variable of `token_env` is empty.
    Empty,
    /// The variable of `token_env` is not set in the environment of `loomd`.
    EnvUnset,
}

/// What the token can do on the forge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ForgeAccess {
    /// The token can publish reviews.
    Write {
        /// The scopes of the token.
        scopes: Vec<String>,
    },
    /// The token can read, but cannot publish reviews.
    ReadOnly {
        /// The scopes of the token.
        scopes: Vec<String>,
    },
    /// The forge does not give the scopes of this token.
    Unknown {
        /// Why the scopes are unknown.
        reason: String,
    },
}

/// Result of the check of one credential. Never contains the token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ForgeCheckOutcome {
    /// `loomd` cannot get the token.
    TokenUnresolved {
        /// What kind of problem.
        problem: TokenProblem,
        /// What went wrong.
        error: String,
        /// The program of `token_cmd` (its first word), for `command_not_found`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        program: Option<String>,
    },
    /// The forge refused the token (`GET /user`).
    Rejected {
        /// HTTP status.
        status: u16,
        /// What the forge said.
        error: String,
    },
    /// The organization uses SAML single sign-on, and the token is not authorized for it.
    SsoRequired {
        /// The URL that authorizes the token, from the `x-github-sso` header.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    /// The forge did not answer, or its answer cannot be read.
    Unreachable {
        /// What went wrong.
        error: String,
    },
    /// The forge accepted the token.
    Authenticated {
        /// The user of the token.
        login: String,
        /// What the token can do.
        access: ForgeAccess,
    },
}

/// The check of the credential of one context for one forge host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ForgeCheck {
    /// The context.
    pub context: ContextName,
    /// The forge host, such as `github.com`.
    pub host: String,
    /// The forge of the host.
    pub kind: ForgeKind,
    /// The API that `loomd` called, such as `https://api.github.com`.
    pub api: String,
    /// The table that gives the credential.
    pub origin: CredentialOrigin,
    /// Where the token comes from.
    pub source: TokenSourceKind,
    /// What happened.
    pub result: ForgeCheckOutcome,
}

/// Answer of `POST /v1/diagnostics/forge-check`: one check per context and forge host that
/// has a credential, sorted by context, then host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ForgeCheckResult {
    /// The checks.
    pub forges: Vec<ForgeCheck>,
}

/// One check of `loom repo doctor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorCheck {
    /// What was checked, such as `roots`, `contexts`, `remote git@github.com (ssh, via
    /// personal/loom)` or `updates`.
    pub check: String,
    /// Outcome.
    pub status: CheckStatus,
    /// What was found, in English.
    pub detail: String,
    /// How to fix it, in English. Absent if there is nothing to fix.
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
