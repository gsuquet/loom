//! Repositories: identity, remotes, forge, context assignment, discovery and scan changes
//! (ADR-0012).
//!
//! Every module is pure (no I/O, no async runtime) except [`scan`], which lists directories
//! and reads git files, never starting git (ADR-0008). Callers inject what needs a process or
//! the user's home: SSH alias resolution (`ssh -G`) and `~` expansion.

mod context;
mod diff;
mod forge;
mod identity;
mod remote;
pub mod scan;

pub use context::{ContextRules, ContextRulesError};
pub use diff::{ScanDiff, diff};
pub use forge::detect_forge;
pub use identity::{assign_unique_slugs, identify};
pub use remote::{RemoteUrlError, parse_remote_url, primary_remote, redact_url, remote};
pub use scan::{ScanOptions, ScanReport, scan};
