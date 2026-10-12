//! Instants on the wire: RFC 3339 strings in UTC, whole seconds.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Seconds since the Unix epoch of `9999-12-31T23:59:59Z`, the last instant with a 4-digit year.
const MAX_UNIX_SECS: u64 = 253_402_300_799;
const SECS_PER_DAY: u64 = 86_400;
/// Length of the only accepted form, `YYYY-MM-DDTHH:MM:SSZ`.
const TEXT_LEN: usize = 20;

/// An instant, serialized as an RFC 3339 string in UTC with whole seconds: `2026-10-12T08:00:00Z`.
///
/// Only that canonical form is accepted when deserializing: no fractional seconds, no offset
/// other than `Z`, uppercase `T` and `Z`, years 1970 to 9999. `loom-api` has no clock: callers
/// build a `Timestamp` from seconds since the Unix epoch they read themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp {
    unix_secs: u64,
}

impl Timestamp {
    /// Builds the instant `unix_secs` seconds after `1970-01-01T00:00:00Z`.
    ///
    /// Returns `None` after `9999-12-31T23:59:59Z`, which RFC 3339 cannot represent.
    #[must_use]
    pub const fn from_unix_secs(unix_secs: u64) -> Option<Self> {
        if unix_secs > MAX_UNIX_SECS {
            None
        } else {
            Some(Self { unix_secs })
        }
    }

    /// Seconds since `1970-01-01T00:00:00Z`.
    #[must_use]
    pub const fn unix_secs(self) -> u64 {
        self.unix_secs
    }

    /// Parses the canonical form `YYYY-MM-DDTHH:MM:SSZ`; `None` for anything else.
    fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != TEXT_LEN {
            return None;
        }
        for (index, separator) in [
            (4, b'-'),
            (7, b'-'),
            (10, b'T'),
            (13, b':'),
            (16, b':'),
            (19, b'Z'),
        ] {
            if bytes[index] != separator {
                return None;
            }
        }
        let year = digits(&bytes[0..4])?;
        let month = digits(&bytes[5..7])?;
        let day = digits(&bytes[8..10])?;
        let hour = digits(&bytes[11..13])?;
        let minute = digits(&bytes[14..16])?;
        let second = digits(&bytes[17..19])?;

        let valid = year >= 1970
            && (1..=12).contains(&month)
            && (1..=days_in_month(year, month)).contains(&day)
            && hour < 24
            && minute < 60
            // Leap seconds (`:60`) are rejected: Unix time does not count them.
            && second < 60;
        if !valid {
            return None;
        }
        let secs =
            days_from_civil(year, month, day) * SECS_PER_DAY + hour * 3600 + minute * 60 + second;
        Self::from_unix_secs(secs)
    }
}

/// RFC 3339 in UTC: `YYYY-MM-DDTHH:MM:SSZ`.
impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (year, month, day) = civil_from_days(self.unix_secs / SECS_PER_DAY);
        let secs_of_day = self.unix_secs % SECS_PER_DAY;
        let (hour, minute, second) = (
            secs_of_day / 3600,
            secs_of_day % 3600 / 60,
            secs_of_day % 60,
        );
        write!(
            f,
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
        )
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| {
            D::Error::custom(format!(
                "invalid timestamp {text:?}: expected RFC 3339 in UTC with whole seconds, such as 2026-10-12T08:00:00Z"
            ))
        })
    }
}

impl JsonSchema for Timestamp {
    fn schema_name() -> Cow<'static, str> {
        "Timestamp".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "RFC 3339 instant in UTC with whole seconds, such as 2026-10-12T08:00:00Z.",
            "type": "string",
            "format": "date-time",
            "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"
        })
    }
}

/// Decimal value of a run of ASCII digits; `None` if any byte is not a digit.
fn digits(bytes: &[u8]) -> Option<u64> {
    bytes.iter().try_fold(0, |value, &byte| {
        byte.is_ascii_digit()
            .then(|| value * 10 + u64::from(byte - b'0'))
    })
}

const fn is_leap_year(year: u64) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

const fn days_in_month(year: u64, month: u64) -> u64 {
    match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

// Howard Hinnant's `days_from_civil` and `civil_from_days`
// (https://howardhinnant.github.io/date_algorithms.html), restricted to years >= 1970 so that
// every intermediate value is a non-negative `u64`.

/// Days since 1970-01-01 of a valid date with `year >= 1970`.
const fn days_from_civil(year: u64, month: u64, day: u64) -> u64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year / 400;
    let year_of_era = year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// `(year, month, day)` of the date `days` days after 1970-01-01.
const fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let days = days + 719_468;
    let era = days / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::{MAX_UNIX_SECS, Timestamp};
    use serde_json::json;

    fn at(unix_secs: u64) -> Timestamp {
        Timestamp::from_unix_secs(unix_secs).unwrap()
    }

    #[test]
    fn formats_known_instants() {
        for (secs, text) in [
            (0, "1970-01-01T00:00:00Z"),
            (951_782_400, "2000-02-29T00:00:00Z"),
            (1_791_792_000, "2026-10-12T08:00:00Z"),
            (4_107_542_399, "2100-02-28T23:59:59Z"),
            (MAX_UNIX_SECS, "9999-12-31T23:59:59Z"),
        ] {
            assert_eq!(at(secs).to_string(), text, "{secs}");
            assert_eq!(Timestamp::parse(text), Some(at(secs)), "{text}");
        }
    }

    #[test]
    fn rejects_instants_after_year_9999() {
        assert_eq!(Timestamp::from_unix_secs(MAX_UNIX_SECS + 1), None);
    }

    #[test]
    fn round_trips_every_day_boundary_over_four_centuries() {
        // 1970 to 2370: covers the 2000 (leap) and 2100, 2200, 2300 (not leap) century rules.
        for day in 0..146_097 {
            let instant = at(day * 86_400 + 86_399);
            assert_eq!(Timestamp::parse(&instant.to_string()), Some(instant));
        }
    }

    #[test]
    fn rejects_non_canonical_or_invalid_text() {
        for text in [
            "",
            "2026-10-12",
            "2026-10-12T08:00:00",
            "2026-10-12T08:00:00z",
            "2026-10-12t08:00:00Z",
            "2026-10-12 08:00:00Z",
            "2026-10-12T08:00:00.5Z",
            "2026-10-12T08:00:00+00:00",
            "2026-10-12T10:00:00+02:00",
            "1969-12-31T23:59:59Z",
            "2026-00-12T08:00:00Z",
            "2026-13-12T08:00:00Z",
            "2026-10-00T08:00:00Z",
            "2026-02-29T08:00:00Z",
            "2100-02-29T08:00:00Z",
            "2026-04-31T08:00:00Z",
            "2026-10-12T24:00:00Z",
            "2026-10-12T08:60:00Z",
            "2026-12-31T23:59:60Z",
            "2026-1a-12T08:00:00Z",
            "+026-10-12T08:00:00Z",
        ] {
            assert_eq!(Timestamp::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn serializes_as_a_string_and_round_trips() {
        let instant = at(1_791_792_000);
        let value = serde_json::to_value(instant).unwrap();
        assert_eq!(value, json!("2026-10-12T08:00:00Z"));
        assert_eq!(serde_json::from_value::<Timestamp>(value).unwrap(), instant);
    }

    #[test]
    fn deserialization_error_names_the_expected_form() {
        let error =
            serde_json::from_value::<Timestamp>(json!("2026-10-12T10:00:00+02:00")).unwrap_err();
        assert!(error.to_string().contains("RFC 3339 in UTC"), "{error}");
        assert!(serde_json::from_value::<Timestamp>(json!(1_791_792_000)).is_err());
    }

    #[test]
    fn orders_by_instant() {
        assert!(at(1) < at(2));
    }
}
