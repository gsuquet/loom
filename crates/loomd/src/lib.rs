//! `loomd` library: the command-line definition, the decisions of the daemon without I/O, and
//! the tracing middleware of the API. `main.rs` and its modules do the I/O, except
//! [`repo_update`], which runs git to update one repository and is here for its integration
//! tests.

pub mod api;
pub mod cli;
pub mod exit;
pub mod hooks;
pub mod repo_update;
pub mod schedule;
pub mod trace;
