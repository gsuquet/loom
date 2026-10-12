//! Types shared by the loomd API, the CLI `--json` output and the MCP tools.
//!
//! No I/O, no clock, no async runtime. Request types use `#[serde(deny_unknown_fields)]`;
//! response types accept unknown fields so that an older client can read a newer daemon.

mod api_version;
mod error;
mod service;
mod status;
mod timestamp;
mod version;

pub use api_version::ApiVersion;
pub use error::{ApiError, ErrorCode};
pub use service::{ServicePlatform, ServiceStatus};
pub use status::StatusResponse;
pub use timestamp::Timestamp;
pub use version::{Channel, Profile, VersionInfo};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(env!("CARGO_PKG_NAME"), "loom-api");
    }
}
