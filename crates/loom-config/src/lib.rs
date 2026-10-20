//! Loom home directory, the files under it and `config.toml`, shared by `loom` and `loomd`.
//!
//! No I/O: callers read the environment and the files, this crate decides.

pub mod config;
pub mod duration;
pub mod home;
pub mod layout;
pub mod machine;
pub mod repos;
pub mod schedules;

pub use repos::{ContextColor, ContextColorError, ContextConfig};
