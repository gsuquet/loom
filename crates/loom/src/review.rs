//! `loom review sync`: the request path and the text output (ADR-0016).

use std::fmt::Write as _;

use loom_api::{ReviewSyncOutcome, ReviewSyncResult};

/// `POST /v1/reviews/sync`.
pub const SYNC_PATH: &str = "/v1/reviews/sync";

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
                created,
                changed,
                other_context,
                failed,
            } => {
                let _ = write!(
                    text,
                    "{mrs} MR(s), {} new Review task(s), {} changed",
                    created.len(),
                    changed.len()
                );
                if *other_context > 0 {
                    let _ = write!(text, ", {other_context} of another context");
                }
                if !failed.is_empty() {
                    let _ = write!(text, ", {} failed", failed.len());
                }
                text.push('\n');
                for failure in failed {
                    let _ = writeln!(text, "  {}: {}", failure.key, failure.error);
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

/// Number of polls that failed, or that have an MR that failed.
#[must_use]
pub fn failures(result: &ReviewSyncResult) -> usize {
    result
        .forges
        .iter()
        .filter(|forge| match &forge.outcome {
            ReviewSyncOutcome::Synced { failed, .. } => !failed.is_empty(),
            ReviewSyncOutcome::Failed { .. } => true,
            ReviewSyncOutcome::RateLimited { .. } => false,
        })
        .count()
}

#[cfg(test)]
mod tests {
    use loom_api::{
        ForgeKind, ReviewSync, ReviewSyncFailure, ReviewSyncOutcome, ReviewSyncResult, Timestamp,
    };

    use super::{failures, render_sync};

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
                        created: vec!["k3q7zm2a".parse().unwrap()],
                        changed: Vec::new(),
                        other_context: 1,
                        failed: vec![ReviewSyncFailure {
                            key: "acme/shop#412".to_owned(),
                            error: "not found on the forge (404): Not Found".to_owned(),
                        }],
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
            "company-a github.com: 2 MR(s), 1 new Review task(s), 0 changed, 1 of another context, 1 failed\n  acme/shop#412: not found on the forge (404): Not Found\ncompany-a gitlab.example.com: rate limited until 1970-01-01T00:00:00Z\ncompany-a gitlab.company-a.example: failed: no usable answer from the forge: timeout\n"
        );
        assert_eq!(failures(&result), 2);
        assert!(
            render_sync(&ReviewSyncResult { forges: Vec::new() })
                .starts_with("No forge is polled here")
        );
    }
}
