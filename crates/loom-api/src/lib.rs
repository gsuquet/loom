//! Types shared by the loomd API, the CLI `--json` output and the MCP tools.
//!
//! Empty until a version needs it. No I/O.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(env!("CARGO_PKG_NAME"), "loom-api");
    }
}
