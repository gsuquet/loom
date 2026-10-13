//! The task domain: tasks, their status transitions, commands and events, and the primitives
//! they are built on (instants, calendar dates, context names, IDs).
//!
//! No I/O, no clock, no randomness of its own, no async runtime: callers inject the current time
//! and a [`RandomSource`], and persist the events [`apply`] returns.

mod command;
mod context;
mod event;
mod id;
mod task;
mod timestamp;

pub use command::{
    Action, Command, DomainError, NewTask, TaskEdit, apply, available_actions, create, undo,
};
pub use context::{CONTEXT_NAME_MAX_LEN, ContextName, ContextNameError, UNASSIGNED};
pub use event::{
    Actor, Change, EVENT_SCHEMA_VERSION, Event, EventRecord, FieldChange, MachineId, Origin,
};
pub use id::{
    EventId, ID_LEN, IdError, PrefixError, RandomSource, TaskId, resolve_prefix,
    shortest_unique_prefixes,
};
/// A calendar date, written `YYYY-MM-DD` (due dates, waits).
pub use jiff::civil::Date;
pub use task::{
    ContentFormat, ExternalRef, Importance, Kind, Link, ReadOnlyReason, Relations, Status,
    StatusName, Task, is_legal_transition,
};
pub use timestamp::Timestamp;
