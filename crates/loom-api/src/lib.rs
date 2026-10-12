//! Types shared by the loomd API, the CLI `--json` output and the MCP tools.
//!
//! No I/O.

mod version;

pub use version::{Channel, Profile, VersionInfo};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(env!("CARGO_PKG_NAME"), "loom-api");
    }
}
