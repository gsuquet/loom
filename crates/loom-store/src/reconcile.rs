//! Hand edits: what loomd does with a `task.md` it did not write, decided without I/O.
//!
//! loomd keeps the hash of each `task.md` it reads or writes in the index. A file whose hash
//! differs was changed by someone else; the changes are what differs between the task its log
//! describes and the task its file holds.

use std::collections::BTreeMap;

use loom_core::{ContextName, FieldChange, SkipReason, TaskId, hand_edit_changes, replay};

use crate::port::{ContentHash, StoredTask, TaskLocation};

/// What to do with a task file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciliation {
    /// The file is the one loomd last indexed, where it was: nothing to do.
    Unchanged,
    /// Index the file as it is, without an event: it moved to or from the archive, it is
    /// invalid, its fields are those its log describes (only the notes changed), or its log
    /// has no creation event to compare with.
    Index,
    /// Index the file as it is, without an event, because the log has a line written by a newer
    /// loom: that line may hold a change this version cannot read, so a difference between the
    /// log and the file is not known to be a hand edit.
    Withhold,
    /// Record these changes as an `edited` event of the actor `file`, then index the file.
    Record(Vec<FieldChange>),
}

/// What to do with `stored`, whose `task.md` had the hash and location `indexed` when loomd
/// last indexed it (`None` when it is not indexed).
#[must_use]
pub fn decide(
    indexed: Option<(&ContentHash, TaskLocation)>,
    stored: &StoredTask,
) -> Reconciliation {
    if let Some((hash, location)) = indexed
        && *hash == stored.file.hash
    {
        return if location == stored.location {
            Reconciliation::Unchanged
        } else {
            Reconciliation::Index
        };
    }
    let Ok(file) = stored.task() else {
        return Reconciliation::Index;
    };
    // A line written by a newer loom may hold a change this version cannot read: the log is not
    // the whole story, and what the file differs by may be that change. A damaged line or an
    // unknown type is no reason to stay silent: the events that were read are replayed.
    if stored
        .events
        .skipped
        .iter()
        .any(|line| matches!(line.reason, SkipReason::NewerVersion { .. }))
    {
        return Reconciliation::Withhold;
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

/// The task IDs held by more than one context store, each with its contexts in the order of
/// `held` (context and ID of every task directory found), without repeats.
///
/// IDs are drawn free in every context, so a duplicate comes from a hand copy of a task
/// directory. Files are the source of truth, but the index keys a task on its ID alone: such an
/// ID cannot be indexed as either copy.
#[must_use]
pub fn duplicate_ids<'a>(
    held: impl IntoIterator<Item = (&'a ContextName, &'a TaskId)>,
) -> BTreeMap<TaskId, Vec<ContextName>> {
    let mut contexts: BTreeMap<TaskId, Vec<ContextName>> = BTreeMap::new();
    for (context, id) in held {
        let held_by = contexts.entry(id.clone()).or_default();
        if !held_by.contains(context) {
            held_by.push(context.clone());
        }
    }
    contexts.retain(|_, held_by| held_by.len() > 1);
    contexts
}

#[cfg(test)]
mod tests;
