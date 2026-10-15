//! Recurring tasks (ADR-0013, decision 9): what a schedule of `config.toml` does today, given
//! the tasks of its context. Pure: the date and the tasks are given.
//!
//! Each task a schedule creates carries the origin link `schedule` / `<id>/<occurrence date>`,
//! the dedup key: the latest one gives the last occurrence created, so a restart, a second
//! check the same day or a week offline never create an occurrence twice. A task whose
//! `task.md` is invalid keeps its occurrence when its text still holds the quoted key.

use jiff::Timestamp;
use jiff::tz::TimeZone;
use loom_api::{ContextName, Date, ExternalRef, Link, NewTask, Status, TaskId};
use loom_config::machine::MachineName;
use loom_config::schedules::Schedule;
use loom_core::{
    PolicyDecision, SCHEDULE_PROVIDER, apply_policy, due_occurrence, occurrence_key,
    parse_occurrence_key,
};
use loom_store::tasks::{IndexedState, IndexedTask};

/// What a schedule does on a given day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No occurrence to create.
    NotDue,
    /// An occurrence is due, but the policy `skip` keeps the open tasks of earlier ones alone.
    Skipped {
        /// The occurrence.
        occurrence: Date,
        /// Its open tasks.
        open: Vec<TaskId>,
    },
    /// Create the task of an occurrence, then cancel the open tasks of earlier ones.
    Create {
        /// The occurrence.
        occurrence: Date,
        /// The task to create.
        new: Box<NewTask>,
        /// Open tasks to cancel (policy `replace`).
        cancel: Vec<TaskId>,
    },
}

/// What `schedule` of `context` does on `today`, given every indexed task (`rows`).
#[must_use]
pub fn plan(
    context: &ContextName,
    schedule: &Schedule,
    rows: &[IndexedTask],
    today: Date,
) -> Outcome {
    let id = schedule.id.get_ref().as_str();
    let tasks: Vec<(Date, &IndexedTask, bool)> = rows
        .iter()
        .filter(|row| row.context == *context)
        .filter_map(|row| match &row.state {
            IndexedState::Valid { task, .. } => {
                let occurrence = occurrence_of(&task.links, id)?;
                Some((occurrence, row, is_open(&task.status)))
            }
            // Its status is unknown: open, so `skip` waits; read-only, so never cancelled.
            IndexedState::Invalid { text, .. } => Some((occurrence_in_text(text, id)?, row, true)),
        })
        .collect();
    let last_created = tasks.iter().map(|(occurrence, ..)| *occurrence).max();
    let Some(occurrence) = due_occurrence(&schedule.recurrence, last_created, today) else {
        return Outcome::NotDue;
    };
    let open: Vec<TaskId> = tasks
        .iter()
        .filter(|(_, _, open)| *open)
        .map(|(_, row, _)| row.id.clone())
        .collect();
    match apply_policy(&open, schedule.policy) {
        PolicyDecision::Skip => Outcome::Skipped { occurrence, open },
        PolicyDecision::Create { cancel } => Outcome::Create {
            occurrence,
            new: Box::new(new_task(context, schedule, occurrence)),
            // A task of a newer loom (unknown status) is read-only: never cancelled.
            cancel: cancel
                .into_iter()
                .filter(|id| {
                    tasks
                        .iter()
                        .any(|(_, row, _)| row.id == *id && !row.read_only())
                })
                .collect(),
        },
    }
}

/// The occurrence of schedule `id` a task with `links` was created for, if any.
fn occurrence_of(links: &[Link], id: &str) -> Option<Date> {
    links.iter().find_map(|link| match link {
        Link::Origin(origin) if origin.provider == SCHEDULE_PROVIDER => {
            parse_occurrence_key(&origin.key)
                .filter(|(schedule, _)| *schedule == id)
                .map(|(_, occurrence)| occurrence)
        }
        Link::Origin(_) | Link::Related(_) | Link::Repo { .. } => None,
    })
}

/// The latest occurrence of schedule `id` whose key, quoted (`"<id>/<date>"`), is in `text`:
/// the occurrence of a task whose `task.md` a hand edit made invalid, so that it is not created
/// again.
fn occurrence_in_text(text: &str, id: &str) -> Option<Date> {
    let prefix = format!("\"{id}/");
    text.match_indices(&prefix)
        .filter_map(|(start, _)| {
            let rest = text.get(start + prefix.len()..)?;
            let (date, after) = (rest.get(..10)?, rest.get(10..)?);
            after.starts_with('"').then(|| date.parse().ok()).flatten()
        })
        .max()
}

/// Whether a task is still to do. A status of a newer loom counts as open: `skip` then waits
/// rather than piling up tasks.
const fn is_open(status: &Status) -> bool {
    match status {
        Status::Inbox
        | Status::Ready
        | Status::Active
        | Status::Waiting { .. }
        | Status::Unknown(_) => true,
        Status::Done | Status::Cancelled => false,
    }
}

/// The task of `occurrence`: `ready`, with the schedule's title, kind and importance, its due
/// date, and the origin link that is its dedup key.
fn new_task(context: &ContextName, schedule: &Schedule, occurrence: Date) -> NewTask {
    NewTask {
        title: schedule.title.clone(),
        kind: schedule.kind.clone(),
        context: context.clone(),
        status: Status::Ready,
        importance: schedule.importance,
        due: schedule.due.map(|due| due.due(occurrence)),
        pinned: false,
        links: vec![Link::Origin(ExternalRef {
            provider: SCHEDULE_PROVIDER.to_owned(),
            key: occurrence_key(schedule.id.get_ref().as_str(), occurrence),
            url: None,
        })],
        relations: loom_api::Relations::default(),
    }
}

/// Which machine runs the schedules of a context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runner {
    /// This one: the first of the context's `machines`.
    This,
    /// Another one, the first of its `machines`: this machine does not run them.
    Other(MachineName),
    /// Every machine that holds the context, `machines` being absent: two machines would both
    /// create each occurrence.
    Unpinned,
}

/// Which machine runs the schedules of a context with `machines`, as seen from machine `this`.
///
/// The first of `machines`: two machines would both create each occurrence, then sync both.
#[must_use]
pub fn runner(machines: Option<&[MachineName]>, this: &MachineName) -> Runner {
    match machines.and_then(<[MachineName]>::first) {
        None => Runner::Unpinned,
        Some(first) if first == this => Runner::This,
        Some(first) => Runner::Other(first.clone()),
    }
}

/// The date at `now` in `tz`: the day schedules are checked for.
#[must_use]
pub fn local_date(now: Timestamp, tz: &TimeZone) -> Date {
    now.to_zoned(tz.clone()).date()
}

#[cfg(test)]
mod tests;
