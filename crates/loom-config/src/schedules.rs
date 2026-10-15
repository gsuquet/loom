//! `[[contexts.<name>.schedules]]`: recurring tasks (ADR-0013, decision 9), checked per value.
//! Two schedules of one context with the same `id` are refused in [`crate::config`].

use std::fmt;

use loom_core::{Date, Importance, Kind, Policy, Recurrence, Rule};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use toml::Spanned;

/// Longest schedule ID.
pub const SCHEDULE_ID_MAX_LEN: usize = 64;

/// A schedule: one task per occurrence of its rule, in its context.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawSchedule")]
pub struct Schedule {
    /// `id`: names the schedule in the dedup key of its tasks.
    pub id: Spanned<ScheduleId>,
    /// `title`: title of each task.
    pub title: String,
    /// `kind`: kind of each task.
    pub kind: Kind,
    /// `importance`: importance of each task; the default of the kind when `None`.
    pub importance: Option<Importance>,
    /// `rrule` and `start`.
    pub recurrence: Recurrence,
    /// `policy`: what happens to the open task of an earlier occurrence.
    pub policy: Policy,
    /// `due`: the due date of each task.
    pub due: Option<DueRule>,
}

/// `due` of a schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum DueRule {
    /// Due on the date of its occurrence.
    #[serde(rename = "same-day")]
    SameDay,
}

impl DueRule {
    /// The due date of the task of `occurrence`.
    #[must_use]
    pub const fn due(self, occurrence: Date) -> Date {
        match self {
            Self::SameDay => occurrence,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSchedule {
    id: Spanned<ScheduleId>,
    title: Title,
    kind: KnownKind,
    #[serde(default)]
    importance: Option<Importance>,
    rrule: RuleValue,
    #[serde(default, deserialize_with = "start")]
    start: Option<Date>,
    #[serde(default)]
    policy: Policy,
    #[serde(default)]
    due: Option<DueRule>,
}

impl TryFrom<RawSchedule> for Schedule {
    type Error = String;

    fn try_from(raw: RawSchedule) -> Result<Self, Self::Error> {
        let recurrence = Recurrence::new(raw.rrule.0, raw.start)
            .map_err(|error| format!("schedule `{}`: {error}", raw.id.get_ref()))?;
        Ok(Self {
            id: raw.id,
            title: raw.title.0,
            kind: raw.kind.0,
            importance: raw.importance,
            recurrence,
            policy: raw.policy,
            due: raw.due,
        })
    }
}

/// ID of a schedule, unique in its context: 1 to 64 lowercase ASCII letters, digits and `-`,
/// starting with a letter or a digit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScheduleId(String);

impl ScheduleId {
    /// The ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ScheduleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ScheduleId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let id = String::deserialize(deserializer)?;
        let valid = !id.is_empty()
            && id.len() <= SCHEDULE_ID_MAX_LEN
            && !id.starts_with('-')
            && id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if !valid {
            return Err(D::Error::custom(format!(
                "invalid schedule id `{id}`: 1 to {SCHEDULE_ID_MAX_LEN} lowercase ASCII letters, digits and `-`, not starting with `-`"
            )));
        }
        Ok(Self(id))
    }
}

struct Title(String);

impl<'de> Deserialize<'de> for Title {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let title = String::deserialize(deserializer)?;
        if title.trim().is_empty() || title.contains(['\n', '\r']) {
            return Err(D::Error::custom(
                "a schedule title is one line of text, not empty",
            ));
        }
        Ok(Self(title))
    }
}

struct KnownKind(Kind);

impl<'de> Deserialize<'de> for KnownKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Kind::known(&name).map(Self).ok_or_else(|| {
            D::Error::custom(format!(
                "unknown kind `{name}`: story, bug, chore, research, investigation, review, support, content, admin, interview or learning"
            ))
        })
    }
}

struct RuleValue(Rule);

impl<'de> Deserialize<'de> for RuleValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse()
            .map(Self)
            .map_err(|error| D::Error::custom(format!("rrule `{text}`: {error}")))
    }
}

/// `start`: a TOML local date (`2026-10-02`) or a string of one (`"2026-10-02"`).
fn start<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Date>, D::Error> {
    let invalid = |shown: &str| {
        D::Error::custom(format!(
            "start {shown}: a date is expected, such as 2026-10-02"
        ))
    };
    let text = match toml::Value::deserialize(deserializer)? {
        toml::Value::String(text) => text,
        toml::Value::Datetime(datetime) if datetime.time.is_none() && datetime.offset.is_none() => {
            datetime.to_string()
        }
        toml::Value::Datetime(datetime) => return Err(invalid(&format!("`{datetime}`"))),
        other => return Err(invalid(other.type_str())),
    };
    text.parse()
        .map(Some)
        .map_err(|_| invalid(&format!("`{text}`")))
}
