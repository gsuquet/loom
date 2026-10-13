//! Decision tables: every field of the state an update reads, under every policy.

use std::path::Path;

use loom_api::{
    ContextAssignment, MaybeKnown, PlannedAction, RepoHead, RepoOperation, RepoState,
    RepoUpdatePolicy, SkipReason, StateField, Timestamp, Untracked, Upstream,
};
use loom_config::config::parse_config;

use super::{
    Action, FetchDecision, UpdatePolicies, decide_fetch, decide_update, local_blocker, plan,
};
use crate::state::{DefaultBranchRefs, RefTip, UpdateRefs};

const HEAD_ID: &str = "1111111111111111111111111111111111111111";
const UPSTREAM_ID: &str = "2222222222222222222222222222222222222222";
const DEFAULT_ID: &str = "3333333333333333333333333333333333333333";
const DEFAULT_UPSTREAM_ID: &str = "4444444444444444444444444444444444444444";

const ALL_OPERATIONS: [RepoOperation; 6] = [
    RepoOperation::Rebase,
    RepoOperation::Merge,
    RepoOperation::CherryPick,
    RepoOperation::Revert,
    RepoOperation::Bisect,
    RepoOperation::Am,
];

fn upstream(ahead: MaybeKnown<u64>, behind: MaybeKnown<u64>) -> MaybeKnown<Option<Upstream>> {
    MaybeKnown::Known(Some(Upstream {
        name: "origin/feat".to_owned(),
        ahead,
        behind,
    }))
}

/// `feat` checked out, clean, 2 commits behind `origin/feat`, nothing ahead.
fn behind() -> RepoState {
    RepoState {
        refreshed_at: Timestamp::EPOCH,
        head: MaybeKnown::Known(RepoHead::Branch {
            name: "feat".to_owned(),
        }),
        dirty: MaybeKnown::Known(0),
        untracked: MaybeKnown::Known(Untracked {
            count: 0,
            capped: false,
        }),
        operation: MaybeKnown::Known(None),
        upstream: upstream(MaybeKnown::Known(0), MaybeKnown::Known(2)),
        default_branch: MaybeKnown::Known(Some("main".to_owned())),
        last_commit: MaybeKnown::Known(None),
        last_fetch: MaybeKnown::Known(None),
        error: None,
    }
}

fn tip() -> RefTip {
    RefTip {
        name: "refs/remotes/origin/feat".to_owned(),
        id: UPSTREAM_ID.to_owned(),
    }
}

/// The refs of [`behind`], without a default branch to move.
fn refs() -> UpdateRefs {
    UpdateRefs {
        head: MaybeKnown::Known(Some(HEAD_ID.to_owned())),
        upstream: MaybeKnown::Known(Some(tip())),
        default_branch: MaybeKnown::Known(None),
        error: None,
    }
}

/// `main`, not checked out, `behind` commits behind and `ahead` ahead of `origin/main`.
fn default_branch(
    ahead: u64,
    behind: u64,
    checked_out: bool,
) -> MaybeKnown<Option<DefaultBranchRefs>> {
    MaybeKnown::Known(Some(DefaultBranchRefs {
        branch: "main".to_owned(),
        id: DEFAULT_ID.to_owned(),
        remote: "origin".to_owned(),
        merge: "refs/heads/main".to_owned(),
        upstream: RefTip {
            name: "refs/remotes/origin/main".to_owned(),
            id: DEFAULT_UPSTREAM_ID.to_owned(),
        },
        ahead,
        behind,
        checked_out,
    }))
}

fn fast_forward() -> Action {
    Action::FastForward {
        branch: "feat".to_owned(),
        from: HEAD_ID.to_owned(),
        to: tip(),
    }
}

fn move_main() -> Action {
    Action::MoveDefaultBranch {
        branch: "main".to_owned(),
        remote: "origin".to_owned(),
        merge: "refs/heads/main".to_owned(),
        from: DEFAULT_ID.to_owned(),
        to: DEFAULT_UPSTREAM_ID.to_owned(),
    }
}

fn skip(reason: SkipReason) -> Action {
    Action::Skip(reason)
}

fn unknown(field: StateField) -> Action {
    skip(SkipReason::Unknown { field })
}

/// The action on `HEAD`'s branch with policy `ff`, for `state` and `refs`.
fn head(state: &RepoState, refs: &UpdateRefs) -> Action {
    let mut actions = decide_update(RepoUpdatePolicy::Ff, state, refs);
    assert_eq!(actions.len(), 1, "{actions:?}");
    actions.remove(0)
}

#[test]
fn a_clean_branch_behind_its_upstream_is_fast_forwarded_with_policy_ff_only() {
    assert_eq!(
        decide_update(RepoUpdatePolicy::Ff, &behind(), &refs()),
        vec![fast_forward()]
    );
    assert_eq!(
        decide_update(RepoUpdatePolicy::Fetch, &behind(), &refs()),
        vec![Action::FetchOnly]
    );
    assert_eq!(
        decide_update(RepoUpdatePolicy::Off, &behind(), &refs()),
        vec![skip(SkipReason::PolicyOff)]
    );
}

#[test]
fn operation_in_progress_or_unknown_blocks() {
    for operation in ALL_OPERATIONS {
        let state = RepoState {
            operation: MaybeKnown::Known(Some(operation)),
            ..behind()
        };
        assert_eq!(
            head(&state, &refs()),
            skip(SkipReason::Operation { operation }),
            "{operation}"
        );
    }
    let state = RepoState {
        operation: MaybeKnown::Unknown,
        ..behind()
    };
    assert_eq!(head(&state, &refs()), unknown(StateField::Operation));
}

#[test]
fn tracked_changes_or_unknown_dirty_block() {
    for paths in [1, 2, 1_000] {
        let state = RepoState {
            dirty: MaybeKnown::Known(paths),
            ..behind()
        };
        assert_eq!(head(&state, &refs()), skip(SkipReason::Dirty { paths }));
    }
    let state = RepoState {
        dirty: MaybeKnown::Unknown,
        ..behind()
    };
    assert_eq!(head(&state, &refs()), unknown(StateField::Dirty));
}

#[test]
fn untracked_files_are_left_to_git() {
    // `git merge --ff-only` refuses on its own when one would be overwritten.
    for untracked in [
        MaybeKnown::Known(Untracked {
            count: 3,
            capped: false,
        }),
        MaybeKnown::Known(Untracked {
            count: 1_000,
            capped: true,
        }),
        MaybeKnown::Unknown,
    ] {
        let state = RepoState {
            untracked,
            ..behind()
        };
        assert_eq!(head(&state, &refs()), fast_forward());
    }
}

#[test]
fn head_must_be_a_branch_with_commits() {
    for (head_state, expected) in [
        (
            MaybeKnown::Known(RepoHead::Detached {
                commit: "abc1234".to_owned(),
            }),
            skip(SkipReason::Detached),
        ),
        (
            MaybeKnown::Known(RepoHead::Unborn {
                name: "feat".to_owned(),
            }),
            skip(SkipReason::Unborn {
                branch: "feat".to_owned(),
            }),
        ),
        (MaybeKnown::Unknown, unknown(StateField::Head)),
    ] {
        // A detached or unknown HEAD has no upstream in the state; the head decides first anyway.
        for upstream_state in [
            behind().upstream,
            MaybeKnown::Known(None),
            MaybeKnown::Unknown,
        ] {
            let state = RepoState {
                head: head_state.clone(),
                upstream: upstream_state,
                ..behind()
            };
            assert_eq!(head(&state, &refs()), expected, "{head_state:?}");
        }
    }
}

#[test]
fn upstream_must_exist_and_be_known() {
    let state = RepoState {
        upstream: MaybeKnown::Known(None),
        ..behind()
    };
    assert_eq!(
        head(&state, &refs()),
        skip(SkipReason::NoUpstream {
            branch: "feat".to_owned()
        })
    );
    let state = RepoState {
        upstream: MaybeKnown::Unknown,
        ..behind()
    };
    assert_eq!(head(&state, &refs()), unknown(StateField::Upstream));
}

#[test]
fn ahead_and_behind_counts() {
    let known = MaybeKnown::Known;
    for (ahead, behind_count, expected) in [
        (known(0), known(1), fast_forward()),
        (known(0), known(250), fast_forward()),
        // Nothing incoming: nothing to move, nothing skipped.
        (known(0), known(0), Action::FetchOnly),
        (known(3), known(0), Action::FetchOnly),
        (MaybeKnown::Unknown, known(0), Action::FetchOnly),
        (
            known(1),
            known(2),
            skip(SkipReason::Diverged {
                branch: "feat".to_owned(),
                ahead: 1,
                behind: 2,
            }),
        ),
        (MaybeKnown::Unknown, known(2), unknown(StateField::Ahead)),
        (known(0), MaybeKnown::Unknown, unknown(StateField::Behind)),
        (
            MaybeKnown::Unknown,
            MaybeKnown::Unknown,
            unknown(StateField::Ahead),
        ),
    ] {
        let state = RepoState {
            upstream: upstream(ahead, behind_count),
            ..behind()
        };
        assert_eq!(
            head(&state, &refs()),
            expected,
            "{ahead:?} {behind_count:?}"
        );
    }
}

#[test]
fn nothing_incoming_is_never_a_skip() {
    let up_to_date = upstream(MaybeKnown::Known(0), MaybeKnown::Known(0));
    for state in [
        RepoState {
            dirty: MaybeKnown::Known(4),
            upstream: up_to_date.clone(),
            ..behind()
        },
        RepoState {
            dirty: MaybeKnown::Unknown,
            upstream: up_to_date.clone(),
            ..behind()
        },
        RepoState {
            operation: MaybeKnown::Known(Some(RepoOperation::Merge)),
            upstream: up_to_date,
            ..behind()
        },
    ] {
        assert_eq!(head(&state, &refs()), Action::FetchOnly);
    }
}

#[test]
fn the_first_failing_condition_is_the_reason() {
    // Operation, then tracked changes, then HEAD, then upstream, then counts.
    let state = RepoState {
        operation: MaybeKnown::Known(Some(RepoOperation::Rebase)),
        dirty: MaybeKnown::Known(2),
        head: MaybeKnown::Known(RepoHead::Detached {
            commit: "abc1234".to_owned(),
        }),
        upstream: MaybeKnown::Known(None),
        ..behind()
    };
    assert_eq!(
        head(&state, &refs()),
        skip(SkipReason::Operation {
            operation: RepoOperation::Rebase
        })
    );
    let state = RepoState {
        dirty: MaybeKnown::Known(2),
        upstream: upstream(MaybeKnown::Known(1), MaybeKnown::Known(1)),
        ..behind()
    };
    assert_eq!(head(&state, &refs()), skip(SkipReason::Dirty { paths: 2 }));
}

#[test]
fn commits_must_be_known_and_consistent() {
    for (head_id, upstream_tip, expected) in [
        (
            MaybeKnown::Unknown,
            MaybeKnown::Known(Some(tip())),
            unknown(StateField::Commits),
        ),
        (
            MaybeKnown::Known(None),
            MaybeKnown::Known(Some(tip())),
            unknown(StateField::Commits),
        ),
        (
            MaybeKnown::Known(Some(HEAD_ID.to_owned())),
            MaybeKnown::Unknown,
            unknown(StateField::Commits),
        ),
        (
            MaybeKnown::Known(Some(HEAD_ID.to_owned())),
            MaybeKnown::Known(None),
            skip(SkipReason::NoUpstream {
                branch: "feat".to_owned(),
            }),
        ),
    ] {
        let refs = UpdateRefs {
            head: head_id,
            upstream: upstream_tip,
            ..refs()
        };
        assert_eq!(head(&behind(), &refs), expected);
    }
}

#[test]
fn fields_the_decision_does_not_read_do_not_matter() {
    let state = RepoState {
        default_branch: MaybeKnown::Unknown,
        last_commit: MaybeKnown::Unknown,
        last_fetch: MaybeKnown::Unknown,
        error: Some("last_commit: x".to_owned()),
        ..behind()
    };
    let refs = UpdateRefs {
        error: Some("x".to_owned()),
        ..refs()
    };
    assert_eq!(head(&state, &refs), fast_forward());
}

#[test]
fn default_branch_moves_only_when_free_and_strictly_behind() {
    for (refs_default, expected) in [
        (default_branch(0, 3, false), Some(move_main())),
        (default_branch(0, 3, true), None),
        (default_branch(0, 0, false), None),
        (default_branch(2, 0, false), None),
        (
            default_branch(1, 3, false),
            Some(skip(SkipReason::DefaultBranchDiverged {
                branch: "main".to_owned(),
                ahead: 1,
                behind: 3,
            })),
        ),
        (default_branch(1, 3, true), None),
        (MaybeKnown::Known(None), None),
        (
            MaybeKnown::Unknown,
            Some(unknown(StateField::DefaultBranch)),
        ),
    ] {
        let refs = UpdateRefs {
            default_branch: refs_default.clone(),
            ..refs()
        };
        let mut actions = decide_update(RepoUpdatePolicy::Ff, &behind(), &refs);
        assert_eq!(actions.remove(0), fast_forward());
        assert_eq!(actions.pop(), expected, "{refs_default:?}");
        assert_eq!(actions, []);
    }
}

#[test]
fn default_branch_moves_whatever_the_checked_out_branch() {
    let refs = UpdateRefs {
        default_branch: default_branch(0, 1, false),
        ..refs()
    };
    let dirty = RepoState {
        dirty: MaybeKnown::Known(1),
        ..behind()
    };
    assert_eq!(
        decide_update(RepoUpdatePolicy::Ff, &dirty, &refs),
        vec![skip(SkipReason::Dirty { paths: 1 }), move_main()]
    );
    // Policy `fetch` never moves a branch.
    assert_eq!(
        decide_update(RepoUpdatePolicy::Fetch, &behind(), &refs),
        vec![Action::FetchOnly]
    );
    assert_eq!(
        decide_update(RepoUpdatePolicy::Off, &behind(), &refs),
        vec![skip(SkipReason::PolicyOff)]
    );
}

#[test]
fn fetch_needs_a_policy_a_remote_and_a_readable_repository() {
    let fetch = FetchDecision::Fetch {
        remote: "origin".to_owned(),
    };
    for policy in [RepoUpdatePolicy::Ff, RepoUpdatePolicy::Fetch] {
        assert_eq!(decide_fetch(policy, Some("origin"), &behind()), fetch);
        assert_eq!(
            decide_fetch(policy, None, &behind()),
            FetchDecision::Skip(SkipReason::NoRemote)
        );
        // Every other field may be unknown: the fetch changes no branch.
        let unknowns = RepoState {
            head: MaybeKnown::Unknown,
            dirty: MaybeKnown::Unknown,
            upstream: MaybeKnown::Unknown,
            ..behind()
        };
        assert_eq!(decide_fetch(policy, Some("origin"), &unknowns), fetch);
        let unreadable = crate::state::unreadable(Timestamp::EPOCH, "no HEAD".to_owned());
        assert_eq!(
            decide_fetch(policy, Some("origin"), &unreadable),
            FetchDecision::Skip(SkipReason::Unreadable {
                error: "no HEAD".to_owned()
            })
        );
    }
    assert_eq!(
        decide_fetch(RepoUpdatePolicy::Off, Some("origin"), &behind()),
        FetchDecision::Skip(SkipReason::PolicyOff)
    );
    assert_eq!(
        decide_fetch(RepoUpdatePolicy::Off, None, &behind()),
        FetchDecision::Skip(SkipReason::PolicyOff)
    );
}

#[test]
fn local_blockers_do_not_depend_on_the_fetch() {
    assert_eq!(local_blocker(&behind()), None);
    let no_upstream = RepoState {
        upstream: MaybeKnown::Known(None),
        ..behind()
    };
    assert_eq!(local_blocker(&no_upstream), None);
    let detached = RepoState {
        head: MaybeKnown::Known(RepoHead::Detached {
            commit: "abc1234".to_owned(),
        }),
        ..behind()
    };
    assert_eq!(local_blocker(&detached), Some(SkipReason::Detached));
}

#[test]
fn dry_run_plans() {
    let dirty = RepoState {
        dirty: MaybeKnown::Known(2),
        ..behind()
    };
    for (policy, remote, state, expected) in [
        (
            RepoUpdatePolicy::Ff,
            Some("origin"),
            behind(),
            vec![PlannedAction::Fetch, PlannedAction::DecideAfterFetch],
        ),
        (
            RepoUpdatePolicy::Ff,
            Some("origin"),
            dirty.clone(),
            vec![
                PlannedAction::Fetch,
                PlannedAction::Skip {
                    reason: SkipReason::Dirty { paths: 2 },
                },
            ],
        ),
        (
            RepoUpdatePolicy::Fetch,
            Some("origin"),
            dirty,
            vec![PlannedAction::Fetch],
        ),
        (
            RepoUpdatePolicy::Off,
            Some("origin"),
            behind(),
            vec![PlannedAction::Skip {
                reason: SkipReason::PolicyOff,
            }],
        ),
        (
            RepoUpdatePolicy::Ff,
            None,
            behind(),
            vec![PlannedAction::Skip {
                reason: SkipReason::NoRemote,
            }],
        ),
    ] {
        assert_eq!(plan(policy, remote, &state), expected, "{policy}");
    }
}

#[test]
fn policy_is_the_repository_then_the_context_then_ff() {
    let text = r#"
[contexts.personal]
match.paths = ["~/perso"]

[contexts.work]
match.paths = ["~/work"]
update = "fetch"

[repos."~/perso/frozen"]
update = "off"

[repos."~/work/mine"]
update = "ff"

[repos."~/elsewhere/loose"]
update = "ff"

[repos."~/elsewhere/quiet"]
update = "off"
"#;
    let config = parse_config(text, Path::new("/c/config.toml")).unwrap();
    let home = Path::new("/home/me");
    let policies = UpdatePolicies::new(&config, home);
    let path = |context: &str| ContextAssignment::Path {
        context: context.parse().unwrap(),
        prefix: format!("~/{context}"),
    };
    for (repo, context, expected) in [
        ("/home/me/perso/a", path("personal"), RepoUpdatePolicy::Ff),
        (
            "/home/me/perso/frozen",
            path("personal"),
            RepoUpdatePolicy::Off,
        ),
        ("/home/me/work/b", path("work"), RepoUpdatePolicy::Fetch),
        ("/home/me/work/mine", path("work"), RepoUpdatePolicy::Ff),
        // Unassigned: fetched only by default; an explicit repository policy wins.
        (
            "/home/me/other",
            ContextAssignment::Unassigned,
            RepoUpdatePolicy::Fetch,
        ),
        (
            "/home/me/elsewhere/loose",
            ContextAssignment::Unassigned,
            RepoUpdatePolicy::Ff,
        ),
        (
            "/home/me/elsewhere/quiet",
            ContextAssignment::Unassigned,
            RepoUpdatePolicy::Off,
        ),
    ] {
        assert_eq!(
            policies.policy(Path::new(repo), &context),
            expected,
            "{repo}"
        );
    }
    assert_eq!(
        UpdatePolicies::default().policy(Path::new("/x"), &path("personal")),
        RepoUpdatePolicy::Ff
    );
}
