//! Types shared by the loomd API, the CLI `--json` output and the MCP tools.
//!
//! No I/O, no clock, no async runtime. Request types use `#[serde(deny_unknown_fields)]`;
//! response types accept unknown fields so that an older client can read a newer daemon.

mod api_version;
mod auto_update;
mod context;
mod error;
mod repo;
mod service;
mod status;
mod timestamp;
mod update;
mod version;

pub use api_version::ApiVersion;
pub use auto_update::{
    PendingUpgrade, UpdateChannel, UpdatePolicyMode, UpdateResult, UpdateState, UpdateStatus,
};
pub use context::{
    CONTEXT_NAME_MAX_LEN, ContextAssignment, ContextName, ContextNameError, UNASSIGNED,
};
pub use error::{ApiError, ErrorCode};
pub use repo::{ForgeKind, Remote, RemoteUrl, RepoIdentity, RepoUpdatePolicy};
pub use service::{ServicePlatform, ServiceStatus};
pub use status::StatusResponse;
pub use timestamp::Timestamp;
pub use update::{
    DaemonRestart, InstallReport, RollbackReport, UpgradeDecision, UpgradeReport,
    UpgradeSkipReason, UpgradeSource,
};
pub use version::{Channel, Profile, TEST_BUILD_VERSION_ENV, VersionInfo};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(env!("CARGO_PKG_NAME"), "loom-api");
    }
}
