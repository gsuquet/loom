//! Human output of `loom repo` commands. Pure.

use std::fmt::Write as _;

use loom_api::{PlannedAction, SyncOutcome, SyncResult};

/// `loom repo sync`: one line per repository, `<slug>  <outcome>`. The outcomes of an update
/// are separated by `; `; the actions of a dry run by `, `.
#[must_use]
pub fn render_sync(result: &SyncResult) -> String {
    let mut text = String::new();
    for repo in &result.repos {
        let outcome = match &repo.result {
            SyncOutcome::Updated { update } => update
                .outcomes
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
            SyncOutcome::Planned { actions, .. } => planned(actions),
        };
        let _ = writeln!(text, "{}  {outcome}", repo.slug);
    }
    text
}

/// `would fetch, then decide`, `would fetch, skip: …`, `skip: …`.
fn planned(actions: &[PlannedAction]) -> String {
    actions
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use loom_api::{
        LastUpdate, PlannedAction, RepoSync, RepoUpdatePolicy, SkipReason, SyncOutcome, SyncResult,
        Timestamp, UpdateOutcome, UpdateStep,
    };

    use super::render_sync;

    fn updated(slug: &str, outcomes: Vec<UpdateOutcome>) -> RepoSync {
        RepoSync {
            slug: slug.to_owned(),
            result: SyncOutcome::Updated {
                update: LastUpdate {
                    time: Timestamp::EPOCH,
                    policy: RepoUpdatePolicy::Ff,
                    outcomes,
                },
            },
        }
    }

    fn planned(slug: &str, actions: Vec<PlannedAction>) -> RepoSync {
        RepoSync {
            slug: slug.to_owned(),
            result: SyncOutcome::Planned {
                policy: RepoUpdatePolicy::Ff,
                actions,
            },
        }
    }

    #[test]
    fn one_line_per_repository() {
        let id = |c: char| c.to_string().repeat(40);
        let result = SyncResult {
            repos: vec![
                updated(
                    "personal/loom",
                    vec![
                        UpdateOutcome::FastForwarded {
                            branch: "main".to_owned(),
                            from: id('a'),
                            to: id('b'),
                        },
                        UpdateOutcome::DefaultBranchMoved {
                            branch: "dev".to_owned(),
                            from: id('c'),
                            to: id('d'),
                        },
                    ],
                ),
                updated(
                    "work/api",
                    vec![UpdateOutcome::Skipped {
                        reason: SkipReason::Dirty { paths: 2 },
                    }],
                ),
                updated(
                    "work/vpn",
                    vec![UpdateOutcome::Failed {
                        step: UpdateStep::Fetch,
                        error: "exit status: 128".to_owned(),
                        stderr_tail: "fatal: unable to access\n".to_owned(),
                    }],
                ),
            ],
        };
        assert_eq!(
            render_sync(&result),
            "personal/loom  fast-forwarded main aaaaaaa..bbbbbbb; \
             moved default branch dev ccccccc..ddddddd\n\
             work/api  skipped: 2 changed tracked paths\n\
             work/vpn  failed: fetch: exit status: 128: fatal: unable to access\n"
        );
        assert_eq!(render_sync(&SyncResult { repos: vec![] }), "");
    }

    #[test]
    fn dry_run_lines() {
        let result = SyncResult {
            repos: vec![
                planned(
                    "personal/a",
                    vec![PlannedAction::Fetch, PlannedAction::DecideAfterFetch],
                ),
                planned(
                    "personal/b",
                    vec![
                        PlannedAction::Fetch,
                        PlannedAction::Skip {
                            reason: SkipReason::Detached,
                        },
                    ],
                ),
                planned("personal/c", vec![PlannedAction::Fetch]),
                planned(
                    "personal/d",
                    vec![PlannedAction::Skip {
                        reason: SkipReason::PolicyOff,
                    }],
                ),
            ],
        };
        assert_eq!(
            render_sync(&result),
            "personal/a  would fetch, then decide\n\
             personal/b  would fetch, skip: HEAD detached\n\
             personal/c  would fetch\n\
             personal/d  skip: update policy is off\n"
        );
    }
}
