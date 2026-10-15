//! `loomd` library: the command-line definition, the decisions of the daemon without I/O, and
//! the tracing middleware of the API. `main.rs` and its modules do the I/O, except
//! [`repo_update`] and [`store_git`], which run git to update one repository and to commit the
//! context stores, and are here for their integration
//! tests.

pub mod api;
pub mod backup;
pub mod cli;
pub mod disk;
pub mod exit;
pub mod hooks;
pub mod repo_update;
pub mod schedule;
pub mod store_git;
pub mod task_api;
pub mod task_archive;
pub mod task_index;
pub mod trace;
