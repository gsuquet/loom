//! Schedules: recurrence rules on calendar dates, which occurrence is due, and what happens to
//! the task of the previous occurrence.
//!
//! A rule is a subset of the RFC 5545 RRULE: `FREQ=DAILY|WEEKLY|MONTHLY`, `INTERVAL`, `BYDAY`,
//! `BYMONTHDAY` and `BYSETPOS`. Occurrences are dates, never times: the caller gives today's
//! date in its time zone, so a change of UTC offset never moves or repeats an occurrence. Weeks
//! start on Monday.

use std::fmt;
use std::str::FromStr;

use jiff::ToSpan as _;
use jiff::civil::Weekday;
use serde::Deserialize;

use crate::Date;
use crate::id::TaskId;

/// How often a rule repeats: `FREQ`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frequency {
    /// Every day.
    Daily,
    /// Every week, Monday to Sunday.
    Weekly,
    /// Every month.
    Monthly,
}

impl Frequency {
    const fn name(self) -> &'static str {
        match self {
            Self::Daily => "DAILY",
            Self::Weekly => "WEEKLY",
            Self::Monthly => "MONTHLY",
        }
    }
}

/// A `BYDAY` entry: a weekday, with its rank in the month (`1MO`, `-1FR`) for a monthly rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ByDay {
    /// 1 to 5 from the start of the month, -1 to -5 from its end; `None` for every such day.
    nth: Option<i8>,
    weekday: Weekday,
}

const WEEKDAYS: [(&str, Weekday); 7] = [
    ("MO", Weekday::Monday),
    ("TU", Weekday::Tuesday),
    ("WE", Weekday::Wednesday),
    ("TH", Weekday::Thursday),
    ("FR", Weekday::Friday),
    ("SA", Weekday::Saturday),
    ("SU", Weekday::Sunday),
];

impl ByDay {
    fn parse(text: &str) -> Option<Self> {
        let split = text.len().checked_sub(2)?;
        let (nth, day) = (text.get(..split)?, text.get(split..)?);
        let weekday = WEEKDAYS
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(day))
            .map(|(_, weekday)| *weekday)?;
        let nth = if nth.is_empty() {
            None
        } else {
            let nth: i8 = nth.parse().ok()?;
            (nth != 0 && (-5..=5).contains(&nth)).then_some(nth)?;
            Some(nth)
        };
        Some(Self { nth, weekday })
    }
}

impl fmt::Display for ByDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(nth) = self.nth {
            write!(f, "{nth}")?;
        }
        let name = WEEKDAYS
            .iter()
            .find(|(_, weekday)| *weekday == self.weekday)
            .map_or("", |(name, _)| *name);
        f.write_str(name)
    }
}

/// A recurrence rule, the supported subset of an RFC 5545 RRULE. Parsed from its text
/// (`FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1`), names and values in any case; displayed
/// in canonical form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    freq: Frequency,
    interval: u32,
    by_day: Vec<ByDay>,
    by_month_day: Vec<i8>,
    by_set_pos: Vec<i16>,
}

/// Why a rule is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuleError {
    /// No `FREQ` part.
    #[error("FREQ is missing: FREQ=DAILY, FREQ=WEEKLY or FREQ=MONTHLY")]
    MissingFrequency,
    /// Two `;` in a row, or a `;` at an end.
    #[error("empty part: parts are separated by one `;`")]
    EmptyPart,
    /// A part without `=`.
    #[error("`{0}` is not `NAME=VALUE`")]
    Malformed(String),
    /// A part given twice.
    #[error("{0} is given twice")]
    Duplicate(String),
    /// A part or value this version does not support.
    #[error(
        "`{0}` is not supported: use FREQ=DAILY|WEEKLY|MONTHLY, INTERVAL, BYDAY, BYMONTHDAY and BYSETPOS"
    )]
    Unsupported(String),
    /// A part this version supports with other frequencies only.
    #[error("`{part}` is not supported with FREQ={freq}")]
    UnsupportedWith {
        /// The part, with its value when the value is the problem.
        part: String,
        /// The frequency of the rule.
        freq: &'static str,
    },
    /// A value out of range or of the wrong form.
    #[error("{part} `{value}` is invalid: {expected}")]
    Invalid {
        /// The part.
        part: &'static str,
        /// The value, as written.
        value: String,
        /// What is expected.
        expected: &'static str,
    },
    /// `BYSETPOS` alone.
    #[error("BYSETPOS needs BYDAY or BYMONTHDAY: it picks among the days they give")]
    SetPositionAlone,
}

impl FromStr for Rule {
    type Err = RuleError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut freq = None;
        let mut interval = None;
        let mut by_day = None;
        let mut by_month_day = None;
        let mut by_set_pos = None;
        for part in text.split(';') {
            if part.is_empty() {
                if text.is_empty() {
                    break;
                }
                return Err(RuleError::EmptyPart);
            }
            let Some((name, value)) = part.split_once('=') else {
                return Err(RuleError::Malformed(part.to_owned()));
            };
            let name = name.to_ascii_uppercase();
            // Values are matched in upper case and quoted as written.
            match name.as_str() {
                "FREQ" => set(&mut freq, "FREQ", parse_frequency(&value)?)?,
                "INTERVAL" => set(&mut interval, "INTERVAL", parse_interval(&value)?)?,
                "BYDAY" => set(&mut by_day, "BYDAY", parse_list(&value, by_day_entry)?)?,
                "BYMONTHDAY" => set(
                    &mut by_month_day,
                    "BYMONTHDAY",
                    parse_list(&value, month_day)?,
                )?,
                "BYSETPOS" => set(&mut by_set_pos, "BYSETPOS", parse_list(&value, set_pos)?)?,
                _ => return Err(RuleError::Unsupported(name)),
            }
        }
        let rule = Self {
            freq: freq.ok_or(RuleError::MissingFrequency)?,
            interval: interval.unwrap_or(1),
            by_day: by_day.unwrap_or_default(),
            by_month_day: by_month_day.unwrap_or_default(),
            by_set_pos: by_set_pos.unwrap_or_default(),
        };
        rule.check()?;
        Ok(rule)
    }
}

fn set<T>(slot: &mut Option<T>, name: &str, value: T) -> Result<(), RuleError> {
    if slot.replace(value).is_some() {
        return Err(RuleError::Duplicate(name.to_owned()));
    }
    Ok(())
}

fn parse_frequency(value: &str) -> Result<Frequency, RuleError> {
    match value.to_ascii_uppercase().as_str() {
        "DAILY" => Ok(Frequency::Daily),
        "WEEKLY" => Ok(Frequency::Weekly),
        "MONTHLY" => Ok(Frequency::Monthly),
        _ => Err(RuleError::Unsupported(format!("FREQ={value}"))),
    }
}

fn parse_interval(value: &str) -> Result<u32, RuleError> {
    value
        .parse()
        .ok()
        .filter(|interval| *interval > 0)
        .ok_or_else(|| RuleError::Invalid {
            part: "INTERVAL",
            value: value.to_owned(),
            expected: "a positive integer",
        })
}

fn parse_list<T>(
    value: &str,
    entry: fn(&str) -> Result<T, RuleError>,
) -> Result<Vec<T>, RuleError> {
    value.split(',').map(entry).collect()
}

fn by_day_entry(value: &str) -> Result<ByDay, RuleError> {
    ByDay::parse(value).ok_or_else(|| RuleError::Invalid {
        part: "BYDAY",
        value: value.to_owned(),
        expected: "MO, TU, WE, TH, FR, SA or SU, with a rank of 1 to 5 or -1 to -5 for FREQ=MONTHLY",
    })
}

fn month_day(value: &str) -> Result<i8, RuleError> {
    value
        .parse()
        .ok()
        .filter(|day: &i8| *day != 0 && (-31..=31).contains(day))
        .ok_or_else(|| RuleError::Invalid {
            part: "BYMONTHDAY",
            value: value.to_owned(),
            expected: "1 to 31, or -1 (last day) to -31",
        })
}

fn set_pos(value: &str) -> Result<i16, RuleError> {
    value
        .parse()
        .ok()
        .filter(|pos: &i16| *pos != 0 && (-366..=366).contains(pos))
        .ok_or_else(|| RuleError::Invalid {
            part: "BYSETPOS",
            value: value.to_owned(),
            expected: "1 to 366, or -1 (last) to -366",
        })
}

impl Rule {
    /// The rules across parts: ranks in `BYDAY` and `BYMONTHDAY` for monthly rules only,
    /// `BYSETPOS` for weekly and monthly rules, after `BYDAY` or `BYMONTHDAY`.
    fn check(&self) -> Result<(), RuleError> {
        let with = |part: String| RuleError::UnsupportedWith {
            part,
            freq: self.freq.name(),
        };
        match self.freq {
            Frequency::Monthly => {}
            Frequency::Daily | Frequency::Weekly => {
                if let Some(ranked) = self.by_day.iter().find(|day| day.nth.is_some()) {
                    return Err(with(format!("BYDAY={ranked}")));
                }
                if !self.by_month_day.is_empty() {
                    return Err(with("BYMONTHDAY".to_owned()));
                }
            }
        }
        if !self.by_set_pos.is_empty() {
            match self.freq {
                Frequency::Daily => return Err(with("BYSETPOS".to_owned())),
                Frequency::Weekly | Frequency::Monthly => {}
            }
            if self.by_day.is_empty() && self.by_month_day.is_empty() {
                return Err(RuleError::SetPositionAlone);
            }
        }
        Ok(())
    }

    /// What the rule needs a start date for, if anything: counting `INTERVAL`, or the day of a
    /// weekly or monthly rule that does not give it.
    fn needs_start(&self) -> Option<&'static str> {
        if self.interval > 1 {
            return Some("INTERVAL counts periods from it");
        }
        match self.freq {
            Frequency::Daily => None,
            Frequency::Weekly => self
                .by_day
                .is_empty()
                .then_some("without BYDAY, the weekday is the start's"),
            Frequency::Monthly => (self.by_day.is_empty() && self.by_month_day.is_empty())
                .then_some("without BYDAY or BYMONTHDAY, the day of the month is the start's"),
        }
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FREQ={}", self.freq.name())?;
        if self.interval > 1 {
            write!(f, ";INTERVAL={}", self.interval)?;
        }
        write_list(f, "BYDAY", &self.by_day)?;
        write_list(f, "BYMONTHDAY", &self.by_month_day)?;
        write_list(f, "BYSETPOS", &self.by_set_pos)
    }
}

fn write_list<T: fmt::Display>(f: &mut fmt::Formatter<'_>, name: &str, list: &[T]) -> fmt::Result {
    for (index, entry) in list.iter().enumerate() {
        let separator = if index == 0 { ";" } else { "," };
        let name = if index == 0 { name } else { "" };
        let equals = if index == 0 { "=" } else { "" };
        write!(f, "{separator}{name}{equals}{entry}")?;
    }
    Ok(())
}

/// A rule and the date it starts from (RFC 5545 `DTSTART`): no occurrence before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recurrence {
    rule: Rule,
    start: Option<Date>,
}

/// A rule that needs a start date was given none.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{rule}` needs `start`: {reason}")]
pub struct MissingStart {
    rule: String,
    reason: &'static str,
}

impl Recurrence {
    /// `rule` from `start`.
    ///
    /// # Errors
    ///
    /// [`MissingStart`] when `start` is `None` and the rule has an `INTERVAL` above 1, or is
    /// weekly without `BYDAY`, or monthly without `BYDAY` nor `BYMONTHDAY`.
    pub fn new(rule: Rule, start: Option<Date>) -> Result<Self, MissingStart> {
        if start.is_none()
            && let Some(reason) = rule.needs_start()
        {
            return Err(MissingStart {
                rule: rule.to_string(),
                reason,
            });
        }
        Ok(Self { rule, start })
    }

    /// The rule.
    #[must_use]
    pub const fn rule(&self) -> &Rule {
        &self.rule
    }

    /// The start date, if any.
    #[must_use]
    pub const fn start(&self) -> Option<Date> {
        self.start
    }

    /// The first day of the period of `day`: itself, its Monday, or the first of its month.
    fn period_of(&self, day: Date) -> Option<Date> {
        match self.rule.freq {
            Frequency::Daily => Some(day),
            Frequency::Weekly => day
                .checked_sub(i64::from(day.weekday().to_monday_zero_offset()).days())
                .ok(),
            Frequency::Monthly => Some(day.first_of_month()),
        }
    }

    /// The first day of the period before the one starting on `period`.
    fn previous_period(&self, period: Date) -> Option<Date> {
        match self.rule.freq {
            Frequency::Daily => period.yesterday().ok(),
            Frequency::Weekly => period.checked_sub(7.days()).ok(),
            Frequency::Monthly => period.yesterday().ok().map(Date::first_of_month),
        }
    }

    /// The last day of the period starting on `period`.
    fn period_end(&self, period: Date) -> Option<Date> {
        match self.rule.freq {
            Frequency::Daily => Some(period),
            Frequency::Weekly => period.checked_add(6.days()).ok(),
            Frequency::Monthly => Some(period.last_of_month()),
        }
    }

    /// Whether the period starting on `period` is one the interval keeps: a multiple of
    /// `INTERVAL` periods after the start's.
    fn counts(&self, period: Date) -> bool {
        if self.rule.interval == 1 {
            return true;
        }
        let Some(first) = self.start.and_then(|start| self.period_of(start)) else {
            return false;
        };
        let elapsed = match self.rule.freq {
            Frequency::Daily => days_between(first, period),
            Frequency::Weekly => days_between(first, period).map(|days| days / 7),
            Frequency::Monthly => Some(
                (i64::from(period.year()) - i64::from(first.year())) * 12
                    + i64::from(period.month())
                    - i64::from(first.month()),
            ),
        };
        elapsed.is_some_and(|elapsed| elapsed >= 0 && elapsed % i64::from(self.rule.interval) == 0)
    }

    /// The occurrences of the period starting on `period`, sorted, before the start filter.
    fn occurrences_in(&self, period: Date) -> Vec<Date> {
        if !self.counts(period) {
            return Vec::new();
        }
        let mut days = match self.rule.freq {
            Frequency::Daily => {
                let keep = self.rule.by_day.is_empty()
                    || self
                        .rule
                        .by_day
                        .iter()
                        .any(|by| by.weekday == period.weekday());
                if keep { vec![period] } else { Vec::new() }
            }
            Frequency::Weekly => self.weekly(period),
            Frequency::Monthly => self.monthly(period),
        };
        days.sort_unstable();
        days.dedup();
        self.select_positions(days)
    }

    fn weekly(&self, monday: Date) -> Vec<Date> {
        let weekdays: Vec<Weekday> = if self.rule.by_day.is_empty() {
            self.start
                .map(|start| start.weekday())
                .into_iter()
                .collect()
        } else {
            self.rule.by_day.iter().map(|by| by.weekday).collect()
        };
        weekdays
            .into_iter()
            .filter_map(|weekday| {
                monday
                    .checked_add(i64::from(weekday.to_monday_zero_offset()).days())
                    .ok()
            })
            .collect()
    }

    fn monthly(&self, first: Date) -> Vec<Date> {
        let length = first.days_in_month();
        let from_month_days: Option<Vec<Date>> = (!self.rule.by_month_day.is_empty()).then(|| {
            self.rule
                .by_month_day
                .iter()
                .filter_map(|&day| {
                    let day = if day < 0 { length + 1 + day } else { day };
                    (1..=length)
                        .contains(&day)
                        .then(|| first.with().day(day).build().ok())
                        .flatten()
                })
                .collect()
        });
        let from_weekdays: Option<Vec<Date>> = (!self.rule.by_day.is_empty()).then(|| {
            self.rule
                .by_day
                .iter()
                .flat_map(|by| match by.nth {
                    Some(nth) => first
                        .nth_weekday_of_month(nth, by.weekday)
                        .ok()
                        .into_iter()
                        .collect::<Vec<_>>(),
                    None => (1..=5)
                        .filter_map(|nth| first.nth_weekday_of_month(nth, by.weekday).ok())
                        .collect(),
                })
                .collect()
        });
        match (from_month_days, from_weekdays) {
            (Some(month_days), Some(weekdays)) => month_days
                .into_iter()
                .filter(|day| weekdays.contains(day))
                .collect(),
            (Some(days), None) | (None, Some(days)) => days,
            (None, None) => self
                .start
                .and_then(|start| {
                    (start.day() <= length)
                        .then(|| first.with().day(start.day()).build().ok())
                        .flatten()
                })
                .into_iter()
                .collect(),
        }
    }

    /// `BYSETPOS`: the days at these positions of the sorted `days`, 1 for the first, -1 for
    /// the last. All of them without `BYSETPOS`.
    fn select_positions(&self, days: Vec<Date>) -> Vec<Date> {
        if self.rule.by_set_pos.is_empty() {
            return days;
        }
        let Ok(count) = i16::try_from(days.len()) else {
            return Vec::new();
        };
        let mut selected: Vec<Date> = self
            .rule
            .by_set_pos
            .iter()
            .filter_map(|&pos| {
                let index = if pos > 0 { pos - 1 } else { count + pos };
                usize::try_from(index)
                    .ok()
                    .and_then(|index| days.get(index).copied())
            })
            .collect();
        selected.sort_unstable();
        selected.dedup();
        selected
    }
}

fn days_between(from: Date, to: Date) -> Option<i64> {
    to.since(from).ok().map(|span| i64::from(span.get_days()))
}

/// The occurrence to create a task for on `today`, if any: the latest occurrence of
/// `recurrence` that is not after `today` nor before its start, and after `last_created` (the
/// occurrence of the latest task created for it). Without any, only an occurrence on `today`
/// itself: a new schedule never creates a task for a date already past.
///
/// Older missed occurrences are never returned: after a week offline, one task is created, not
/// one per missed day.
#[must_use]
pub fn due_occurrence(
    recurrence: &Recurrence,
    last_created: Option<Date>,
    today: Date,
) -> Option<Date> {
    let earliest = match last_created {
        Some(last) => last.tomorrow().ok()?,
        None => today,
    };
    let earliest = recurrence
        .start
        .map_or(earliest, |start| earliest.max(start));
    if earliest > today {
        return None;
    }
    let mut period = recurrence.period_of(today)?;
    loop {
        if recurrence.period_end(period)? < earliest {
            return None;
        }
        let latest = recurrence
            .occurrences_in(period)
            .into_iter()
            .rev()
            .find(|day| *day <= today);
        if let Some(day) = latest {
            return (day >= earliest).then_some(day);
        }
        period = recurrence.previous_period(period)?;
    }
}

/// What to do with the open tasks of earlier occurrences of a schedule when a new occurrence
/// is due: `policy` in the configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    /// Create nothing while a task of an earlier occurrence is open.
    #[default]
    Skip,
    /// Create the task and keep the open ones.
    Stack,
    /// Create the task and cancel the open ones.
    Replace,
}

/// What [`apply_policy`] decides for a due occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Create no task: one is still open.
    Skip,
    /// Create the task, then cancel these tasks.
    Create {
        /// Open tasks of earlier occurrences to cancel.
        cancel: Vec<TaskId>,
    },
}

/// What to do for a due occurrence of a schedule, given its tasks still open.
#[must_use]
pub fn apply_policy(previous_open: &[TaskId], policy: Policy) -> PolicyDecision {
    match policy {
        Policy::Skip if !previous_open.is_empty() => PolicyDecision::Skip,
        Policy::Skip | Policy::Stack => PolicyDecision::Create { cancel: Vec::new() },
        Policy::Replace => PolicyDecision::Create {
            cancel: previous_open.to_vec(),
        },
    }
}

/// Provider of the origin link of a task created by a schedule.
pub const SCHEDULE_PROVIDER: &str = "schedule";

/// Key of the origin link of the task of occurrence `date` of schedule `schedule`:
/// `<schedule>/<date>`, the dedup key.
#[must_use]
pub fn occurrence_key(schedule: &str, date: Date) -> String {
    format!("{schedule}/{date}")
}

/// The schedule and occurrence date of an origin key written by [`occurrence_key`].
#[must_use]
pub fn parse_occurrence_key(key: &str) -> Option<(&str, Date)> {
    let (schedule, date) = key.rsplit_once('/')?;
    Some((schedule, date.parse().ok()?))
}

#[cfg(test)]
mod tests;
