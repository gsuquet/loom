//! `loom` library: the command-line definition and the conventions every
//! command follows. `main.rs` does the I/O.

pub mod backup;
pub mod cli;
pub mod cli_doc;
pub mod conventions;
pub mod doctor;
pub mod loomd_path;
pub mod reindex;
pub mod repo;
pub mod search;
pub mod service;
pub mod shell;
pub mod sse;
pub mod status;
pub mod task;
