//! The task domain and the primitives it is built on (instants, context names, IDs).
//!
//! No I/O, no clock, no randomness of its own, no async runtime: callers inject the current time
//! and a [`RandomSource`].

mod context;
mod id;
mod timestamp;

pub use context::{CONTEXT_NAME_MAX_LEN, ContextName, ContextNameError, UNASSIGNED};
pub use id::{
    EventId, ID_LEN, IdError, PrefixError, RandomSource, TaskId, resolve_prefix,
    shortest_unique_prefixes,
};
pub use timestamp::Timestamp;
