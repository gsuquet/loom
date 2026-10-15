//! When loomd archives the closed tasks (ADR-0013, decision 10), decided without I/O: at start,
//! then every [`ARCHIVE_INTERVAL`], at the time [`archive_now`] gives.

use std::time::Duration;

use jiff::tz::TimeZone;
use loom_api::{Date, Timestamp};

/// Time between two archive passes after the one at start.
pub const ARCHIVE_INTERVAL: Duration = Duration::from_hours(1);

/// Most tasks one pass moves (one commit each, a few git processes): a larger backlog is moved
/// by passes queued one after the other, so that the requests queued meanwhile run in between.
pub const ARCHIVE_BATCH: usize = 50;

/// The time the archive rule compares closings with: `test` (`YYYY-MM-DD`, from
/// `LOOM_TEST_TODAY`) at `00:00:00Z` when `allowed` (a debug build) and valid, otherwise `now`.
#[must_use]
pub fn archive_now(test: Option<&str>, allowed: bool, now: Timestamp) -> Timestamp {
    test.filter(|_| allowed)
        .and_then(|text| text.parse::<Date>().ok())
        .and_then(midnight_utc)
        .unwrap_or(now)
}

/// `00:00:00Z` of `date`; `None` before 1970 or after 9999.
fn midnight_utc(date: Date) -> Option<Timestamp> {
    let secs = date.to_zoned(TimeZone::UTC).ok()?.timestamp().as_second();
    Timestamp::from_unix_secs(u64::try_from(secs).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> Timestamp {
        Timestamp::from_unix_secs(secs).unwrap()
    }

    #[test]
    fn a_debug_build_archives_at_midnight_of_the_test_date() {
        // 2026-10-23T00:00:00Z.
        assert_eq!(
            archive_now(Some("2026-10-23"), true, at(5)),
            at(1_792_713_600)
        );
    }

    #[test]
    fn otherwise_the_clock_is_used() {
        assert_eq!(archive_now(Some("2026-10-23"), false, at(5)), at(5));
        assert_eq!(archive_now(None, true, at(5)), at(5));
        assert_eq!(archive_now(Some("tomorrow"), true, at(5)), at(5));
        assert_eq!(archive_now(Some("1969-12-31"), true, at(5)), at(5));
    }
}
