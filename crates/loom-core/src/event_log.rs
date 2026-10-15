//! `events.jsonl`: one JSON object per line, appended and never rewritten.
//!
//! A line this version cannot replay (newer schema version, unknown event type, invalid JSON) is
//! kept as is and reported, never an error for the whole log.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::event::{EVENT_SCHEMA_VERSION, EventRecord};

/// Event types this version replays, as written in `type`.
const EVENT_TYPES: [&str; 5] = [
    "created",
    "edited",
    "status_changed",
    "note_logged",
    "undone",
];

/// The lines of an `events.jsonl` file, in order: the events to replay and the lines skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventLog {
    /// Events this version replays, in file order.
    pub events: Vec<EventRecord>,
    /// Lines kept as is and not replayed, in file order.
    pub skipped: Vec<SkippedLine>,
}

/// A line of `events.jsonl` that is not replayed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkippedLine {
    /// Line number, from 1.
    pub line: usize,
    /// The line as written, without its line end.
    pub raw: String,
    /// Why it is not replayed.
    pub reason: SkipReason,
}

/// Why a line of `events.jsonl` is not replayed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum SkipReason {
    /// Written with a newer schema version.
    #[error("schema version {v}, newer than {EVENT_SCHEMA_VERSION}")]
    NewerVersion {
        /// The `v` of the line.
        v: u64,
    },
    /// An event type this version does not know.
    #[error("unknown event type `{name}`")]
    UnknownType {
        /// The `type` of the line.
        name: String,
    },
    /// Not a valid event of this schema version.
    #[error("invalid event: {message}")]
    Invalid {
        /// What is wrong.
        message: String,
    },
}

/// Why an event cannot be written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cannot serialize event: {0}")]
pub struct EventEncodeError(String);

/// Reads an `events.jsonl` file. Blank lines are ignored; LF and CRLF line endings are accepted.
#[must_use]
pub fn parse_events(text: &str) -> EventLog {
    let mut log = EventLog::default();
    for (index, line) in text.split('\n').enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        match parse_line(line) {
            Ok(record) => log.events.push(record),
            Err(reason) => log.skipped.push(SkippedLine {
                line: index + 1,
                raw: line.to_owned(),
                reason,
            }),
        }
    }
    log
}

/// Writes `record` as one line of `events.jsonl`, line ending included.
///
/// # Errors
///
/// [`EventEncodeError`] if the record cannot be serialized to JSON.
pub fn render_event(record: &EventRecord) -> Result<String, EventEncodeError> {
    let mut line =
        serde_json::to_string(record).map_err(|error| EventEncodeError(error.to_string()))?;
    line.push('\n');
    Ok(line)
}

fn parse_line(line: &str) -> Result<EventRecord, SkipReason> {
    let invalid = |message: String| SkipReason::Invalid { message };
    let json: Json = serde_json::from_str(line).map_err(|error| invalid(error.to_string()))?;
    let Some(object) = json.as_object() else {
        return Err(invalid("not a JSON object".to_owned()));
    };
    let v = object
        .get("v")
        .and_then(Json::as_u64)
        .ok_or_else(|| invalid("missing or invalid `v`".to_owned()))?;
    if v > u64::from(EVENT_SCHEMA_VERSION) {
        return Err(SkipReason::NewerVersion { v });
    }
    let name = object
        .get("type")
        .and_then(Json::as_str)
        .ok_or_else(|| invalid("missing or invalid `type`".to_owned()))?;
    if !EVENT_TYPES.contains(&name) {
        return Err(SkipReason::UnknownType {
            name: name.to_owned(),
        });
    }
    serde_json::from_value(json).map_err(|error| invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Actor, Change, Event, FieldChange, MachineId, Origin};
    use crate::id::EventId;
    use crate::task::{Importance, Status, Task};
    use crate::timestamp::Timestamp;

    fn record(id: &str, event: Event) -> EventRecord {
        let origin = Origin {
            actor: Actor::User,
            machine_id: MachineId("laptop".to_owned()),
        };
        EventRecord::new(
            id.parse::<EventId>().unwrap(),
            event,
            Timestamp::from_unix_secs(1_791_796_440).unwrap(),
            &origin,
        )
    }

    /// One record of each event type.
    fn records() -> Vec<EventRecord> {
        let task: Task = serde_json::from_value(serde_json::json!({
            "id": "k3q7zm2a",
            "title": "Login timeout on mobile checkout",
            "kind": "bug",
            "context": "company-a",
            "status": "inbox",
            "importance": "p1",
            "created": "2026-10-12T09:14:00Z",
            "updated": "2026-10-12T09:14:00Z"
        }))
        .unwrap();
        vec![
            record(
                "e0000000",
                Event::Created {
                    task: Box::new(task),
                },
            ),
            record(
                "e0000001",
                Event::Edited {
                    changes: vec![FieldChange::Importance(Change {
                        from: Importance::P2,
                        to: Importance::P1,
                    })],
                },
            ),
            record(
                "e0000002",
                Event::StatusChanged(Change {
                    from: Status::Ready,
                    to: Status::Active,
                }),
            ),
            record(
                "e0000003",
                Event::NoteLogged {
                    text: "Called the client".to_owned(),
                },
            ),
            record(
                "e0000004",
                Event::Undone {
                    of: "e0000002".parse().unwrap(),
                    changes: vec![],
                },
            ),
        ]
    }

    fn type_of(record: &EventRecord) -> String {
        serde_json::to_value(record).unwrap()["type"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn rendered_events_read_back() {
        let records = records();
        let text: String = records
            .iter()
            .map(|record| render_event(record).unwrap())
            .collect();
        assert_eq!(text.lines().count(), records.len());
        assert!(text.starts_with(r#"{"v":1,"id":"e0000000","at":"2026-10-12T09:14:00Z""#));
        assert_eq!(
            parse_events(&text),
            EventLog {
                events: records,
                skipped: vec![]
            }
        );
    }

    #[test]
    fn every_type_written_is_replayed() {
        let mut written: Vec<String> = records().iter().map(type_of).collect();
        written.sort();
        let mut replayed = EVENT_TYPES.map(str::to_owned).to_vec();
        replayed.sort();
        assert_eq!(written, replayed);
    }

    #[test]
    fn lines_this_version_cannot_replay_are_kept_and_reported() {
        let records = records();
        let newer = r#"{"v":2,"id":"e0000009","type":"edited","changes":"later"}"#;
        let unknown = r#"{"v":1,"id":"e000000a","at":"2026-10-12T09:14:00Z","type":"agent_run"}"#;
        let truncated = r#"{"v":1,"id":"e000000b","#;
        let text = format!(
            "{}{newer}\r\n{}\n{unknown}\n{truncated}",
            render_event(&records[0]).unwrap(),
            render_event(&records[1]).unwrap().trim_end(),
        );
        let log = parse_events(&text);
        assert_eq!(log.events, records[..2]);
        let skipped: Vec<(usize, &str, SkipReason)> = log
            .skipped
            .iter()
            .map(|line| (line.line, line.raw.as_str(), line.reason.clone()))
            .collect();
        assert_eq!(skipped[0], (2, newer, SkipReason::NewerVersion { v: 2 }));
        assert_eq!(
            skipped[1],
            (
                4,
                unknown,
                SkipReason::UnknownType {
                    name: "agent_run".to_owned()
                }
            )
        );
        assert_eq!(skipped[2].0, 5);
        assert_eq!(skipped[2].1, truncated);
        assert!(matches!(skipped[2].2, SkipReason::Invalid { .. }));
    }

    #[test]
    fn invalid_known_events_are_reported_with_the_reason() {
        let cases = [
            ("[1, 2]", "invalid event: not a JSON object"),
            (
                r#"{"type":"edited"}"#,
                "invalid event: missing or invalid `v`",
            ),
            (r#"{"v":1}"#, "invalid event: missing or invalid `type`"),
        ];
        for (line, message) in cases {
            let log = parse_events(line);
            assert_eq!(log.skipped[0].reason.to_string(), message);
        }
        let log = parse_events(r#"{"v":1,"type":"note_logged"}"#);
        assert!(log.skipped[0].reason.to_string().contains("missing field"));
    }

    #[test]
    fn blank_lines_and_an_empty_log_are_empty() {
        assert_eq!(parse_events(""), EventLog::default());
        assert_eq!(parse_events("\n\r\n  \n"), EventLog::default());
    }
}
