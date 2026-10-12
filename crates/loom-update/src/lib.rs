//! Release discovery, update decision, download, verification, install and rollback.
//!
//! Empty until a version needs it. Shared by `loom` and `loomd`.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(env!("CARGO_PKG_NAME"), "loom-update");
    }
}
