//! Repositories: identity, remotes, forge and context assignment (ADR-0012).
//!
//! No I/O, no async runtime. Callers read the repositories and the environment and inject
//! what needs a process or the user's home: SSH alias resolution (`ssh -G`) and `~` expansion.

mod context;
mod forge;
mod identity;
mod remote;

pub use context::{ContextRules, ContextRulesError};
pub use forge::detect_forge;
pub use identity::{assign_unique_slugs, identify};
pub use remote::{RemoteUrlError, parse_remote_url, primary_remote, redact_url, remote};
