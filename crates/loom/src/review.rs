//! `loom review sync`, `queue` and `start`: the request paths and the text output.

use std::fmt::Write as _;

use loom_api::{
    ContextName, QueueReason, ReviewList, ReviewQueue, ReviewStartResult, ReviewSyncOutcome,
    ReviewSyncResult,
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
pub fn render_queue(queue: &ReviewQueue) -> String {
    if queue.entries.is_empty() {
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
                for failure in failed_lists {
                    let _ = writeln!(
                        text,
                        "  list {}: {}",
                        list_name(failure.list),
                        failure.error
                    );
                }
            }
            ReviewSyncOutcome::RateLimited { until } => {
                let _ = writeln!(text, "rate limited until {until}");
            }
            ReviewSyncOutcome::Failed { error } => {
                let _ = writeln!(text, "failed: {error}");
            }
        }
    }
    text
}

/// What failed in `result`, to end the command with an error; `None` when nothing failed. Pure.
///
/// A rate limit is not a failure. Each sentence says only what is true of its case: a failed list
/// does not mean that the MRs keep a snapshot, since the poll updated them.
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
            ReviewSyncOutcome::Failed { .. } => polls += 1,
            ReviewSyncOutcome::RateLimited { .. } => {}
        }
    }
    let mut parts = Vec::new();
    if polls > 0 {
        parts.push(format!(
            "{polls} forge poll(s) failed: their MRs keep their last snapshot"
        ));
    }
    if mrs > 0 {
        parts.push(format!("{mrs} MR(s) failed: they keep their last snapshot"));
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
        ForgeKind, QueueReason, ReviewList, ReviewListFailure, ReviewListFailureKind, ReviewQueue,
        ReviewQueueEntry, ReviewSync, ReviewSyncFailure, ReviewSyncOutcome, ReviewSyncResult,
        Timestamp,
    };

    use super::{
        EMPTY_QUEUE, failure_message, queue_path, reason_name, render_queue, render_sync,
        start_path,
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
            render_queue(&ReviewQueue {
                entries: Vec::new()
            }),
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
            render_queue(&queue),
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
                    },
                ),
                forge(
                    "gitlab.company-a.example",
                    ReviewSyncOutcome::Failed {
                        error: "no usable answer from the forge: timeout".to_owned(),
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
            Some(
                "1 forge poll(s) failed: their MRs keep their last snapshot; 1 MR(s) failed: they keep their last snapshot"
            )
        );
        // A rate limit alone is not a failure.
        assert_eq!(
            failure_message(&ReviewSyncResult {
                forges: vec![forge(
                    "gitlab.example.com",
                    ReviewSyncOutcome::RateLimited {
                        until: Timestamp::from_unix_secs(0).unwrap(),
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
}
