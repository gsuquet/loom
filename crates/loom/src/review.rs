//! `loom review sync`, `queue` and `start`: the request paths and the text output.

use std::fmt::Write as _;

use loom_api::{
    ConnectorList, ContextName, QueueReason, ReviewList, ReviewListFailure, ReviewQueue,
    ReviewStartResult, ReviewSyncOutcome, ReviewSyncResult,
};

use crate::repo::encode_segment;

/// `POST /v1/reviews/sync`.
pub const SYNC_PATH: &str = "/v1/reviews/sync";

/// `GET /v1/reviews/queue`, of one context when `context` names it.
#[must_use]
pub fn queue_path(context: Option<&ContextName>) -> String {
    context.map_or_else(
        || "/v1/reviews/queue".to_owned(),
        |context| {
            format!(
                "/v1/reviews/queue?context={}",
                encode_segment(context.as_str())
            )
        },
    )
}

/// `POST /v1/reviews/queue/{key}/start`: `key` holds `/` and `#` or `!`, which are
/// percent-encoded.
#[must_use]
pub fn start_path(key: &str) -> String {
    format!("/v1/reviews/queue/{}/start", encode_segment(key))
}

/// The text of an empty queue. It does not claim that nothing waits: a poll that fails changes no
/// entry, and this command calls no forge.
const NOTHING_TO_DO: &str = "";
const PROBLEM_HEADER: &str = "";
const PROBLEM_FOOTER: &str = "";
const EMPTY_QUEUE: &str = "The queue is empty. Run `loom review sync` to poll the forges now.\n";

const fn reason_name(reason: QueueReason) -> &'static str {
    match reason {
        QueueReason::ReviewRequested => "review_requested",
        QueueReason::Assigned => "assigned",
        QueueReason::Mentioned => "mentioned",
        QueueReason::OwnNewComments => "own_new_comments",
        QueueReason::OwnPipelineFailed => "own_pipeline_failed",
        QueueReason::OwnReadyToMerge => "own_ready_to_merge",
    }
}

const fn list_name(list: ReviewList) -> &'static str {
    match list {
        ReviewList::Assigned => "assigned",
        ReviewList::Own => "own",
        ReviewList::Mentioned => "mentioned",
    }
}

/// One line per entry: context, key, reasons, title, author, URL.
#[must_use]
pub fn render_queue(queue: &ReviewQueue, connectors: Option<&ConnectorList>) -> String {
    if queue.entries.is_empty() {
        let _ = connectors;
        return EMPTY_QUEUE.to_owned();
    }
    let mut text = String::new();
    for entry in &queue.entries {
        let reasons: Vec<&str> = entry.reasons.iter().copied().map(reason_name).collect();
        let _ = writeln!(
            text,
            "{} {} [{}] {} ({}) {}",
            entry.context,
            entry.key,
            reasons.join(", "),
            entry.title,
            entry.author,
            entry.web_url
        );
    }
    text
}

/// One line: the task, and whether this start created it. A task that exists can be in any
/// status, such as `inbox` for a task of before the queue.
#[must_use]
pub fn render_start(result: &ReviewStartResult) -> String {
    let id = &result.task.summary.id;
    let title = result
        .task
        .summary
        .task
        .as_ref()
        .map_or("", |task| task.title.as_str());
    if result.created {
        format!("Started {id}: {title}\n")
    } else {
        let status = result
            .task
            .summary
            .task
            .as_ref()
            .and_then(|task| task.status.name())
            .map_or_else(|| "unknown".to_owned(), |status| status.to_string());
        format!("Review task {id} already exists ({status}): {title}\n")
    }
}

/// One line per context and forge host, then one indented line per MR that failed.
#[must_use]
pub fn render_sync(result: &ReviewSyncResult) -> String {
    if result.forges.is_empty() {
        return "No forge is polled here: add a forge credential (`token_cmd` or `token_env`), or poll on the first machine of the context.\n"
            .to_owned();
    }
    let mut text = String::new();
    for forge in &result.forges {
        let _ = write!(text, "{} {}: ", forge.context, forge.host);
        match &forge.outcome {
            ReviewSyncOutcome::Synced {
                mrs,
                added,
                removed,
                changed,
                other_context,
                failed,
                failed_lists,
            } => {
                let _ = write!(
                    text,
                    "{mrs} MR(s), {} added to the queue, {} removed, {} changed",
                    added.len(),
                    removed.len(),
                    changed.len()
                );
                if *other_context > 0 {
                    let _ = write!(text, ", {other_context} of another context");
                }
                if !failed.is_empty() {
                    let _ = write!(text, ", {} failed", failed.len());
                }
                if !failed_lists.is_empty() {
                    let _ = write!(text, ", {} list(s) failed", failed_lists.len());
                }
                text.push('\n');
                for failure in failed {
                    let _ = writeln!(text, "  {}: {}", failure.key, failure.error);
                }
                write_failed_lists(&mut text, failed_lists);
            }
            ReviewSyncOutcome::RateLimited {
                until,
                failed_lists,
            } => {
                let _ = writeln!(text, "rate limited until {until}");
                write_failed_lists(&mut text, failed_lists);
            }
            ReviewSyncOutcome::Failed {
                error,
                failed_lists,
            } => {
                let _ = writeln!(text, "failed: {error}");
                write_failed_lists(&mut text, failed_lists);
            }
        }
    }
    text
}

/// One indented line per list that failed.
fn write_failed_lists(text: &mut String, failed_lists: &[ReviewListFailure]) {
    for failure in failed_lists {
        let _ = writeln!(
            text,
            "  list {}: {}",
            list_name(failure.list),
            failure.error
        );
    }
}

/// What failed in `result`, to end the command with an error; `None` when nothing failed. Pure.
///
/// A rate limit alone is not a failure, but a list that failed before it is. Each sentence says
/// only what is true of each mix: a stopped poll applied what it read, a held entry follows its
/// read, and my own MR with no context has no snapshot. So only the MRs not read keep one.
#[must_use]
pub fn failure_message(result: &ReviewSyncResult) -> Option<String> {
    let (mut polls, mut mrs, mut lists) = (0, 0, 0);
    for forge in &result.forges {
        match &forge.outcome {
            ReviewSyncOutcome::Synced {
                failed,
                failed_lists,
                ..
            } => {
                mrs += failed.len();
                lists += failed_lists.len();
            }
            ReviewSyncOutcome::Failed { failed_lists, .. } => {
                polls += 1;
                lists += failed_lists.len();
            }
            ReviewSyncOutcome::RateLimited { failed_lists, .. } => lists += failed_lists.len(),
        }
    }
    let mut parts = Vec::new();
    let what = match (polls, mrs) {
        (0, 0) => None,
        (polls, 0) => Some(format!("{polls} forge poll(s) failed")),
        (0, mrs) => Some(format!("{mrs} MR(s) failed")),
        (polls, mrs) => Some(format!("{polls} forge poll(s) failed, {mrs} MR(s) failed")),
    };
    if let Some(what) = what {
        parts.push(format!("{what}: MRs not read keep their last snapshot"));
    }
    if lists > 0 {
        parts.push(format!(
            "{lists} list(s) failed: the poll updated each MR that it read, and the next poll reads the list(s) again"
        ));
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

#[cfg(test)]
mod tests {
    use loom_api::{
        ConnectorCounts, ConnectorFailure, ConnectorHealth, ConnectorKind, ConnectorList,
        ConnectorStatus, ForgeKind, PolledBy, QueueReason, ReviewList, ReviewListFailure,
        ReviewListFailureKind, ReviewQueue, ReviewQueueEntry, ReviewSync, ReviewSyncFailure,
        ReviewSyncOutcome, ReviewSyncResult, Secret, Timestamp,
    };

    use super::{
        EMPTY_QUEUE, NOTHING_TO_DO, PROBLEM_FOOTER, PROBLEM_HEADER, failure_message, queue_path,
        reason_name, render_queue, render_sync, start_path,
    };

    #[test]
    fn the_paths_encode_the_key_and_the_context() {
        assert_eq!(
            start_path("acme/shop#412"),
            "/v1/reviews/queue/acme%2Fshop%23412/start"
        );
        assert_eq!(
            start_path("company-a/checkout!88"),
            "/v1/reviews/queue/company-a%2Fcheckout%2188/start"
        );
        assert_eq!(queue_path(None), "/v1/reviews/queue");
        assert_eq!(
            queue_path(Some(&"company-a".parse().unwrap())),
            "/v1/reviews/queue?context=company-a"
        );
    }

    #[test]
    fn a_list_that_failed_is_a_failure_and_is_shown() {
        let result = ReviewSyncResult {
            forges: vec![ReviewSync {
                context: "company-a".parse().unwrap(),
                host: "gitlab.example.com".to_owned(),
                kind: ForgeKind::Gitlab,
                last_success: None,
                outcome: ReviewSyncOutcome::Synced {
                    mrs: 1,
                    added: Vec::new(),
                    removed: Vec::new(),
                    changed: Vec::new(),
                    other_context: 0,
                    failed: Vec::new(),
                    failed_lists: vec![ReviewListFailure {
                        list: ReviewList::Mentioned,
                        kind: ReviewListFailureKind::Forbidden,
                        error: "403 Forbidden".to_owned(),
                    }],
                },
            }],
        };
        assert_eq!(
            failure_message(&result).as_deref(),
            Some(
                "1 list(s) failed: the poll updated each MR that it read, and the next poll reads the list(s) again"
            )
        );
        assert_eq!(
            render_sync(&result),
            "company-a gitlab.example.com: 1 MR(s), 0 added to the queue, 0 removed, 0 changed, 1 list(s) failed\n  list mentioned: 403 Forbidden\n"
        );
    }

    #[test]
    fn each_reason_has_the_name_of_the_api() {
        for (reason, name) in [
            (QueueReason::ReviewRequested, "review_requested"),
            (QueueReason::Assigned, "assigned"),
            (QueueReason::Mentioned, "mentioned"),
            (QueueReason::OwnNewComments, "own_new_comments"),
            (QueueReason::OwnPipelineFailed, "own_pipeline_failed"),
            (QueueReason::OwnReadyToMerge, "own_ready_to_merge"),
        ] {
            assert_eq!(reason_name(reason), name);
            assert_eq!(serde_json::to_value(reason).unwrap(), name);
        }
    }

    #[test]
    fn renders_the_queue_and_says_when_it_is_empty() {
        assert_eq!(
            render_queue(
                &ReviewQueue {
                    entries: Vec::new()
                },
                None
            ),
            EMPTY_QUEUE
        );
        let queue = ReviewQueue {
            entries: vec![ReviewQueueEntry {
                context: "personal".parse().unwrap(),
                key: "acme/shop#412".to_owned(),
                title: "Auth refactor".to_owned(),
                author: "alice".to_owned(),
                web_url: "https://github.com/acme/shop/pull/412".to_owned(),
                head_sha: "7ebf283bfee25ab48f95c0c55181a4ca66b4c990".to_owned(),
                updated_at: Timestamp::from_unix_secs(0).unwrap(),
                reasons: vec![QueueReason::ReviewRequested],
                seen_at: None,
            }],
        };
        assert_eq!(
            render_queue(&queue, None),
            "personal acme/shop#412 [review_requested] Auth refactor (alice) https://github.com/acme/shop/pull/412\n"
        );
        assert_eq!(
            reason_name(QueueReason::ReviewRequested),
            "review_requested"
        );
    }

    fn forge(host: &str, outcome: ReviewSyncOutcome) -> ReviewSync {
        ReviewSync {
            context: "company-a".parse().unwrap(),
            host: host.to_owned(),
            kind: ForgeKind::Github,
            last_success: None,
            outcome,
        }
    }

    #[test]
    fn renders_each_poll_and_counts_the_failures() {
        let result = ReviewSyncResult {
            forges: vec![
                forge(
                    "github.com",
                    ReviewSyncOutcome::Synced {
                        mrs: 2,
                        added: vec!["acme/shop#7".to_owned()],
                        removed: vec!["acme/shop#6".to_owned(), "acme/shop#5".to_owned()],
                        changed: vec!["k3q7zm2a".parse().unwrap()],
                        other_context: 1,
                        failed: vec![ReviewSyncFailure {
                            key: "acme/shop#412".to_owned(),
                            error: "not found on the forge (404): Not Found".to_owned(),
                        }],
                        failed_lists: Vec::new(),
                    },
                ),
                forge(
                    "gitlab.example.com",
                    ReviewSyncOutcome::RateLimited {
                        until: Timestamp::from_unix_secs(0).unwrap(),
                        failed_lists: Vec::new(),
                    },
                ),
                forge(
                    "gitlab.company-a.example",
                    ReviewSyncOutcome::Failed {
                        error: "no usable answer from the forge: timeout".to_owned(),
                        failed_lists: Vec::new(),
                    },
                ),
            ],
        };
        assert_eq!(
            render_sync(&result),
            "company-a github.com: 2 MR(s), 1 added to the queue, 2 removed, 1 changed, 1 of another context, 1 failed\n  acme/shop#412: not found on the forge (404): Not Found\ncompany-a gitlab.example.com: rate limited until 1970-01-01T00:00:00Z\ncompany-a gitlab.company-a.example: failed: no usable answer from the forge: timeout\n"
        );
        assert_eq!(
            failure_message(&result).as_deref(),
            Some("1 forge poll(s) failed, 1 MR(s) failed: MRs not read keep their last snapshot")
        );
        // A rate limit alone is not a failure.
        assert_eq!(
            failure_message(&ReviewSyncResult {
                forges: vec![forge(
                    "gitlab.example.com",
                    ReviewSyncOutcome::RateLimited {
                        until: Timestamp::from_unix_secs(0).unwrap(),
                        failed_lists: Vec::new(),
                    },
                )],
            }),
            None
        );
        assert!(
            render_sync(&ReviewSyncResult { forges: Vec::new() })
                .starts_with("No forge is polled here")
        );
    }

    #[test]
    fn a_failed_list_fails_the_command_in_each_mix_and_each_sentence_stays_true() {
        const NOT_READ: &str = "MRs not read keep their last snapshot";
        const LIST: &str = "1 list(s) failed: the poll updated each MR that it read, and the next poll reads the list(s) again";
        let list = || {
            vec![ReviewListFailure {
                list: ReviewList::Own,
                kind: ReviewListFailureKind::Forbidden,
                error: "403 Forbidden".to_owned(),
            }]
        };
        let synced = |failed: usize, failed_lists| ReviewSyncOutcome::Synced {
            mrs: 1,
            added: Vec::new(),
            removed: Vec::new(),
            changed: Vec::new(),
            other_context: 0,
            failed: vec![
                ReviewSyncFailure {
                    key: "acme/shop!1".to_owned(),
                    error: "403 Forbidden".to_owned(),
                };
                failed
            ],
            failed_lists,
        };
        let limited = |failed_lists| ReviewSyncOutcome::RateLimited {
            until: Timestamp::from_unix_secs(0).unwrap(),
            failed_lists,
        };
        let stopped = |failed_lists| ReviewSyncOutcome::Failed {
            error: "timeout".to_owned(),
            failed_lists,
        };
        let table = [
            (
                "list, then a rate limit",
                vec![limited(list())],
                LIST.to_owned(),
            ),
            (
                "list, then a stop",
                vec![stopped(list())],
                format!("1 forge poll(s) failed: {NOT_READ}; {LIST}"),
            ),
            (
                "MR and list",
                vec![synced(1, list())],
                format!("1 MR(s) failed: {NOT_READ}; {LIST}"),
            ),
            (
                "MR alone",
                vec![synced(1, Vec::new())],
                format!("1 MR(s) failed: {NOT_READ}"),
            ),
            (
                "poll alone",
                vec![stopped(Vec::new())],
                format!("1 forge poll(s) failed: {NOT_READ}"),
            ),
            (
                "two hosts",
                vec![stopped(Vec::new()), synced(1, list())],
                format!("1 forge poll(s) failed, 1 MR(s) failed: {NOT_READ}; {LIST}"),
            ),
        ];
        for (name, outcomes, expected) in table {
            let result = ReviewSyncResult {
                forges: outcomes
                    .into_iter()
                    .map(|outcome| forge("gitlab.example.com", outcome))
                    .collect(),
            };
            assert_eq!(failure_message(&result), Some(expected), "{name}");
        }
    }

    #[test]
    fn a_stopped_poll_prints_the_lists_that_failed_before_it() {
        let failed_lists = vec![ReviewListFailure {
            list: ReviewList::Mentioned,
            kind: ReviewListFailureKind::Forbidden,
            error: "403 Forbidden".to_owned(),
        }];
        let result = ReviewSyncResult {
            forges: vec![
                forge(
                    "gitlab.example.com",
                    ReviewSyncOutcome::RateLimited {
                        until: Timestamp::from_unix_secs(0).unwrap(),
                        failed_lists: failed_lists.clone(),
                    },
                ),
                forge(
                    "gitlab.company-a.example",
                    ReviewSyncOutcome::Failed {
                        error: "timeout".to_owned(),
                        failed_lists,
                    },
                ),
            ],
        };
        assert_eq!(
            render_sync(&result),
            "company-a gitlab.example.com: rate limited until 1970-01-01T00:00:00Z\n  list mentioned: 403 Forbidden\ncompany-a gitlab.company-a.example: failed: timeout\n  list mentioned: 403 Forbidden\n"
        );
    }

    fn connector(kind: ConnectorKind, health: ConnectorHealth) -> ConnectorStatus {
        ConnectorStatus {
            context: "work".parse().unwrap(),
            name: "forge.example.com".to_owned(),
            kind,
            host: "forge.example.com".to_owned(),
            polled_by: PolledBy::ThisMachine,
            health,
            stale: false,
            last_attempt: None,
            last_success: None,
            next_poll: None,
            items: ConnectorCounts::default(),
            capabilities: kind.capabilities(),
        }
    }

    fn empty(connectors: Vec<ConnectorStatus>) -> String {
        render_queue(
            &ReviewQueue {
                entries: Vec::new(),
            },
            Some(&ConnectorList { connectors }),
        )
    }

    const NOTHING: &str = NOTHING_TO_DO;

    #[test]
    fn an_empty_queue_with_good_polls_says_nothing_waits() {
        let ok = connector(ConnectorKind::Gitlab, ConnectorHealth::Ok);
        assert_eq!(empty(vec![ok.clone()]), NOTHING);
        let github = connector(ConnectorKind::Github, ConnectorHealth::Ok);
        assert_eq!(empty(vec![ok, github]), NOTHING);
    }

    #[test]
    fn an_empty_queue_with_a_failed_poll_shows_it_for_each_health() {
        let failed = vec![ConnectorFailure {
            key: "list:own".to_owned(),
            reason: "502".to_owned(),
        }];
        let cases = [
            (
                ConnectorHealth::Partial {
                    failed: failed.clone(),
                },
                "partial, 1 failed",
            ),
            (
                ConnectorHealth::RateLimited {
                    until: Timestamp::from_unix_secs(1_800_000_000).unwrap(),
                    failed: vec![],
                },
                "rate limited until ",
            ),
            (
                ConnectorHealth::Credential {
                    which: Secret::Token,
                    error: "401".to_owned(),
                    failed: vec![],
                },
                "token problem: 401",
            ),
            (
                ConnectorHealth::Forbidden {
                    error: "403".to_owned(),
                    failed: vec![],
                },
                "forbidden: 403",
            ),
            (
                ConnectorHealth::InvalidFilter {
                    error: "bad".to_owned(),
                },
                "invalid filter: bad",
            ),
            (
                ConnectorHealth::Unreachable {
                    error: "down".to_owned(),
                    failures: 2,
                    failed: vec![],
                },
                "unreachable (2 failed in a row): down",
            ),
        ];
        for (health, line) in cases {
            let text = empty(vec![connector(ConnectorKind::Gitlab, health)]);
            assert!(text.starts_with(PROBLEM_HEADER), "{text}");
            assert!(
                text.contains(&format!("work forge.example.com: {line}")),
                "{text}"
            );
            assert!(text.ends_with(PROBLEM_FOOTER), "{text}");
        }
        let text = empty(vec![connector(
            ConnectorKind::Gitlab,
            ConnectorHealth::Partial { failed },
        )]);
        assert!(text.contains("\n  list:own: 502\n"), "{text}");
    }

    #[test]
    fn a_stale_success_is_a_problem_but_a_poll_to_come_is_not() {
        let stale = ConnectorStatus {
            stale: true,
            ..connector(ConnectorKind::Github, ConnectorHealth::Ok)
        };
        let text = empty(vec![stale]);
        assert!(text.starts_with(PROBLEM_HEADER), "{text}");
        assert!(text.contains("ok, stale"), "{text}");
        // Not polled yet is stale too, and says nothing about a failure.
        let fresh = ConnectorStatus {
            stale: true,
            ..connector(ConnectorKind::Github, ConnectorHealth::NotPolledYet)
        };
        assert_eq!(empty(vec![fresh]), EMPTY_QUEUE);
    }

    #[test]
    fn a_failure_shows_only_the_connectors_that_failed() {
        let ok = connector(ConnectorKind::Github, ConnectorHealth::Ok);
        let down = ConnectorStatus {
            name: "down.example.com".to_owned(),
            ..connector(
                ConnectorKind::Gitlab,
                ConnectorHealth::Unreachable {
                    error: "down".to_owned(),
                    failures: 1,
                    failed: vec![],
                },
            )
        };
        let text = empty(vec![ok, down]);
        assert!(
            text.contains("work down.example.com: unreachable"),
            "{text}"
        );
        assert!(!text.contains("forge.example.com"), "{text}");
    }

    #[test]
    fn no_claim_when_no_poll_here_says_so() {
        let not_yet = connector(ConnectorKind::Gitlab, ConnectorHealth::NotPolledYet);
        let ok = connector(ConnectorKind::Github, ConnectorHealth::Ok);
        let other = ConnectorStatus {
            polled_by: PolledBy::Other {
                machine: "tower".to_owned(),
            },
            ..connector(ConnectorKind::Gitlab, ConnectorHealth::NotPolledYet)
        };
        assert_eq!(empty(vec![not_yet.clone()]), EMPTY_QUEUE);
        assert_eq!(empty(vec![other.clone()]), EMPTY_QUEUE);
        // One good poll does not speak for a forge that did not poll.
        assert_eq!(empty(vec![ok.clone(), not_yet]), EMPTY_QUEUE);
        assert_eq!(empty(vec![ok, other]), EMPTY_QUEUE);
    }

    #[test]
    fn no_claim_without_a_forge_or_a_status() {
        assert_eq!(empty(vec![]), EMPTY_QUEUE);
        // Only the forges fill the queue: a failure of another connector is not shown here.
        let datadog = connector(
            ConnectorKind::Datadog,
            ConnectorHealth::Unreachable {
                error: "down".to_owned(),
                failures: 1,
                failed: vec![],
            },
        );
        assert_eq!(empty(vec![datadog.clone()]), EMPTY_QUEUE);
        let ok = connector(ConnectorKind::Gitlab, ConnectorHealth::Ok);
        assert_eq!(empty(vec![datadog, ok]), NOTHING);
        let none = render_queue(
            &ReviewQueue {
                entries: Vec::new(),
            },
            None,
        );
        assert_eq!(none, EMPTY_QUEUE);
    }

    #[test]
    fn a_text_of_the_forge_is_cleaned_in_the_empty_queue() {
        let text = empty(vec![connector(
            ConnectorKind::Gitlab,
            ConnectorHealth::Forbidden {
                error: "a\u{202e}b\nc\u{1b}[2J".to_owned(),
                failed: vec![ConnectorFailure {
                    key: "k\u{202d}\n".to_owned(),
                    reason: "r\u{1b}".to_owned(),
                }],
            },
        )]);
        assert!(text.contains("forbidden: ab c [2J"), "{text:?}");
        assert!(text.contains("\n  k : r \n"), "{text:?}");
        assert!(
            !text.contains('\u{1b}') && !text.contains('\u{202e}'),
            "{text:?}"
        );
    }

    #[test]
    fn a_queue_with_entries_ignores_the_status() {
        let queue = ReviewQueue {
            entries: vec![ReviewQueueEntry {
                context: "personal".parse().unwrap(),
                key: "acme/shop#412".to_owned(),
                title: "t".to_owned(),
                author: "a".to_owned(),
                web_url: "u".to_owned(),
                head_sha: "x".to_owned(),
                updated_at: Timestamp::from_unix_secs(0).unwrap(),
                reasons: vec![QueueReason::Assigned],
                seen_at: None,
            }],
        };
        let down = ConnectorList {
            connectors: vec![connector(
                ConnectorKind::Gitlab,
                ConnectorHealth::Unreachable {
                    error: "down".to_owned(),
                    failures: 1,
                    failed: vec![],
                },
            )],
        };
        assert_eq!(
            render_queue(&queue, Some(&down)),
            render_queue(&queue, None)
        );
    }
}
