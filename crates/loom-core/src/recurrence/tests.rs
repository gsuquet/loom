use super::*;
use crate::id::TaskId;

fn date(text: &str) -> Date {
    text.parse().unwrap()
}

fn rule(text: &str) -> Recurrence {
    Recurrence::new(text.parse().unwrap(), None).unwrap()
}

fn rule_from(text: &str, start: &str) -> Recurrence {
    Recurrence::new(text.parse().unwrap(), Some(date(start))).unwrap()
}

/// Every date of `from..=to` on which `recurrence` creates a task when checked once a day,
/// starting with no task created.
fn created_daily(recurrence: &Recurrence, from: &str, to: &str) -> Vec<Date> {
    let (mut day, to) = (date(from), date(to));
    let mut last = None;
    let mut created = Vec::new();
    while day <= to {
        if let Some(occurrence) = due_occurrence(recurrence, last, day) {
            created.push(occurrence);
            last = Some(occurrence);
        }
        day = day.tomorrow().unwrap();
    }
    created
}

fn dates(texts: &[&str]) -> Vec<Date> {
    texts.iter().map(|text| date(text)).collect()
}

const LAST_WORKING_DAY: &str = "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1";

#[test]
fn last_working_day_of_the_month() {
    let created = created_daily(&rule(LAST_WORKING_DAY), "2026-01-01", "2026-12-31");
    assert_eq!(
        created,
        dates(&[
            "2026-01-30", // Saturday the 31st
            "2026-02-27",
            "2026-03-31",
            "2026-04-30",
            "2026-05-29", // Sunday the 31st
            "2026-06-30",
            "2026-07-31",
            "2026-08-31",
            "2026-09-30",
            "2026-10-30", // Saturday the 31st
            "2026-11-30",
            "2026-12-31",
        ])
    );
}

#[test]
fn every_friday() {
    let created = created_daily(&rule("FREQ=WEEKLY;BYDAY=FR"), "2026-10-01", "2026-10-31");
    assert_eq!(
        created,
        dates(&[
            "2026-10-02",
            "2026-10-09",
            "2026-10-16",
            "2026-10-23",
            "2026-10-30"
        ])
    );
}

#[test]
fn a_week_offline_creates_only_the_latest_missed_occurrence() {
    let daily = rule("FREQ=DAILY");
    // Last created on Monday, back the next Monday.
    assert_eq!(
        due_occurrence(&daily, Some(date("2026-10-05")), date("2026-10-12")),
        Some(date("2026-10-12"))
    );
    let fridays = rule("FREQ=WEEKLY;BYDAY=FR");
    // Last created on Friday the 2nd, back on Sunday the 18th: Friday the 16th, not the 9th.
    assert_eq!(
        due_occurrence(&fridays, Some(date("2026-10-02")), date("2026-10-18")),
        Some(date("2026-10-16"))
    );
}

#[test]
fn an_occurrence_already_created_is_not_due_again() {
    let fridays = rule("FREQ=WEEKLY;BYDAY=FR");
    for today in ["2026-10-16", "2026-10-17", "2026-10-22"] {
        assert_eq!(
            due_occurrence(&fridays, Some(date("2026-10-16")), date(today)),
            None,
            "{today}"
        );
    }
    // A creation recorded after today (a clock set back) creates nothing either.
    assert_eq!(
        due_occurrence(&fridays, Some(date("2026-10-23")), date("2026-10-17")),
        None
    );
}

#[test]
fn a_schedule_without_any_task_starts_on_its_first_occurrence_from_today() {
    let fridays = rule("FREQ=WEEKLY;BYDAY=FR");
    // Seen first on a Saturday: last Friday is not created.
    assert_eq!(due_occurrence(&fridays, None, date("2026-10-17")), None);
    assert_eq!(
        due_occurrence(&fridays, None, date("2026-10-16")),
        Some(date("2026-10-16"))
    );
}

#[test]
fn no_occurrence_before_the_start() {
    let daily = rule_from("FREQ=DAILY", "2026-10-20");
    assert_eq!(due_occurrence(&daily, None, date("2026-10-19")), None);
    assert_eq!(
        due_occurrence(&daily, Some(date("2026-01-01")), date("2026-10-19")),
        None
    );
    assert_eq!(
        due_occurrence(&daily, None, date("2026-10-20")),
        Some(date("2026-10-20"))
    );
}

#[test]
fn intervals_count_from_the_start() {
    let every_other_friday = rule_from("FREQ=WEEKLY;INTERVAL=2;BYDAY=FR", "2026-10-02");
    assert_eq!(
        created_daily(&every_other_friday, "2026-10-01", "2026-11-30"),
        dates(&[
            "2026-10-02",
            "2026-10-16",
            "2026-10-30",
            "2026-11-13",
            "2026-11-27"
        ])
    );
    // A start in the middle of a week counts from that week.
    let from_wednesday = rule_from("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,FR", "2026-10-07");
    assert_eq!(
        created_daily(&from_wednesday, "2026-10-07", "2026-10-31"),
        dates(&["2026-10-09", "2026-10-19", "2026-10-23"])
    );
    let every_third_day = rule_from("FREQ=DAILY;INTERVAL=3", "2026-10-01");
    assert_eq!(
        created_daily(&every_third_day, "2026-10-01", "2026-10-10"),
        dates(&["2026-10-01", "2026-10-04", "2026-10-07", "2026-10-10"])
    );
    let quarterly = rule_from("FREQ=MONTHLY;INTERVAL=3;BYMONTHDAY=1", "2026-01-01");
    assert_eq!(
        created_daily(&quarterly, "2026-01-01", "2026-12-31"),
        dates(&["2026-01-01", "2026-04-01", "2026-07-01", "2026-10-01"])
    );
}

#[test]
fn month_days_from_the_end_and_short_months() {
    let last_day = rule("FREQ=MONTHLY;BYMONTHDAY=-1");
    assert_eq!(
        created_daily(&last_day, "2026-01-01", "2026-04-30"),
        dates(&["2026-01-31", "2026-02-28", "2026-03-31", "2026-04-30"])
    );
    // RFC 5545: a day the month does not have is skipped.
    let the_31st = rule("FREQ=MONTHLY;BYMONTHDAY=31");
    assert_eq!(
        created_daily(&the_31st, "2026-01-01", "2026-05-31"),
        dates(&["2026-01-31", "2026-03-31", "2026-05-31"])
    );
    // Without BYMONTHDAY or BYDAY, the day of the start.
    let like_start = rule_from("FREQ=MONTHLY", "2026-01-30");
    assert_eq!(
        created_daily(&like_start, "2026-01-01", "2026-04-30"),
        dates(&["2026-01-30", "2026-03-30", "2026-04-30"])
    );
}

#[test]
fn nth_weekday_of_the_month() {
    let first_monday = rule("FREQ=MONTHLY;BYDAY=1MO");
    assert_eq!(
        created_daily(&first_monday, "2026-09-01", "2026-11-30"),
        dates(&["2026-09-07", "2026-10-05", "2026-11-02"])
    );
    let last_friday = rule("FREQ=MONTHLY;BYDAY=-1FR");
    assert_eq!(
        created_daily(&last_friday, "2026-09-01", "2026-11-30"),
        dates(&["2026-09-25", "2026-10-30", "2026-11-27"])
    );
    // BYMONTHDAY and BYDAY together: their intersection, here Friday the 13th.
    let friday_13 = rule("FREQ=MONTHLY;BYDAY=FR;BYMONTHDAY=13");
    assert_eq!(
        created_daily(&friday_13, "2026-01-01", "2026-12-31"),
        dates(&["2026-02-13", "2026-03-13", "2026-11-13"])
    );
}

#[test]
fn daily_with_weekdays_and_weekly_set_positions() {
    let working_days = rule("FREQ=DAILY;BYDAY=MO,TU,WE,TH,FR");
    assert_eq!(
        created_daily(&working_days, "2026-10-15", "2026-10-20"),
        dates(&["2026-10-15", "2026-10-16", "2026-10-19", "2026-10-20"])
    );
    let first_of_the_week = rule("FREQ=WEEKLY;BYDAY=TU,TH;BYSETPOS=1");
    assert_eq!(
        created_daily(&first_of_the_week, "2026-10-12", "2026-10-25"),
        dates(&["2026-10-13", "2026-10-20"])
    );
}

#[test]
fn rules_parse_case_insensitively_and_name_what_is_wrong() {
    assert_eq!(
        "freq=weekly;byday=fr".parse::<Rule>(),
        "FREQ=WEEKLY;BYDAY=FR".parse::<Rule>()
    );
    let cases: &[(&str, &str)] = &[
        ("", "FREQ is missing"),
        ("BYDAY=FR", "FREQ is missing"),
        ("FREQ=WEEKLY;;BYDAY=FR", "empty part"),
        ("FREQ", "`FREQ` is not `NAME=VALUE`"),
        ("FREQ=YEARLY", "`FREQ=YEARLY` is not supported"),
        ("FREQ=HOURLY", "`FREQ=HOURLY` is not supported"),
        ("FREQ=DAILY;COUNT=3", "`COUNT` is not supported"),
        ("FREQ=DAILY;UNTIL=20261231", "`UNTIL` is not supported"),
        ("FREQ=DAILY;BYHOUR=9", "`BYHOUR` is not supported"),
        ("FREQ=DAILY;BYMINUTE=0", "`BYMINUTE` is not supported"),
        ("FREQ=DAILY;BYMONTH=1", "`BYMONTH` is not supported"),
        ("FREQ=WEEKLY;WKST=SU;BYDAY=FR", "`WKST` is not supported"),
        ("FREQ=DAILY;X-NAME=1", "`X-NAME` is not supported"),
        ("FREQ=DAILY;FREQ=WEEKLY", "FREQ is given twice"),
        ("FREQ=DAILY;INTERVAL=0", "INTERVAL `0`"),
        ("FREQ=DAILY;INTERVAL=x", "INTERVAL `x`"),
        ("FREQ=WEEKLY;BYDAY=FRI", "BYDAY `FRI`"),
        ("FREQ=WEEKLY;BYDAY=", "BYDAY ``"),
        ("FREQ=MONTHLY;BYDAY=6MO", "BYDAY `6MO`"),
        ("FREQ=MONTHLY;BYDAY=0MO", "BYDAY `0MO`"),
        (
            "FREQ=WEEKLY;BYDAY=1FR",
            "`BYDAY=1FR` is not supported with FREQ=WEEKLY",
        ),
        (
            "FREQ=DAILY;BYDAY=-1FR",
            "`BYDAY=-1FR` is not supported with FREQ=DAILY",
        ),
        ("FREQ=MONTHLY;BYMONTHDAY=0", "BYMONTHDAY `0`"),
        ("FREQ=MONTHLY;BYMONTHDAY=32", "BYMONTHDAY `32`"),
        (
            "FREQ=WEEKLY;BYMONTHDAY=1",
            "`BYMONTHDAY` is not supported with FREQ=WEEKLY",
        ),
        ("FREQ=MONTHLY;BYDAY=FR;BYSETPOS=0", "BYSETPOS `0`"),
        ("FREQ=MONTHLY;BYDAY=FR;BYSETPOS=367", "BYSETPOS `367`"),
        (
            "FREQ=DAILY;BYDAY=FR;BYSETPOS=1",
            "`BYSETPOS` is not supported with FREQ=DAILY",
        ),
        (
            "FREQ=MONTHLY;BYSETPOS=1",
            "BYSETPOS needs BYDAY or BYMONTHDAY",
        ),
    ];
    for (text, needle) in cases {
        let error = text.parse::<Rule>().unwrap_err().to_string();
        assert!(error.contains(needle), "{text:?}: {error}");
    }
}

#[test]
fn a_rule_that_does_not_fix_its_days_needs_a_start() {
    let cases: &[(&str, &str)] = &[
        ("FREQ=DAILY;INTERVAL=2", "INTERVAL"),
        ("FREQ=WEEKLY", "BYDAY"),
        ("FREQ=MONTHLY", "BYDAY or BYMONTHDAY"),
    ];
    for (text, needle) in cases {
        let error = Recurrence::new(text.parse().unwrap(), None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("needs `start`"), "{text}: {error}");
        assert!(error.contains(needle), "{text}: {error}");
        assert!(Recurrence::new(text.parse().unwrap(), Some(date("2026-10-01"))).is_ok());
    }
}

#[test]
fn rules_display_in_canonical_form() {
    let rule: Rule = "byday=fr,mo;freq=monthly;bysetpos=-1".parse().unwrap();
    assert_eq!(rule.to_string(), "FREQ=MONTHLY;BYDAY=FR,MO;BYSETPOS=-1");
    assert_eq!(rule.to_string().parse::<Rule>().unwrap(), rule);
    let rule: Rule = "FREQ=MONTHLY;INTERVAL=2;BYDAY=-1FR;BYMONTHDAY=-3,1"
        .parse()
        .unwrap();
    assert_eq!(
        rule.to_string(),
        "FREQ=MONTHLY;INTERVAL=2;BYDAY=-1FR;BYMONTHDAY=-3,1"
    );
}

#[test]
fn a_very_old_last_creation_ends_the_search() {
    // A fifth Monday is never the first of the month: the search stops at the last creation.
    let never = rule("FREQ=MONTHLY;BYDAY=5MO;BYMONTHDAY=1");
    assert_eq!(
        due_occurrence(&never, Some(date("0001-01-01")), date("2026-10-12")),
        None
    );
}

fn id(text: &str) -> TaskId {
    text.parse().unwrap()
}

#[test]
fn policies() {
    let open = [id("0000000a"), id("0000000b")];
    for policy in [Policy::Skip, Policy::Stack, Policy::Replace] {
        assert_eq!(
            apply_policy(&[], policy),
            PolicyDecision::Create { cancel: vec![] },
            "{policy:?}"
        );
    }
    assert_eq!(apply_policy(&open, Policy::Skip), PolicyDecision::Skip);
    assert_eq!(
        apply_policy(&open, Policy::Stack),
        PolicyDecision::Create { cancel: vec![] }
    );
    assert_eq!(
        apply_policy(&open, Policy::Replace),
        PolicyDecision::Create {
            cancel: open.to_vec()
        }
    );
}

#[test]
fn occurrence_keys_round_trip() {
    let key = occurrence_key("weekly-review", date("2026-10-16"));
    assert_eq!(key, "weekly-review/2026-10-16");
    assert_eq!(
        parse_occurrence_key(&key),
        Some(("weekly-review", date("2026-10-16")))
    );
    assert_eq!(parse_occurrence_key("weekly-review"), None);
    assert_eq!(parse_occurrence_key("weekly-review/2026-13-01"), None);
}
