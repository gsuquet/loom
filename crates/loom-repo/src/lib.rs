//! Repositories: identity, remotes, forge, context assignment, discovery and scan changes
//! (ADR-0012).
//!
//! Every module is pure (no I/O, no async runtime) except [`scan`], which lists directories
//! and reads git files, and [`state`], which reads a repository with `gix`; neither starts git
//! (ADR-0008). Callers inject what needs a process or the user's home: SSH alias resolution
//! (`ssh -G`) and `~` expansion.

mod context;
mod diff;
mod forge;
mod identity;
mod remote;
pub mod scan;
pub mod state;
pub mod update;

pub use context::{ContextRules, ContextRulesError};
pub use diff::{ScanDiff, diff};
pub use forge::detect_forge;
pub use identity::{assign_unique_slugs, identify};
pub use remote::{RemoteUrlError, parse_remote_url, primary_remote, redact_url, remote};
pub use scan::{
    ScanOptions, ScanReport, find_root, git_dirs, read_repos, scan, scan_roots,
    scan_with_registered,
};
pub use state::{StateOptions, UpdateRefs, state, update_refs};
pub use update::{Action, FetchDecision, UpdatePolicies, decide_fetch, decide_update, plan};
