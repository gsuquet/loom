//! The archive (ADR-0013, decision 10): a done or cancelled task leaves the active store a set
//! time after it was closed, for `archive/<yyyy>/`, `<yyyy>` the year it was closed.

use std::time::Duration;

use crate::event::{Event, EventRecord, FieldChange};
use crate::task::{Status, Task};
use crate::timestamp::Timestamp;

/// When `task` was closed: the time of the last event of `events` (its log) that set its
/// current status, `done` or `cancelled`.
///
/// `None` when it is not closed, or when no event closed it (a lost log, a hand edit not
/// recorded yet): a closing the log does not show is never guessed.
#[must_use]
pub fn closed_at(task: &Task, events: &[EventRecord]) -> Option<Timestamp> {
    match task.status {
        Status::Done | Status::Cancelled => {}
        Status::Inbox
        | Status::Ready
        | Status::Active
        | Status::Waiting { .. }
        | Status::Unknown(_) => return None,
    }
    let closing = events.iter().rev().find(|record| match &record.event {
        Event::Created { task: created } => created.status == task.status,
        Event::StatusChanged(change) => change.to == task.status,
        Event::Edited { changes } | Event::Undone { changes, .. } => {
            changes.iter().any(|change| match change {
                FieldChange::Status(change) => change.to == task.status,
                FieldChange::Title(_)
                | FieldChange::Kind(_)
                | FieldChange::Importance(_)
                | FieldChange::Due(_)
                | FieldChange::Pinned(_)
                | FieldChange::Links(_)
                | FieldChange::Relations(_)
                | FieldChange::Context(_) => false,
            })
        }
        Event::NoteLogged { .. } => false,
    });
    closing.map(|record| record.at)
}

/// The year of the archive directory `task` (with its log `events`) moves to at `now`, when it
/// was closed at least `after` ago (see [`closed_at`]); `None` when it stays.
#[must_use]
pub fn archive_year(
    task: &Task,
    events: &[EventRecord],
    now: Timestamp,
    after: Duration,
) -> Option<u16> {
    let closed = closed_at(task, events)?;
    let due = closed.unix_secs().saturating_add(after.as_secs());
    (now.unix_secs() >= due).then(|| closed.year())
}

#[cfg(test)]
mod tests;
