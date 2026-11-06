use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use loom_api::{
    ChangeEvent, ConnectorCounts, ConnectorFailure, ConnectorHealth, ConnectorKind, ContextName,
    ForgeKind, PolledBy, ReviewList, ReviewListFailure, ReviewListFailureKind, ReviewSyncFailure,
    ReviewSyncOutcome, Secret, Timestamp,
};
use loom_config::config::parse_config;
use loom_config::layout::STATE_DIR_MODE;
use loom_forge::client::{ForgeError, RateLimitWait};
use tempfile::TempDir;

use super::*;
use crate::events::{Bus, LastSeen};

const INTERVAL: Duration = Duration::from_mins(5);

fn at(secs: u64) -> Timestamp {
    Timestamp::from_unix_secs(1_800_000_000 + secs).unwrap()
}

fn context(name: &str) -> ContextName {
    name.parse().unwrap()
}

fn instance(context_name: &str, name: &str, polled_by: PolledBy) -> Instance {
    Instance {
        context: context(context_name),
        name: name.to_owned(),
        kind: ConnectorKind::Gitlab,
        host: name.to_owned(),
        polled_by,
        interval: INTERVAL,
    }
}

fn synced(
    mrs: usize,
    failed: Vec<ReviewSyncFailure>,
    failed_lists: Vec<ReviewListFailure>,
) -> ReviewSyncOutcome {
    ReviewSyncOutcome::Synced {
        mrs,
        added: vec![],
        removed: vec![],
        changed: vec![],
        other_context: 0,
        failed,
        failed_lists,
    }
}

fn mr_failure(key: &str, error: &str) -> ReviewSyncFailure {
    ReviewSyncFailure {
        key: key.to_owned(),
        error: error.to_owned(),
    }
}

fn list_failure(list: ReviewList, kind: ReviewListFailureKind, error: &str) -> ReviewListFailure {
    ReviewListFailure {
        list,
        kind,
        error: error.to_owned(),
    }
}

fn failure(key: &str, reason: &str) -> ConnectorFailure {
    ConnectorFailure {
        key: key.to_owned(),
        reason: reason.to_owned(),
    }
}

fn report(at: Timestamp, health: ConnectorHealth) -> Report {
    Report {
        at,
        attempted: true,
        success: matches!(health, ConnectorHealth::Ok),
        health,
        items: Some(ConnectorCounts::default()),
        next_poll: None,
    }
}

struct Fixture {
    dir: TempDir,
    bus: Arc<Bus>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: TempDir::new().unwrap(),
            bus: Arc::new(Bus::new(1, 16, 16)),
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.path().join("state/connectors.json")
    }

    fn open(&self, instances: Vec<Instance>) -> ConnectorStatuses {
        ConnectorStatuses::open(self.path(), instances, Arc::clone(&self.bus), at(0))
    }
}

fn gitlab(polled_by: PolledBy) -> Vec<Instance> {
    vec![instance("work", "gitlab.com", polled_by)]
}

fn state(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

// `stale`

#[test]
fn stale_after_three_intervals_or_without_a_success() {
    let cases = [
        (None, 0, true),
        (Some(at(0)), 0, false),
        (Some(at(0)), 899, false),
        (Some(at(0)), 900, true),
        (Some(at(0)), 901, true),
        // A clock set back is no outage.
        (Some(at(1000)), 0, false),
    ];
    for (last_success, now, expected) in cases {
        assert_eq!(
            is_stale(last_success, INTERVAL, at(now)),
            expected,
            "{last_success:?} at {now}"
        );
    }
}

// From `ReviewSync` to health.

#[test]
fn a_clean_poll_is_ok_and_counts_its_mrs() {
    let poll = forge_poll(&synced(3, vec![], vec![]), None, 0);
    assert_eq!(poll.health, ConnectorHealth::Ok);
    assert!(poll.success);
    assert_eq!(
        poll.items,
        Some(ConnectorCounts {
            listed: 3,
            ..ConnectorCounts::default()
        })
    );
}

#[test]
fn failed_mrs_and_lists_are_partial_and_the_poll_counts_as_a_success() {
    let outcome = synced(
        3,
        vec![mr_failure("acme/shop!1", "500")],
        vec![list_failure(
            ReviewList::Own,
            ReviewListFailureKind::Other,
            "502",
        )],
    );
    let poll = forge_poll(&outcome, None, 0);
    assert_eq!(
        poll.health,
        ConnectorHealth::Partial {
            failed: vec![failure("acme/shop!1", "500"), failure("list:own", "502")],
        }
    );
    assert!(poll.success);
    assert_eq!(poll.items.unwrap().failed, 2);
}

#[test]
fn each_list_has_its_own_key() {
    for (list, key) in [
        (ReviewList::Assigned, "list:assigned"),
        (ReviewList::Own, "list:own"),
        (ReviewList::Mentioned, "list:mentioned"),
    ] {
        let outcome = synced(
            0,
            vec![],
            vec![list_failure(list, ReviewListFailureKind::Other, "x")],
        );
        assert_eq!(
            forge_poll(&outcome, None, 0).health,
            ConnectorHealth::Partial {
                failed: vec![failure(key, "x")],
            }
        );
    }
}

#[test]
fn a_refused_list_is_forbidden_and_keeps_what_else_failed() {
    let outcome = synced(
        2,
        vec![mr_failure("acme/shop!1", "500")],
        vec![
            list_failure(ReviewList::Own, ReviewListFailureKind::Other, "502"),
            list_failure(
                ReviewList::Mentioned,
                ReviewListFailureKind::Forbidden,
                "no scope",
            ),
            list_failure(
                ReviewList::Assigned,
                ReviewListFailureKind::Forbidden,
                "no access",
            ),
        ],
    );
    let poll = forge_poll(&outcome, None, 0);
    assert_eq!(
        poll.health,
        ConnectorHealth::Forbidden {
            error: "list:mentioned: no scope; list:assigned: no access".to_owned(),
            failed: vec![failure("acme/shop!1", "500"), failure("list:own", "502")],
        }
    );
    // The poll went on: the MRs it read are applied.
    assert!(poll.success);
    assert_eq!(poll.items.unwrap().failed, 4);
}

#[test]
fn a_rate_limit_is_as_it_is_and_keeps_the_counts() {
    let poll = forge_poll(
        &ReviewSyncOutcome::RateLimited {
            until: at(60),
            failed_lists: vec![],
        },
        None,
        2,
    );
    assert_eq!(poll.health, ConnectorHealth::RateLimited { until: at(60) });
    assert!(!poll.success);
    assert_eq!(poll.items, None);
}

#[test]
fn a_stopped_poll_follows_its_cause_and_keeps_the_counts() {
    let failed = ReviewSyncOutcome::Failed {
        error: "why".to_owned(),
        failed_lists: vec![],
    };
    let cases = [
        (
            Some(ForgeStop::Credential),
            ConnectorHealth::Credential {
                which: Secret::Token,
                error: "why".to_owned(),
            },
        ),
        (
            Some(ForgeStop::Forbidden),
            ConnectorHealth::Forbidden {
                error: "why".to_owned(),
                failed: vec![],
            },
        ),
        (
            Some(ForgeStop::Unreachable),
            ConnectorHealth::Unreachable {
                error: "why".to_owned(),
                failures: 3,
            },
        ),
        (
            None,
            ConnectorHealth::Unreachable {
                error: "why".to_owned(),
                failures: 3,
            },
        ),
    ];
    for (stop, health) in cases {
        let poll = forge_poll(&failed, stop, 3);
        assert_eq!(poll.health, health);
        assert!(!poll.success);
        assert_eq!(poll.items, None);
    }
}

#[test]
fn the_error_of_the_forge_gives_the_cause_of_the_stop() {
    let rate = ForgeError::RateLimited {
        status: 429,
        wait: RateLimitWait::Unknown,
        message: String::new(),
    };
    let cases = [
        (
            ForgeError::Unauthorized {
                message: String::new(),
            },
            ForgeStop::Credential,
        ),
        (
            ForgeError::Forbidden {
                message: String::new(),
            },
            ForgeStop::Forbidden,
        ),
        (
            ForgeError::InsufficientScope {
                scope: None,
                message: String::new(),
            },
            ForgeStop::Forbidden,
        ),
        (ForgeError::SsoRequired { url: None }, ForgeStop::Forbidden),
        (
            ForgeError::NotFound {
                message: String::new(),
            },
            ForgeStop::Unreachable,
        ),
        (rate, ForgeStop::Unreachable),
    ];
    for (error, stop) in cases {
        assert_eq!(stop_of(&error), stop, "{error}");
    }
}

// Instances from the configuration.

#[test]
fn every_credential_is_an_instance_with_who_polls_it() {
    let config = parse_config(
        r#"
[contexts.mine]
machines = ["laptop", "tower"]

[contexts.other]
machines = ["tower"]

[contexts.free]

[forges."gitlab.com"]
kind = "gitlab"
token_env = "T"
poll_interval = "10m"

[contexts.free.forges."github.com"]
token_env = "U"
"#,
        Path::new("/c.toml"),
    )
    .unwrap();
    let instances = forge_instances(&config, &"laptop".parse().unwrap());
    let polled: Vec<(&str, &str, &PolledBy)> = instances
        .iter()
        .map(|i| (i.context.as_str(), i.name.as_str(), &i.polled_by))
        .collect();
    assert_eq!(
        polled,
        [
            ("free", "github.com", &PolledBy::EveryMachine),
            ("free", "gitlab.com", &PolledBy::EveryMachine),
            ("mine", "gitlab.com", &PolledBy::ThisMachine),
            (
                "other",
                "gitlab.com",
                &PolledBy::Other {
                    machine: "tower".to_owned()
                }
            ),
        ]
    );
    assert_eq!(instances[0].kind, ConnectorKind::from(ForgeKind::Github));
    assert_eq!(instances[0].host, "github.com");
    assert_eq!(instances[1].interval, Duration::from_mins(10));
    assert_eq!(
        instances[0].interval,
        loom_config::forges::DEFAULT_POLL_INTERVAL
    );
}

// `ConnectorStatuses`.

#[test]
fn before_any_poll_an_instance_is_not_polled_yet_and_stale() {
    let fixture = Fixture::new();
    let statuses = fixture.open(gitlab(PolledBy::ThisMachine));
    let list = statuses.list(None, at(0));
    assert_eq!(list.connectors.len(), 1);
    let status = &list.connectors[0];
    assert_eq!(status.health, ConnectorHealth::NotPolledYet);
    assert!(status.stale);
    assert_eq!(status.last_attempt, None);
    assert_eq!(status.last_success, None);
    assert_eq!(status.capabilities, ConnectorKind::Gitlab.capabilities());
    // Nothing happened: nothing is written.
    assert!(!fixture.path().exists());
}

#[test]
fn another_machine_makes_no_claim() {
    let fixture = Fixture::new();
    let machine = PolledBy::Other {
        machine: "tower".to_owned(),
    };
    let statuses = fixture.open(gitlab(machine.clone()));
    let list = statuses.list(None, at(100_000));
    let status = &list.connectors[0];
    assert_eq!(status.polled_by, machine);
    assert_eq!(status.health, ConnectorHealth::NotPolledYet);
    assert!(!status.stale);
    assert_eq!(status.next_poll, None);
}

#[test]
fn the_list_is_sorted_and_a_context_filters_it() {
    let fixture = Fixture::new();
    let statuses = fixture.open(vec![
        instance("work", "gitlab.com", PolledBy::ThisMachine),
        instance("home", "gitlab.com", PolledBy::ThisMachine),
        instance("home", "github.com", PolledBy::ThisMachine),
    ]);
    let names = |list: ConnectorList| -> Vec<(String, String)> {
        list.connectors
            .into_iter()
            .map(|status| (status.context.to_string(), status.name))
            .collect()
    };
    assert_eq!(
        names(statuses.list(None, at(0))),
        [
            ("home".to_owned(), "github.com".to_owned()),
            ("home".to_owned(), "gitlab.com".to_owned()),
            ("work".to_owned(), "gitlab.com".to_owned()),
        ]
    );
    assert_eq!(
        names(statuses.list(Some(&context("work")), at(0))),
        [("work".to_owned(), "gitlab.com".to_owned())]
    );
    assert_eq!(names(statuses.list(Some(&context("none")), at(0))), []);
}

#[test]
fn a_report_updates_the_status_and_the_file_holds_only_the_times() {
    let fixture = Fixture::new();
    let statuses = fixture.open(gitlab(PolledBy::ThisMachine));
    let context = context("work");
    statuses.report(
        &context,
        "gitlab.com",
        Report {
            at: at(10),
            attempted: true,
            success: true,
            health: ConnectorHealth::Ok,
            items: Some(ConnectorCounts {
                listed: 4,
                ..ConnectorCounts::default()
            }),
            next_poll: Some(at(310)),
        },
    );
    let list = statuses.list(None, at(20));
    let status = &list.connectors[0];
    assert_eq!(status.health, ConnectorHealth::Ok);
    assert!(!status.stale);
    assert_eq!(status.last_attempt, Some(at(10)));
    assert_eq!(status.last_success, Some(at(10)));
    assert_eq!(status.next_poll, Some(at(310)));
    assert_eq!(status.items.listed, 4);
    assert_eq!(
        state(&fixture.path()),
        serde_json::json!({
            "v": 1,
            "connectors": [{
                "context": "work",
                "name": "gitlab.com",
                "last_attempt": at(10),
                "last_success": at(10),
            }],
        })
    );
    assert_eq!(
        fs::metadata(fixture.path()).unwrap().permissions().mode() & 0o777,
        loom_config::layout::STATE_FILE_MODE
    );
    assert_eq!(
        fs::metadata(fixture.dir.path().join("state"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        STATE_DIR_MODE
    );
    // Only the file itself is left in the directory: the temporary file is renamed.
    assert_eq!(
        fs::read_dir(fixture.dir.path().join("state"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn a_failed_poll_moves_last_attempt_and_keeps_last_success_and_the_counts() {
    let fixture = Fixture::new();
    let statuses = fixture.open(gitlab(PolledBy::ThisMachine));
    let context = context("work");
    statuses.report(
        &context,
        "gitlab.com",
        Report {
            items: Some(ConnectorCounts {
                listed: 4,
                ..ConnectorCounts::default()
            }),
            ..report(at(10), ConnectorHealth::Ok)
        },
    );
    statuses.report(
        &context,
        "gitlab.com",
        Report {
            items: None,
            ..report(
                at(400),
                ConnectorHealth::Unreachable {
                    error: "down".to_owned(),
                    failures: 1,
                },
            )
        },
    );
    let list = statuses.list(None, at(400));
    let status = &list.connectors[0];
    assert_eq!(status.last_attempt, Some(at(400)));
    assert_eq!(status.last_success, Some(at(10)));
    assert_eq!(status.items.listed, 4);
}

#[test]
fn a_skipped_poll_moves_no_time_and_writes_nothing() {
    let fixture = Fixture::new();
    let statuses = fixture.open(gitlab(PolledBy::ThisMachine));
    let context = context("work");
    statuses.report(&context, "gitlab.com", report(at(10), ConnectorHealth::Ok));
    fs::remove_file(fixture.path()).unwrap();
    statuses.report(
        &context,
        "gitlab.com",
        Report {
            attempted: false,
            items: None,
            ..report(at(50), ConnectorHealth::RateLimited { until: at(90) })
        },
    );
    let list = statuses.list(None, at(50));
    assert_eq!(list.connectors[0].last_attempt, Some(at(10)));
    assert_eq!(
        list.connectors[0].health,
        ConnectorHealth::RateLimited { until: at(90) }
    );
    // Same bytes as the last write: the removed file does not come back.
    assert!(!fixture.path().exists());
}

#[test]
fn a_restart_keeps_the_times_and_not_the_health() {
    let fixture = Fixture::new();
    let context = context("work");
    {
        let statuses = fixture.open(gitlab(PolledBy::ThisMachine));
        statuses.report(
            &context,
            "gitlab.com",
            Report {
                items: Some(ConnectorCounts {
                    listed: 4,
                    ..ConnectorCounts::default()
                }),
                ..report(at(10), ConnectorHealth::Ok)
            },
        );
    }
    let statuses = fixture.open(gitlab(PolledBy::ThisMachine));
    let list = statuses.list(None, at(20));
    let status = &list.connectors[0];
    assert_eq!(status.health, ConnectorHealth::NotPolledYet);
    assert_eq!(status.last_attempt, Some(at(10)));
    assert_eq!(status.last_success, Some(at(10)));
    assert_eq!(status.items, ConnectorCounts::default());
    assert!(!status.stale);
}

#[test]
fn a_removed_instance_leaves_the_file_at_the_next_write() {
    let fixture = Fixture::new();
    let context = context("work");
    fixture
        .open(vec![
            instance("work", "gitlab.com", PolledBy::ThisMachine),
            instance("work", "github.com", PolledBy::ThisMachine),
        ])
        .report(&context, "github.com", report(at(10), ConnectorHealth::Ok));
    let statuses = fixture.open(gitlab(PolledBy::ThisMachine));
    assert_eq!(statuses.list(None, at(0)).connectors.len(), 1);
    statuses.report(&context, "gitlab.com", report(at(20), ConnectorHealth::Ok));
    let written = state(&fixture.path());
    let names: Vec<&str> = written["connectors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["gitlab.com"]);
}

#[test]
fn an_unreadable_file_is_replaced_and_never_stops_the_start() {
    for text in [
        "not json",
        r#"{"v":2,"connectors":[{"context":"work","name":"gitlab.com","last_success":"2027-01-15T08:00:00Z"}]}"#,
    ] {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.path().parent().unwrap()).unwrap();
        fs::write(fixture.path(), text).unwrap();
        let statuses = fixture.open(gitlab(PolledBy::ThisMachine));
        assert_eq!(statuses.list(None, at(0)).connectors[0].last_success, None);
        statuses.report(
            &context("work"),
            "gitlab.com",
            report(at(10), ConnectorHealth::Ok),
        );
        assert_eq!(state(&fixture.path())["v"], 1);
    }
}

#[test]
fn a_failed_write_keeps_the_status_and_is_tried_again() {
    let fixture = Fixture::new();
    // `state` is a file: the directory cannot be created.
    let blocker = fixture.dir.path().join("state");
    fs::write(&blocker, "").unwrap();
    let statuses = fixture.open(gitlab(PolledBy::ThisMachine));
    let context = context("work");
    statuses.report(&context, "gitlab.com", report(at(10), ConnectorHealth::Ok));
    assert_eq!(
        statuses.list(None, at(10)).connectors[0].health,
        ConnectorHealth::Ok
    );
    fs::remove_file(&blocker).unwrap();
    // A report that moves no time still writes what the failed write lost.
    statuses.report(
        &context,
        "gitlab.com",
        Report {
            attempted: false,
            items: None,
            ..report(at(50), ConnectorHealth::RateLimited { until: at(90) })
        },
    );
    assert_eq!(
        state(&fixture.path())["connectors"][0]["last_attempt"],
        serde_json::to_value(at(10)).unwrap()
    );
}

#[test]
fn a_report_for_an_unknown_instance_changes_nothing() {
    let fixture = Fixture::new();
    let statuses = fixture.open(gitlab(PolledBy::ThisMachine));
    assert!(
        statuses
            .report(
                &context("work"),
                "other.example",
                report(at(10), ConnectorHealth::Ok)
            )
            .is_none()
    );
    assert!(!fixture.path().exists());
}

#[test]
fn the_transition_gives_the_previous_and_the_new_health() {
    let fixture = Fixture::new();
    let statuses = fixture.open(gitlab(PolledBy::ThisMachine));
    let context = context("work");
    let refused = ConnectorHealth::Credential {
        which: Secret::Token,
        error: "401".to_owned(),
    };
    let first = statuses
        .report(&context, "gitlab.com", report(at(10), refused.clone()))
        .unwrap();
    assert_eq!(first.previous, ConnectorHealth::NotPolledYet);
    assert_eq!(first.current, refused);
    let second = statuses
        .report(&context, "gitlab.com", report(at(20), ConnectorHealth::Ok))
        .unwrap();
    assert_eq!(second.previous, refused);
    assert_eq!(second.current, ConnectorHealth::Ok);
}

// The change event.

fn events(bus: &Bus) -> tokio::sync::broadcast::Receiver<crate::events::Numbered> {
    bus.subscribe(LastSeen::Nothing).live
}

fn drained(
    live: &mut tokio::sync::broadcast::Receiver<crate::events::Numbered>,
) -> Vec<ChangeEvent> {
    let mut seen = Vec::new();
    while let Ok(numbered) = live.try_recv() {
        seen.push(numbered.event);
    }
    seen
}

#[test]
fn a_change_of_health_counts_or_stale_sends_one_event_and_times_send_none() {
    let fixture = Fixture::new();
    let mut live = events(&fixture.bus);
    let statuses = fixture.open(gitlab(PolledBy::ThisMachine));
    let context = context("work");
    let changed = vec![ChangeEvent::ConnectorChanged {
        context: context.clone(),
        name: "gitlab.com".to_owned(),
    }];

    // Health.
    statuses.report(&context, "gitlab.com", report(at(10), ConnectorHealth::Ok));
    assert_eq!(drained(&mut live), changed);
    // Only the times moved.
    statuses.report(&context, "gitlab.com", report(at(310), ConnectorHealth::Ok));
    assert_eq!(drained(&mut live), []);
    // Counts.
    statuses.report(
        &context,
        "gitlab.com",
        Report {
            items: Some(ConnectorCounts {
                listed: 2,
                ..ConnectorCounts::default()
            }),
            ..report(at(610), ConnectorHealth::Ok)
        },
    );
    assert_eq!(drained(&mut live), changed);
    // Stale: nothing else differs. A poll skipped 4 intervals later.
    statuses.report(
        &context,
        "gitlab.com",
        Report {
            attempted: false,
            success: false,
            items: None,
            ..report(at(1810), ConnectorHealth::Ok)
        },
    );
    assert_eq!(drained(&mut live), changed);
    // No new `stale`, health or counts.
    statuses.report(
        &context,
        "gitlab.com",
        Report {
            attempted: false,
            success: false,
            items: None,
            ..report(at(1820), ConnectorHealth::Ok)
        },
    );
    assert_eq!(drained(&mut live), []);
}

#[test]
fn a_report_after_the_bus_closed_is_harmless() {
    let fixture = Fixture::new();
    let statuses = fixture.open(gitlab(PolledBy::ThisMachine));
    fixture.bus.close();
    assert!(
        statuses
            .report(
                &context("work"),
                "gitlab.com",
                report(at(10), ConnectorHealth::Ok)
            )
            .is_some()
    );
}

#[test]
fn a_schedule_sets_the_next_poll_with_no_event_and_no_write() {
    let fixture = Fixture::new();
    let mut live = events(&fixture.bus);
    let statuses = fixture.open(gitlab(PolledBy::ThisMachine));
    statuses.schedule(&context("work"), "gitlab.com", at(30));
    assert_eq!(
        statuses.list(None, at(0)).connectors[0].next_poll,
        Some(at(30))
    );
    assert_eq!(drained(&mut live), []);
    assert!(!fixture.path().exists());
}

#[test]
fn an_error_stays_out_of_the_file() {
    let fixture = Fixture::new();
    let statuses = fixture.open(gitlab(PolledBy::ThisMachine));
    statuses.report(
        &context("work"),
        "gitlab.com",
        report(
            at(10),
            ConnectorHealth::Unreachable {
                error: "marker-text".to_owned(),
                failures: 1,
            },
        ),
    );
    let text = fs::read_to_string(fixture.path()).unwrap();
    assert!(!text.contains("marker-text"), "{text}");
}

/// The temporary file of the write is a FIFO with no reader: the write hangs, as on a stuck disk.
#[test]
fn a_read_never_waits_for_a_stuck_write() {
    let fixture = Fixture::new();
    let dir = fixture.path().parent().unwrap().to_path_buf();
    fs::create_dir_all(&dir).unwrap();
    let fifo = dir.join(".connectors.json.tmp");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let statuses = Arc::new(fixture.open(vec![
        instance("work", "gitlab.com", PolledBy::ThisMachine),
        instance("work", "github.com", PolledBy::ThisMachine),
    ]));
    let writer = {
        let statuses = Arc::clone(&statuses);
        thread::spawn(move || {
            statuses.report(
                &context("work"),
                "gitlab.com",
                report(at(10), ConnectorHealth::Ok),
            );
        })
    };
    // On another thread: a read that blocks must fail this test, not hang it.
    let (sent, received) = mpsc::channel();
    let reader = Arc::clone(&statuses);
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let list = reader.list(None, at(10));
            let health = list.connectors[1].health.clone();
            if health == ConnectorHealth::Ok || Instant::now() > deadline {
                let _ = sent.send(health);
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
    });
    let seen = received.recv_timeout(Duration::from_secs(5));
    // Ends the write in any case. A FIFO takes the bytes, then its sync may fail.
    let drained = fs::read(&fifo).unwrap();
    writer.join().unwrap();
    assert_eq!(seen, Ok(ConnectorHealth::Ok));
    assert_ne!(drained, Vec::<u8>::new());
}

/// A read in the gap can show `stale: true`, though the last report was not stale. Only a report
/// tells such a client that it is over.
#[test]
fn a_success_after_a_gap_sends_one_event() {
    let fixture = Fixture::new();
    let mut live = events(&fixture.bus);
    let statuses = fixture.open(gitlab(PolledBy::ThisMachine));
    let context = context("work");
    statuses.report(&context, "gitlab.com", report(at(10), ConnectorHealth::Ok));
    drained(&mut live);
    assert!(statuses.list(None, at(28_800)).connectors[0].stale);

    statuses.report(
        &context,
        "gitlab.com",
        report(at(28_810), ConnectorHealth::Ok),
    );
    assert_eq!(
        drained(&mut live),
        [ChangeEvent::ConnectorChanged {
            context: context.clone(),
            name: "gitlab.com".to_owned(),
        }]
    );
    assert!(!statuses.list(None, at(28_810)).connectors[0].stale);
    // Back to normal: only the times move.
    statuses.report(
        &context,
        "gitlab.com",
        report(at(29_110), ConnectorHealth::Ok),
    );
    assert_eq!(drained(&mut live), []);
}

/// One writer hangs on a FIFO and a second waits behind it. A third report records a newer time
/// while the second waits: the second must write the status of now, not what it saw on arrival.
#[test]
fn a_write_that_waited_holds_the_newest_times() {
    let fixture = Fixture::new();
    let dir = fixture.path().parent().unwrap().to_path_buf();
    fs::create_dir_all(&dir).unwrap();
    let fifo = dir.join(".connectors.json.tmp");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let statuses = Arc::new(fixture.open(gitlab(PolledBy::ThisMachine)));
    let writer = |time: Timestamp| {
        let statuses = Arc::clone(&statuses);
        thread::spawn(move || {
            statuses.report(
                &context("work"),
                "gitlab.com",
                report(time, ConnectorHealth::Ok),
            );
        })
    };
    let wait_for = |time: Timestamp| {
        let deadline = Instant::now() + Duration::from_secs(20);
        while statuses.list(None, time).connectors[0].last_attempt != Some(time) {
            assert!(Instant::now() < deadline, "no report at {time}");
            thread::sleep(Duration::from_millis(5));
        }
    };
    let first = writer(at(5));
    wait_for(at(5));
    // Let the first writer reach the FIFO, and the second reach the lock behind it.
    thread::sleep(Duration::from_millis(200));
    let second = writer(at(10));
    wait_for(at(10));
    thread::sleep(Duration::from_millis(200));

    // A report whose own write has not come yet.
    statuses
        .record(
            &context("work"),
            "gitlab.com",
            report(at(20), ConnectorHealth::Ok),
        )
        .unwrap();

    // The first write ends, and fails: its temporary file is gone. The next one is a real file.
    let mut pipe = fs::File::open(&fifo).unwrap();
    fs::remove_file(&fifo).unwrap();
    let mut drained_bytes = Vec::new();
    std::io::Read::read_to_end(&mut pipe, &mut drained_bytes).unwrap();
    first.join().unwrap();
    second.join().unwrap();
    assert_eq!(
        state(&fixture.path())["connectors"][0]["last_attempt"],
        serde_json::to_value(at(20)).unwrap()
    );
}
