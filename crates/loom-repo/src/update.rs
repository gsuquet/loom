//! Automatic updates (ADR-0012): which update policy applies to a repository, and what an
//! update does to it, decided from its state. Pure: the git commands are run by `loomd`.
//!
//! An update happens in two steps. Before anything runs, [`decide_fetch`] says whether the
//! primary remote is fetched at all. After the fetch, the state is read again and
//! [`decide_update`] says which branches move: the branch `HEAD` is on, fast-forwarded only when
//! every condition holds, and the local default branch, moved only when no working tree has it
//! checked out and it is strictly behind its upstream. Any field the decision needs that is
//! unknown means no move, never a guess.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use loom_api::{
    ContextAssignment, ContextName, MaybeKnown, PlannedAction, RepoHead, RepoState,
    RepoUpdatePolicy, SkipReason, StateField,
};
use loom_config::config::Config;

use crate::state::{RefTip, UpdateRefs};

/// Policy when neither the repository nor its context sets one.
pub const DEFAULT_POLICY: RepoUpdatePolicy = RepoUpdatePolicy::Ff;

/// The `update` keys of a configuration, with `~` expanded, ready to resolve policies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdatePolicies {
    /// `[repos."<path>"] update`, paths expanded.
    repos: Vec<(PathBuf, RepoUpdatePolicy)>,
    /// `[contexts.<name>] update`.
    contexts: BTreeMap<ContextName, RepoUpdatePolicy>,
}

impl UpdatePolicies {
    /// The policies of `config`, with `~` expanded to `home`. Pure.
    #[must_use]
    pub fn new(config: &Config, home: &Path) -> Self {
        Self {
            repos: config
                .repos
                .overrides
                .iter()
                .filter_map(|repo| {
                    repo.update
                        .map(|policy| (repo.path.get_ref().expand(home), policy))
                })
                .collect(),
            contexts: config
                .contexts
                .iter()
                .filter_map(|(name, context)| context.update.map(|policy| (name.clone(), policy)))
                .collect(),
        }
    }

    /// Policy of the repository at `path` (absolute, as found) assigned to `context`:
    /// `[repos."<path>"] update`, otherwise the context's `update`, otherwise
    /// [`DEFAULT_POLICY`]. An unassigned repository is never fast-forwarded: `ff` becomes
    /// `fetch` (`off` stays `off`). Pure.
    #[must_use]
    pub fn policy(&self, path: &Path, context: &ContextAssignment) -> RepoUpdatePolicy {
        let configured = self
            .repos
            .iter()
            .find(|(repo, _)| repo == path)
            .map(|(_, policy)| *policy)
            .or_else(|| {
                context
                    .context()
                    .and_then(|name| self.contexts.get(name).copied())
            })
            .unwrap_or(DEFAULT_POLICY);
        match (context, configured) {
            (ContextAssignment::Unassigned, RepoUpdatePolicy::Ff) => RepoUpdatePolicy::Fetch,
            (_, policy) => policy,
        }
    }
}

/// Whether an update fetches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchDecision {
    /// No git command runs.
    Skip(SkipReason),
    /// `git fetch` of this remote, then [`decide_update`].
    Fetch {
        /// Name of the primary remote.
        remote: String,
    },
}

/// Whether to fetch the repository whose primary remote is `primary_remote` and whose state,
/// read just before, is `state`. Pure.
///
/// Policy `off` and a repository without remote are skipped; so is a repository that cannot be
/// opened (its `operation` is unknown only then), since git could not fetch it either.
#[must_use]
pub fn decide_fetch(
    policy: RepoUpdatePolicy,
    primary_remote: Option<&str>,
    state: &RepoState,
) -> FetchDecision {
    match policy {
        RepoUpdatePolicy::Off => return FetchDecision::Skip(SkipReason::PolicyOff),
        RepoUpdatePolicy::Ff | RepoUpdatePolicy::Fetch => {}
    }
    let Some(remote) = primary_remote else {
        return FetchDecision::Skip(SkipReason::NoRemote);
    };
    if state.operation == MaybeKnown::Unknown {
        return FetchDecision::Skip(SkipReason::Unreadable {
            error: state
                .error
                .clone()
                .unwrap_or_else(|| "cannot open the repository".to_owned()),
        });
    }
    FetchDecision::Fetch {
        remote: remote.to_owned(),
    }
}

/// A step of an update after the fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// The step is not done.
    Skip(SkipReason),
    /// Nothing moves: the fetch was all (policy `fetch`, or nothing new for `HEAD`'s branch).
    FetchOnly,
    /// `git merge --ff-only <to.id>` on the branch `HEAD` is on.
    FastForward {
        /// Short branch name.
        branch: String,
        /// Commit of `HEAD`.
        from: String,
        /// The upstream and the commit to fast-forward to.
        to: RefTip,
    },
    /// `git fetch <remote> <merge>:refs/heads/<branch>` of the default branch.
    MoveDefaultBranch {
        /// Short branch name.
        branch: String,
        /// Its remote, the primary one.
        remote: String,
        /// Its branch on the remote, full name.
        merge: String,
        /// Its commit.
        from: String,
        /// The commit of its remote-tracking ref.
        to: String,
    },
}

/// What to do after the fetch, from the state and refs read after it. Pure.
///
/// The first action is about the branch `HEAD` is on; a second one, when present, about the
/// default branch. Policy `fetch` gives [`Action::FetchOnly`]; policy `off` never gets here
/// ([`decide_fetch`]) and gives a skip.
///
/// With policy `ff`, the branch `HEAD` is on is fast-forwarded only when all of these hold, the
/// first that fails being the reason of the skip: no operation in progress, no tracked change,
/// `HEAD` on a branch with commits, an upstream whose ref exists, the branch not ahead of it,
/// and every count and commit known. When the upstream is known not to be ahead, nothing has to
/// move and nothing is skipped. The default branch is moved when it exists locally with an
/// upstream on the primary remote, is checked out in no working tree, and is behind and not
/// ahead; diverged or unknown, the skip is reported; otherwise nothing is said about it.
#[must_use]
pub fn decide_update(
    policy: RepoUpdatePolicy,
    state: &RepoState,
    refs: &UpdateRefs,
) -> Vec<Action> {
    match policy {
        RepoUpdatePolicy::Off => vec![Action::Skip(SkipReason::PolicyOff)],
        RepoUpdatePolicy::Fetch => vec![Action::FetchOnly],
        RepoUpdatePolicy::Ff => {
            let mut actions = vec![head_action(state, refs)];
            actions.extend(default_branch_action(refs));
            actions
        }
    }
}

/// The action on the branch `HEAD` is on, policy `ff`.
fn head_action(state: &RepoState, refs: &UpdateRefs) -> Action {
    // Nothing incoming: nothing to do, whatever the rest of the state.
    if let MaybeKnown::Known(Some(upstream)) = &state.upstream
        && upstream.behind == MaybeKnown::Known(0)
    {
        return Action::FetchOnly;
    }
    match fast_forward(state, refs) {
        Ok(action) => action,
        Err(reason) => Action::Skip(reason),
    }
}

/// The fast-forward of the branch `HEAD` is on, or why not.
fn fast_forward(state: &RepoState, refs: &UpdateRefs) -> Result<Action, SkipReason> {
    let unknown = |field| SkipReason::Unknown { field };
    local_blocker(state).map_or(Ok(()), Err)?;
    let branch = match state.head.as_known() {
        Some(RepoHead::Branch { name }) => name,
        // Reported by `local_blocker`.
        Some(RepoHead::Unborn { .. } | RepoHead::Detached { .. }) | None => {
            return Err(unknown(StateField::Head));
        }
    };
    let upstream = match &state.upstream {
        MaybeKnown::Unknown => return Err(unknown(StateField::Upstream)),
        MaybeKnown::Known(None) => {
            return Err(SkipReason::NoUpstream {
                branch: branch.clone(),
            });
        }
        MaybeKnown::Known(Some(upstream)) => upstream,
    };
    let ahead = upstream
        .ahead
        .known()
        .ok_or_else(|| unknown(StateField::Ahead))?;
    let behind = upstream
        .behind
        .known()
        .ok_or_else(|| unknown(StateField::Behind))?;
    if behind == 0 {
        return Ok(Action::FetchOnly);
    }
    if ahead > 0 {
        return Err(SkipReason::Diverged {
            branch: branch.clone(),
            ahead,
            behind,
        });
    }
    let from = match &refs.head {
        MaybeKnown::Known(Some(id)) => id,
        // The branch got its first commit or lost it between the two reads: decide next time.
        MaybeKnown::Known(None) | MaybeKnown::Unknown => return Err(unknown(StateField::Commits)),
    };
    let to = match &refs.upstream {
        MaybeKnown::Known(Some(tip)) => tip,
        MaybeKnown::Known(None) => {
            return Err(SkipReason::NoUpstream {
                branch: branch.clone(),
            });
        }
        MaybeKnown::Unknown => return Err(unknown(StateField::Commits)),
    };
    Ok(Action::FastForward {
        branch: branch.clone(),
        from: from.clone(),
        to: to.clone(),
    })
}

/// The first condition of the working tree that forbids a fast-forward, whatever the fetch.
///
/// An operation in progress, a tracked change, `HEAD` not on a branch with commits, or one of
/// these unknown. Pure.
#[must_use]
pub fn local_blocker(state: &RepoState) -> Option<SkipReason> {
    let unknown = |field| Some(SkipReason::Unknown { field });
    match state.operation {
        MaybeKnown::Unknown => return unknown(StateField::Operation),
        MaybeKnown::Known(Some(operation)) => return Some(SkipReason::Operation { operation }),
        MaybeKnown::Known(None) => {}
    }
    match state.dirty {
        MaybeKnown::Unknown => return unknown(StateField::Dirty),
        MaybeKnown::Known(0) => {}
        MaybeKnown::Known(paths) => return Some(SkipReason::Dirty { paths }),
    }
    match &state.head {
        MaybeKnown::Unknown => unknown(StateField::Head),
        MaybeKnown::Known(RepoHead::Branch { .. }) => None,
        MaybeKnown::Known(RepoHead::Unborn { name }) => Some(SkipReason::Unborn {
            branch: name.clone(),
        }),
        MaybeKnown::Known(RepoHead::Detached { .. }) => Some(SkipReason::Detached),
    }
}

/// The action on the default branch, policy `ff`; `None` when there is nothing to say.
fn default_branch_action(refs: &UpdateRefs) -> Option<Action> {
    let branch = match &refs.default_branch {
        MaybeKnown::Unknown => {
            return Some(Action::Skip(SkipReason::Unknown {
                field: StateField::DefaultBranch,
            }));
        }
        MaybeKnown::Known(None) => return None,
        MaybeKnown::Known(Some(branch)) => branch,
    };
    // Checked out: the update of that working tree moves it, under the conditions above.
    if branch.checked_out || branch.behind == 0 {
        return None;
    }
    if branch.ahead > 0 {
        return Some(Action::Skip(SkipReason::DefaultBranchDiverged {
            branch: branch.branch.clone(),
            ahead: branch.ahead,
            behind: branch.behind,
        }));
    }
    Some(Action::MoveDefaultBranch {
        branch: branch.branch.clone(),
        remote: branch.remote.clone(),
        merge: branch.merge.clone(),
        from: branch.id.clone(),
        to: branch.upstream.id.clone(),
    })
}

/// What an update would do, from the current `state`, without fetching: the dry run of
/// `loom repo sync -n`. Pure.
///
/// Branch moves depend on what the fetch brings, so with policy `ff` they are
/// [`PlannedAction::DecideAfterFetch`], unless the working tree already forbids a fast-forward
/// of `HEAD`'s branch ([`local_blocker`]): that skip is planned instead. The default branch is
/// never predicted.
#[must_use]
pub fn plan(
    policy: RepoUpdatePolicy,
    primary_remote: Option<&str>,
    state: &RepoState,
) -> Vec<PlannedAction> {
    match decide_fetch(policy, primary_remote, state) {
        FetchDecision::Skip(reason) => vec![PlannedAction::Skip { reason }],
        FetchDecision::Fetch { .. } => match policy {
            RepoUpdatePolicy::Fetch | RepoUpdatePolicy::Off => vec![PlannedAction::Fetch],
            RepoUpdatePolicy::Ff => vec![
                PlannedAction::Fetch,
                local_blocker(state).map_or(PlannedAction::DecideAfterFetch, |reason| {
                    PlannedAction::Skip { reason }
                }),
            ],
        },
    }
}

#[cfg(test)]
mod tests;
