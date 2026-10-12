//! `loomd` library: the command-line definition, the decisions of the daemon without I/O, and
//! the tracing middleware of the API. `main.rs` and its modules do the I/O.

pub mod api;
pub mod cli;
pub mod exit;
pub mod hooks;
pub mod trace;
