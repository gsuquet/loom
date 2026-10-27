//! Hand edits: what loomd does with a `task.md` it did not write, decided without I/O.
//!
//! loomd keeps the hash of each `task.md` it reads or writes in the index. A file whose hash
//! differs was changed by someone else; the changes are what differs between the task its log
//! describes and the task its file holds.

use std::collections::BTreeMap;

use loom_core::{
    Change, ContextName, Event, FieldChange, NotesHash, SkipReason, TaskId, hand_edit_changes,
    replay,
};

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
    /// Record this hand edit as events of the actor `file`, then index the file.
    Record(HandEdit),
}

/// What a hand edit changed: fields, notes, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandEdit {
    /// The fields that differ from the log, as one `edited` event; empty when only the notes
    /// changed.
    pub changes: Vec<FieldChange>,
    /// The notes before and after, as a `notes_changed` event; `None` when they did not change
    /// or when the notes before are unknown.
    pub notes: Option<Change<NotesHash>>,
}

impl HandEdit {
    /// The events to append, in order: the field changes, then the notes.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        let fields = (!self.changes.is_empty()).then(|| Event::Edited {
            changes: self.changes.clone(),
        });
        let notes = self.notes.clone().map(Event::NotesChanged);
        fields.into_iter().chain(notes).collect()
    }
}

/// What to do with `stored`, whose `task.md` had the hash and location `indexed` when loomd
/// last indexed it (`None` when it is not indexed), and whose notes had the hash `notes`.
///
/// The notes before a hand edit are the ones loomd indexed. Without an index row, they are the
/// ones the log recorded last; if it recorded none, they are unknown and a change of the notes
/// is not recorded, rather than guessed.
#[must_use]
pub fn decide(
    indexed: Option<(&ContentHash, TaskLocation)>,
    notes: Option<&ContentHash>,
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
    let notes = notes_change(notes, stored);
    if changes.is_empty() && notes.is_none() {
        Reconciliation::Index
    } else {
        Reconciliation::Record(HandEdit { changes, notes })
    }
}

/// The change of the notes of `stored` since `indexed` (the hash loomd indexed), or since the
/// log recorded them last; `None` when they did not change or their previous value is unknown.
fn notes_change(indexed: Option<&ContentHash>, stored: &StoredTask) -> Option<Change<NotesHash>> {
    let document = stored.file.document.as_ref().ok()?;
    let to = hash_of(&ContentHash::of(document.body().as_bytes()))?;
    let from = indexed.and_then(hash_of).or_else(|| {
        stored
            .events
            .events
            .iter()
            .rev()
            .find_map(|record| match &record.event {
                Event::NotesChanged(change) => Some(change.to.clone()),
                Event::Created { .. }
                | Event::Edited { .. }
                | Event::StatusChanged(_)
                | Event::NoteLogged { .. }
                | Event::Undone { .. } => None,
            })
    })?;
    (from != to).then_some(Change { from, to })
}

/// The hash as the ledger records it; `None` for an index row that holds something else than a
/// SHA-256 in hex.
fn hash_of(hash: &ContentHash) -> Option<NotesHash> {
    NotesHash::from_hex(hash.as_str().to_owned()).ok()
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
