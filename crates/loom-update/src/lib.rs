//! Release discovery, update decision, download, verification, install and rollback.
//!
//! Shared by `loom` and `loomd`. For now it holds the pure update decision ([`decide`]).

mod decision;

pub use decision::{Channel, SkipReason, UpdateDecision, UpdateMode, UpdatePolicy, decide};
