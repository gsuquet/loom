//! Hand edits: what loomd does with a `task.md` it did not write, decided without I/O.
//!
//! loomd keeps the hash of each `task.md` it reads or writes in the index. A file whose hash
//! differs was changed by someone else; the changes are what differs between the task its log
//! describes and the task its file holds.

use loom_core::{FieldChange, hand_edit_changes, replay};

use crate::port::{ContentHash, StoredTask};

/// What to do with a task file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciliation {
    /// The file is the one loomd last indexed: nothing to do.
    Unchanged,
    /// Index the file as it is, without an event: it is invalid, its fields are those its log
    /// describes (only the notes changed), or its log cannot be replayed.
    Index,
    /// Record these changes as an `edited` event of the actor `file`, then index the file.
    Record(Vec<FieldChange>),
}

/// What to do with `stored`, whose `task.md` had the hash `indexed` when loomd last indexed it
/// (`None` when it is not indexed).
#[must_use]
pub fn decide(indexed: Option<&ContentHash>, stored: &StoredTask) -> Reconciliation {
    if indexed == Some(&stored.file.hash) {
        return Reconciliation::Unchanged;
    }
    let Ok(file) = stored.task() else {
        return Reconciliation::Index;
    };
    // A line this version cannot replay may hold a change: the log is not the whole story.
    if !stored.events.skipped.is_empty() {
        return Reconciliation::Index;
    }
    let Ok(logged) = replay(&stored.events.events) else {
        return Reconciliation::Index;
    };
    let changes = hand_edit_changes(&logged, &file);
    if changes.is_empty() {
        Reconciliation::Index
    } else {
        Reconciliation::Record(changes)
    }
}

#[cfg(test)]
mod tests;
