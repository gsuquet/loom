//! The Events tab of the task page: the rows of the timeline and its filters.
//!
//! Pure functions; the state keeps the loaded log and the two filters.

use jiff::tz::TimeZone;
use loom_api::{Actor, Event, EventRecord, FieldChange, TaskEvents};

use super::render::importance_label;

/// Which filter of the Events tab a key cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventsFilter {
    /// The type of the event.
    Type,
    /// The actor of the event.
    Actor,
}

/// What the Events tab holds for the open task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventsView {
    /// The log, once loaded.
    pub load: EventsLoad,
    /// Only the events of this type (`edited`, `status_changed`, ...), if set.
    pub type_filter: Option<String>,
    /// Only the events of this actor (`user`, `loomd`, ...), if set.
    pub actor_filter: Option<String>,
}

/// Whether the log of the task is loaded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum EventsLoad {
    /// Not asked yet.
    #[default]
    NotLoaded,
    /// Asked, no answer yet.
    Loading,
    /// The log.
    Loaded(Box<TaskEvents>),
    /// The answer was an error.
    Failed(String),
}

/// One line of the timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    /// `YYYY-MM-DD HH:MM`, in the time zone of the TUI.
    pub time: String,
    /// Who: `user`, `loomd`, `file`, `agent <name>`.
    pub actor: String,
    /// The type of the event.
    pub kind: &'static str,
    /// What changed, in one line.
    pub summary: String,
}

/// Name of the type of `event`, as in the log.
#[must_use]
pub const fn type_name(event: &Event) -> &'static str {
    match event {
        Event::Created { .. } => "created",
        Event::Edited { .. } => "edited",
        Event::StatusChanged(_) => "status_changed",
        Event::NoteLogged { .. } => "note_logged",
        Event::Undone { .. } => "undone",
    }
}

/// Label of an actor.
#[must_use]
pub fn actor_label(actor: &Actor) -> String {
    match actor {
        Actor::User => "user".to_owned(),
        Actor::File => "file".to_owned(),
        Actor::Daemon => "loomd".to_owned(),
        Actor::Agent { name } => format!("agent {name}"),
    }
}

fn change_text(change: &FieldChange) -> String {
    match change {
        FieldChange::Title(c) => format!("title: {} -> {}", c.from, c.to),
        FieldChange::Kind(c) => format!("kind: {} -> {}", c.from.name(), c.to.name()),
        FieldChange::Status(c) => format!(
            "status: {} -> {}",
            c.from
                .name()
                .map_or_else(|| "unknown".to_owned(), |n| n.to_string()),
            c.to.name()
                .map_or_else(|| "unknown".to_owned(), |n| n.to_string())
        ),
        FieldChange::Importance(c) => format!(
            "importance: {} -> {}",
            importance_label(c.from),
            importance_label(c.to)
        ),
        FieldChange::Due(c) => format!(
            "due: {} -> {}",
            c.from.map_or_else(|| "none".to_owned(), |d| d.to_string()),
            c.to.map_or_else(|| "none".to_owned(), |d| d.to_string())
        ),
        FieldChange::Pinned(c) => format!("pinned: {} -> {}", c.from, c.to),
        FieldChange::Links(_) => "links".to_owned(),
        FieldChange::Relations(_) => "relations".to_owned(),
    }
}

fn changes_text(changes: &[FieldChange]) -> String {
    changes
        .iter()
        .map(change_text)
        .collect::<Vec<_>>()
        .join("; ")
}

/// One line that tells what `event` did.
#[must_use]
pub fn summary(event: &Event) -> String {
    match event {
        Event::Created { task } => format!("created: {}", task.title),
        Event::Edited { changes } => changes_text(changes),
        Event::StatusChanged(change) => change_text(&FieldChange::Status(change.clone())),
        Event::NoteLogged { text } => text.lines().next().unwrap_or_default().to_owned(),
        Event::Undone { changes, .. } => format!("undone: {}", changes_text(changes)),
    }
}

/// `YYYY-MM-DD HH:MM` of `at` in `zone`. An instant that `jiff` cannot place shows as in the log
/// (UTC).
fn local_time(at: loom_api::Timestamp, zone: &TimeZone) -> String {
    i64::try_from(at.unix_secs())
        .ok()
        .and_then(|secs| jiff::Timestamp::from_second(secs).ok())
        .map_or_else(
            || at.to_string().replace('T', " ").chars().take(16).collect(),
            |instant| {
                instant
                    .to_zoned(zone.clone())
                    .strftime("%Y-%m-%d %H:%M")
                    .to_string()
            },
        )
}

fn row(record: &EventRecord, zone: &TimeZone) -> EventRow {
    EventRow {
        time: local_time(record.at, zone),
        actor: actor_label(&record.actor),
        kind: type_name(&record.event),
        summary: summary(&record.event),
    }
}

/// The rows of the timeline, newest first, that pass the filters.
#[must_use]
pub fn rows(
    log: &TaskEvents,
    zone: &TimeZone,
    type_filter: Option<&str>,
    actor_filter: Option<&str>,
) -> Vec<EventRow> {
    log.events
        .iter()
        .filter(|record| type_filter.is_none_or(|t| type_name(&record.event) == t))
        .filter(|record| actor_filter.is_none_or(|a| actor_label(&record.actor) == a))
        .map(|record| row(record, zone))
        .rev()
        .collect()
}

/// The note under the rows when the log has lines that this version does not replay.
#[must_use]
pub fn skipped_note(log: &TaskEvents) -> Option<String> {
    match log.skipped.len() {
        0 => None,
        1 => Some("1 line not shown".to_owned()),
        n => Some(format!("{n} lines not shown")),
    }
}

/// The last useful scroll offset of the timeline: the last line at the top of the pane.
#[must_use]
pub fn scroll_max(view: &EventsView) -> usize {
    let EventsLoad::Loaded(log) = &view.load else {
        return 0;
    };
    let rows = rows(
        log,
        &TimeZone::UTC,
        view.type_filter.as_deref(),
        view.actor_filter.as_deref(),
    )
    .len();
    (rows + usize::from(skipped_note(log).is_some())).saturating_sub(1)
}

/// The types of event that the log has, in order of first use.
#[must_use]
pub fn types_present(log: &TaskEvents) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for record in &log.events {
        let name = type_name(&record.event);
        if !seen.iter().any(|s| s == name) {
            seen.push(name.to_owned());
        }
    }
    seen
}

/// The actors that the log has, in order of first use.
#[must_use]
pub fn actors_present(log: &TaskEvents) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for record in &log.events {
        let name = actor_label(&record.actor);
        if !seen.contains(&name) {
            seen.push(name);
        }
    }
    seen
}

/// The next filter value when the key is pressed: no filter, then each option, then no filter
/// again. A filter that is not among the options starts over from the first option.
#[must_use]
pub fn next_filter(current: Option<&str>, options: &[String]) -> Option<String> {
    current.map_or_else(
        || options.first().cloned(),
        |value| {
            options
                .iter()
                .position(|o| o == value)
                .and_then(|at| options.get(at + 1))
                .cloned()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_api::{Change, MachineId, Timestamp};
    use loom_core::{Origin, Status};

    fn record(actor: Actor, event: Event, secs: u64) -> EventRecord {
        let origin = Origin {
            actor,
            machine_id: MachineId("desk".to_owned()),
        };
        EventRecord::new(
            "aaaaaaaa".parse().unwrap(),
            event,
            Timestamp::from_unix_secs(secs).unwrap(),
            &origin,
        )
    }

    fn log() -> TaskEvents {
        let note = Event::NoteLogged {
            text: "first line\nsecond".to_owned(),
        };
        let moved = Event::StatusChanged(Change {
            from: Status::Inbox,
            to: Status::Ready,
        });
        TaskEvents {
            id: "k3q7zm2a".parse().unwrap(),
            events: vec![
                record(Actor::User, note.clone(), 1_791_792_000),
                record(Actor::Daemon, moved, 1_791_792_060),
                record(Actor::User, note, 1_791_792_120),
            ],
            skipped: Vec::new(),
        }
    }

    #[test]
    fn rows_show_time_actor_type_and_one_line() {
        let rows = rows(&log(), &TimeZone::UTC, None, None);
        assert_eq!(rows.len(), 3);
        // Newest first.
        assert_eq!(rows[2].kind, "note_logged");
        assert_eq!(rows[2].actor, "user");
        assert_eq!(rows[2].summary, "first line");
        assert_eq!(rows[2].time.len(), 16);
        assert_eq!(rows[1].summary, "status: inbox -> ready");
        assert_eq!(rows[1].actor, "loomd");
        assert!(rows[0].time > rows[2].time);
    }

    #[test]
    fn times_are_shown_in_the_given_time_zone() {
        let paris = TimeZone::get("Europe/Paris").unwrap();
        let utc = rows(&log(), &TimeZone::UTC, None, None);
        let local = rows(&log(), &paris, None, None);
        // 1_791_792_000 is 2026-10-12 08:00 UTC; Paris is on summer time (UTC+2) until 25 October.
        assert_eq!(utc[2].time, "2026-10-12 08:00");
        assert_eq!(local[2].time, "2026-10-12 10:00");
    }

    #[test]
    fn filters_by_type_and_by_actor_together() {
        let log = log();
        assert_eq!(
            rows(&log, &TimeZone::UTC, Some("note_logged"), None).len(),
            2
        );
        assert_eq!(rows(&log, &TimeZone::UTC, None, Some("loomd")).len(), 1);
        assert_eq!(
            rows(&log, &TimeZone::UTC, Some("note_logged"), Some("loomd")).len(),
            0
        );
        assert_eq!(rows(&log, &TimeZone::UTC, Some("created"), None).len(), 0);
    }

    #[test]
    fn the_options_are_the_values_of_the_log_in_order() {
        let log = log();
        assert_eq!(types_present(&log), ["note_logged", "status_changed"]);
        assert_eq!(actors_present(&log), ["user", "loomd"]);
    }

    #[test]
    fn skipped_lines_are_counted_and_the_scroll_stops_at_the_last_line() {
        let mut log = log();
        assert_eq!(skipped_note(&log), None);
        let view = |log: &TaskEvents| EventsView {
            load: EventsLoad::Loaded(Box::new(log.clone())),
            ..EventsView::default()
        };
        assert_eq!(scroll_max(&view(&log)), 2);
        log.skipped = (1..=2)
            .map(|line| loom_api::SkippedLine {
                line,
                raw: "{}".to_owned(),
                reason: loom_core::SkipReason::UnknownType {
                    name: "x".to_owned(),
                },
            })
            .collect();
        assert_eq!(skipped_note(&log).as_deref(), Some("2 lines not shown"));
        assert_eq!(scroll_max(&view(&log)), 3);
        assert_eq!(scroll_max(&EventsView::default()), 0);
    }

    #[test]
    fn a_filter_cycles_through_the_options_and_back_to_none() {
        let options = vec!["a".to_owned(), "b".to_owned()];
        let first = next_filter(None, &options);
        assert_eq!(first.as_deref(), Some("a"));
        let second = next_filter(first.as_deref(), &options);
        assert_eq!(second.as_deref(), Some("b"));
        assert_eq!(next_filter(second.as_deref(), &options), None);
        assert_eq!(next_filter(Some("gone"), &options), None);
        assert_eq!(next_filter(None, &[]), None);
    }
}
