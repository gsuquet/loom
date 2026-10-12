//! `loomd` library: the command-line definition and the decisions of the daemon, without I/O.
//! `main.rs` and its modules do the I/O.

pub mod api;
pub mod cli;
pub mod exit;
pub mod logging;
