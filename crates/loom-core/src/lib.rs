//! The task domain: tasks, their status transitions, commands and events, and the primitives
//! they are built on (instants, calendar dates, context names, IDs), and the file formats they
//! are stored in (`task.md`, `events.jsonl`), as strings in and strings out.
//!
//! No I/O, no clock, no randomness of its own, no async runtime: callers inject the current time
//! and a [`RandomSource`], and persist the events [`apply`] returns.

mod archive;
mod command;
mod context;
mod event;
mod event_log;
mod id;
mod priority;
mod recurrence;
mod relations;
mod task;
mod task_file;
mod timestamp;

pub use archive::{archive_year, closed_at};
pub use command::{
    Action, Command, DomainError, NewTask, ReplayError, TaskEdit, apply, available_actions, create,
    hand_edit_changes, replay, undo,
};
pub use context::{CONTEXT_NAME_MAX_LEN, ContextName, ContextNameError, UNASSIGNED};
pub use event::{
    Actor, Change, EVENT_SCHEMA_VERSION, Event, EventRecord, FieldChange, MachineId, Origin,
};
pub use event_log::{
    EventEncodeError, EventLog, SkipReason, SkippedLine, parse_events, render_event,
};
pub use id::{
    EventId, ID_LEN, IdError, PrefixError, RandomSource, TaskId, resolve_prefix,
    shortest_unique_prefixes,
};
/// A calendar date, written `YYYY-MM-DD` (due dates, waits).
pub use jiff::civil::Date;
pub use priority::{RankReason, Ranked, rank};
pub use recurrence::{
    Frequency, MissingStart, Policy, PolicyDecision, Recurrence, Rule, RuleError,
    SCHEDULE_PROVIDER, apply_policy, due_occurrence, occurrence_key, parse_occurrence_key,
};
pub use relations::{ParentLink, ParentProblem, check_parent};
pub use task::{
    ContentFormat, ExternalRef, Importance, Kind, Link, ReadOnlyReason, Relations, Status,
    StatusName, Task, is_legal_transition,
};
pub use task_file::{FormatError, TaskDocument, body_template, parse_task_md, render_task_md};
pub use timestamp::Timestamp;
