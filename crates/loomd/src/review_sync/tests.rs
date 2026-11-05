use std::path::PathBuf;
use std::time::Duration;

use loom_api::{ContextAssignment, ForgeKind, Remote, RemoteUrl, RepoIdentity, RepoSummary};
use loom_core::{Kind, Link, StatusName};
use loom_forge::github::{Review, ReviewState, parse_pull, parse_reviews};
use loom_forge::gitlab::parse_versions;
use loom_forge::model::{MrKey, MrState, MrVersion, Sha};

use super::*;
use crate::mr_refs::DiffKind;

const V1: &str = "7ebf283bfee25ab48f95c0c55181a4ca66b4c990";
const V2: &str = "84280c47b6aa128c645b1fbea54aded8b32e8463";

fn fixture(path: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/forge")
        .join(path);
    std::fs::read(path).unwrap()
}

fn sha(text: &str) -> Sha {
    Sha::try_from(text.to_owned()).unwrap()
}

fn at(secs: u64) -> Timestamp {
    Timestamp::from_unix_secs(secs).unwrap()
}

/// An open MR on V2, seen before on V2, where I am a requested reviewer at both polls.
fn seen() -> Observation {
    Observation {
        state: MrState::Open,
        head: sha(V2),
        previous_head: Some(sha(V2)),
        requested: true,
        was_requested: Some(true),
        verdict: None,
        new_verdict: false,
        previous_state: Some(MrState::Open),
    }
}

fn set(status: StatusName, reason: Reason) -> Decision {
    Decision::SetStatus { status, reason }
}

#[test]
#[expect(clippy::too_many_lines, reason = "one row per rule")]
fn decision_table() {
    use StatusName::{Active, Cancelled, Done, Inbox, Ready, Waiting};
    let new_head = Reason::NewHead {
        from: sha(V1),
        to: sha(V2),
    };
    let after_review_on_v1 = Observation {
        previous_head: Some(sha(V1)),
        // GitHub drops me from the requested reviewers once I review.
        requested: false,
        was_requested: Some(false),
        verdict: Some(sha(V1)),
        ..seen()
    };
    let verdict_now = Observation {
        verdict: Some(sha(V2)),
        new_verdict: true,
        ..seen()
    };
    let removed = Observation {
        requested: false,
        ..seen()
    };
    let requested_again = Observation {
        was_requested: Some(false),
        verdict: Some(sha(V2)),
        ..seen()
    };
    let merged = Observation {
        state: MrState::Merged,
        ..seen()
    };
    let closed = Observation {
        state: MrState::Closed,
        ..seen()
    };
    // (task status, observation, decision)
    let table: Vec<(Option<StatusName>, Observation, Decision)> = vec![
        // Nothing new.
        (Some(Inbox), seen(), Decision::Keep),
        (Some(Done), seen(), Decision::Keep),
        (Some(Cancelled), seen(), Decision::Keep),
        // My verdict on the current head closes an open task.
        (Some(Inbox), verdict_now.clone(), set(Done, Reason::Verdict)),
        (
            Some(Waiting),
            verdict_now.clone(),
            set(Done, Reason::Verdict),
        ),
        (Some(Done), verdict_now, Decision::Keep),
        // A new head after the verdict reopens, even when the forge dropped my request.
        (Some(Done), after_review_on_v1.clone(), set(Ready, new_head)),
        // A head that the mirror missed (its write failed), with my verdict on it: still done.
        (
            Some(Done),
            Observation {
                previous_head: Some(sha(V1)),
                verdict: Some(sha(V2)),
                ..seen()
            },
            Decision::Keep,
        ),
        // A task that I closed by hand reopens on a new head.
        (
            Some(Done),
            Observation {
                previous_head: Some(sha(V1)),
                ..seen()
            },
            set(
                Ready,
                Reason::NewHead {
                    from: sha(V1),
                    to: sha(V2),
                },
            ),
        ),
        // An MR reopened on the forge reopens its cancelled task while I am requested.
        (
            Some(Cancelled),
            Observation {
                previous_state: Some(MrState::Closed),
                ..seen()
            },
            set(Ready, Reason::Reopened),
        ),
        (
            Some(Cancelled),
            Observation {
                previous_state: Some(MrState::Closed),
                requested: false,
                was_requested: Some(false),
                ..seen()
            },
            Decision::Keep,
        ),
        // A new head of an open task changes nothing; of a cancelled one either.
        (Some(Ready), after_review_on_v1.clone(), Decision::Keep),
        (Some(Cancelled), after_review_on_v1, Decision::Keep),
        // Request removed: cancelled, or done after a verdict.
        (
            Some(Active),
            removed.clone(),
            set(Cancelled, Reason::RequestRemoved),
        ),
        (
            Some(Ready),
            Observation {
                verdict: Some(sha(V2)),
                ..removed.clone()
            },
            set(Done, Reason::RequestRemoved),
        ),
        // Request removed after a verdict on an older head (GitHub drops me when I review, and
        // the author pushed before the poll): the new head is still to review.
        (
            Some(Ready),
            Observation {
                previous_head: Some(sha(V1)),
                verdict: Some(sha(V1)),
                ..removed.clone()
            },
            Decision::Keep,
        ),
        // Not requested at either poll: no change (the task reopened by a new head stays).
        (
            Some(Ready),
            Observation {
                was_requested: Some(false),
                ..removed.clone()
            },
            Decision::Keep,
        ),
        // Without a sync state, no edge.
        (
            Some(Ready),
            Observation {
                was_requested: None,
                ..removed
            },
            Decision::Keep,
        ),
        // Requested again.
        (
            Some(Done),
            requested_again.clone(),
            set(Ready, Reason::RequestedAgain),
        ),
        (
            Some(Cancelled),
            requested_again.clone(),
            set(Ready, Reason::RequestedAgain),
        ),
        (Some(Ready), requested_again, Decision::Keep),
        // A terminal remote state closes an open task, and leaves a closed one.
        (Some(Ready), merged.clone(), set(Done, Reason::Merged)),
        (Some(Inbox), closed.clone(), set(Cancelled, Reason::Closed)),
        (Some(Cancelled), merged.clone(), Decision::Keep),
        // Closed on the forge at the last poll already: a task that I reopened by hand stays.
        (
            Some(Ready),
            Observation {
                previous_state: Some(MrState::Merged),
                ..merged
            },
            Decision::Keep,
        ),
        (
            Some(Active),
            Observation {
                previous_state: Some(MrState::Closed),
                ..closed.clone()
            },
            Decision::Keep,
        ),
        // Without a mirror, a merged MR closes its open task.
        (
            Some(Inbox),
            Observation {
                previous_state: None,
                state: MrState::Merged,
                ..seen()
            },
            set(Done, Reason::Merged),
        ),
        (Some(Done), closed, Decision::Keep),
        // A status of a newer loom: read-only.
        (None, seen(), Decision::Keep),
    ];
    for (task, observation, expected) in table {
        assert_eq!(
            decide(task, &observation),
            expected,
            "{task:?} {observation:?}"
        );
    }
}

fn review(id: &str, author: &str, state: ReviewState, commit: &str, at_secs: u64) -> Review {
    Review {
        id: id.to_owned(),
        author: author.to_owned(),
        state,
        body: String::new(),
        commit_sha: Some(sha(commit)),
        submitted_at: (state != ReviewState::Pending).then(|| at(at_secs)),
    }
}

#[test]
fn my_last_submitted_review_is_my_github_verdict() {
    // The fixture has bob's review only.
    let reviews = parse_reviews(&fixture("github/pull-412-reviews.json")).unwrap();
    assert_eq!(github_verdict(&reviews, "carol"), None);
    assert_eq!(
        github_verdict(&reviews, "bob").map(|verdict| verdict.sha),
        Some(sha(V1))
    );
    let reviews = vec![
        review("R1", "carol", ReviewState::ChangesRequested, V1, 10),
        review("R2", "Carol", ReviewState::Commented, V2, 20),
        review("R3", "carol", ReviewState::Pending, V2, 30),
        review("R4", "carol", ReviewState::Dismissed, V2, 40),
        review("R5", "bob", ReviewState::Approved, V2, 50),
    ];
    assert_eq!(
        github_verdict(&reviews, "carol"),
        Some(SeenVerdict {
            sha: sha(V2),
            id: "R2".to_owned(),
        })
    );
}

#[test]
fn a_verdict_is_new_once_and_a_reset_approval_stays_remembered() {
    let on_v1 = gitlab_verdict(true, &sha(V1));
    assert_eq!(gitlab_verdict(false, &sha(V1)), None);
    let (recorded, new) = next_verdict(None, false, on_v1.clone());
    assert!(new);
    assert_eq!(recorded, on_v1);
    // Seen again: not new.
    assert_eq!(
        next_verdict(recorded.as_ref(), true, on_v1.clone()),
        (on_v1.clone(), false)
    );
    // The approval is reset by a push: still remembered.
    assert_eq!(next_verdict(recorded.as_ref(), true, None), (on_v1, false));
    // Approved again on V2, after the reset: new.
    let on_v2 = gitlab_verdict(true, &sha(V2));
    assert_eq!(
        next_verdict(recorded.as_ref(), false, on_v2.clone()),
        (on_v2, true)
    );
}

#[test]
fn a_gitlab_approval_kept_across_pushes_is_not_a_new_verdict() {
    // A project that does not reset approvals: approved on V1, then pushes to V2 and V3.
    let v3 = "0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e";
    let on_v1 = gitlab_verdict(true, &sha(V1));
    let (recorded, _) = next_verdict(None, false, on_v1.clone());
    for head in [V2, v3] {
        assert_eq!(
            next_verdict(recorded.as_ref(), true, gitlab_verdict(true, &sha(head))),
            (on_v1.clone(), false),
            "{head}"
        );
    }
}

#[test]
fn a_new_github_review_is_new_even_while_the_last_one_is_shown() {
    let first = Some(SeenVerdict {
        sha: sha(V1),
        id: "R1".to_owned(),
    });
    let second = Some(SeenVerdict {
        sha: sha(V2),
        id: "R2".to_owned(),
    });
    assert_eq!(
        next_verdict(first.as_ref(), true, second.clone()),
        (second, true)
    );
}

#[test]
fn versions_are_kept_oldest_first_and_completed() {
    let gitlab = parse_versions(&fixture("gitlab/merge-request-88-versions.json")).unwrap();
    assert_eq!(gitlab.len(), 2);
    let versions = merge_versions(&[], &gitlab, MrState::Open, at(100));
    let heads: Vec<&str> = versions
        .iter()
        .map(|record| record.version.head_sha.as_str())
        .collect();
    assert_eq!(heads, [V1, V2]);
    assert!(versions.iter().all(|record| record.first_seen == at(100)));
    // Seen again later: unchanged.
    assert_eq!(
        merge_versions(&versions, &gitlab, MrState::Open, at(200)),
        versions
    );

    // GitHub: one version per poll, without a merge base.
    let v1 = parse_pull(&fixture("github/pull-412.v1.json"))
        .unwrap()
        .version;
    let v2 = parse_pull(&fixture("github/pull-412.v2.json"))
        .unwrap()
        .version;
    let first = merge_versions(&[], std::slice::from_ref(&v1), MrState::Open, at(100));
    let both = merge_versions(&first, std::slice::from_ref(&v2), MrState::Open, at(200));
    assert_eq!(both.len(), 2);
    assert_eq!(both[0].first_seen, at(100));
    assert_eq!(both[1].version.head_sha, sha(V2));
    assert_eq!(both[1].first_seen, at(200));

    // A merge base learned later fills the known version.
    let mut bare = gitlab[1].clone();
    bare.merge_base = None;
    bare.forge_version_id = None;
    let known = merge_versions(&[], &[bare], MrState::Open, at(100));
    let completed = merge_versions(&known, &gitlab, MrState::Open, at(200));
    assert_eq!(completed[0].version, gitlab[1]);
    assert_eq!(completed[0].first_seen, at(100));
}

#[test]
fn a_review_task_links_its_mr_and_is_found_by_it() {
    let mr = parse_pull(&fixture("github/pull-412.v2.json")).unwrap();
    let context: ContextName = "company-a".parse().unwrap();
    let new = new_review_task(&mr, &context);
    assert_eq!(new.title, "Review acme/shop#412: Auth refactor");
    assert_eq!(new.kind, Kind::Review);
    assert_eq!(new.status, Status::Active);
    assert_eq!(new.context, context);
    let (key, host) = review_key(&new.kind, &new.links).unwrap();
    assert_eq!(key, mr.key);
    assert_eq!(host.as_deref(), Some("github.com"));
    // Another kind, or a link of another provider, is not a Review task of an MR.
    assert_eq!(review_key(&Kind::Story, &new.links), None);
    let jira = vec![Link::Origin(loom_core::ExternalRef {
        provider: "jira".to_owned(),
        key: "acme/shop#412".to_owned(),
        url: None,
    })];
    assert_eq!(review_key(&Kind::Review, &jira), None);
    assert_eq!(
        url_host("https://user@GitLab.Example.com:8443/a/b/-/merge_requests/1").as_deref(),
        Some("gitlab.example.com")
    );
    assert_eq!(url_host("no-scheme"), None);
}

fn repo(path: &str, remote: &str, last_used: Option<u64>) -> RepoSummary {
    let (host, project) = remote.split_once('/').unwrap();
    RepoSummary {
        identity: RepoIdentity {
            path: PathBuf::from(path),
            name: "shop".to_owned(),
            slug: format!("company-a/{path}"),
            context: ContextAssignment::Unassigned,
            remotes: vec![Remote {
                name: "upstream".to_owned(),
                url: format!("git@{host}:{project}.git"),
                location: Some(RemoteUrl {
                    host: host.to_owned(),
                    port: None,
                    path: project.to_owned(),
                }),
            }],
            primary_remote: Some("upstream".to_owned()),
            forge: Some(ForgeKind::Github),
        },
        registered: false,
        last_used: last_used.map(at),
        state: None,
        last_update: None,
        forge_metadata: None,
    }
}

#[test]
fn the_clone_of_a_project_is_the_most_recently_used_then_the_shortest_path() {
    let repos = vec![
        repo("/work/other", "github.com/acme/other", Some(90)),
        repo("/work/shop-long", "github.com/acme/shop", None),
        repo("/work/shop", "github.com/Acme/Shop", None),
    ];
    let clone = |repos: &[RepoSummary]| {
        clone_of(repos, "github.com", "acme/shop").map(|repo| repo.identity.path.clone())
    };
    assert_eq!(clone(&repos), Some(PathBuf::from("/work/shop")));
    let mut used = repos.clone();
    used.push(repo("/work/a/b/shop", "github.com/acme/shop", Some(10)));
    assert_eq!(clone(&used), Some(PathBuf::from("/work/a/b/shop")));
    assert_eq!(clone_of(&repos, "gitlab.com", "acme/shop"), None);
}

#[test]
fn the_mirror_round_trips_and_keeps_the_description_apart() {
    let mr = parse_pull(&fixture("github/pull-412.v2.json")).unwrap();
    let versions = merge_versions(
        &[],
        std::slice::from_ref(&mr.version),
        MrState::Open,
        at(100),
    );
    let state = SyncState {
        v: SyncState::VERSION,
        requested: true,
        verdict: None,
        shown: false,
    };
    let files = mirror(&mr, &versions, None, &state).unwrap();
    let snapshot: serde_json::Value = serde_json::from_slice(&files.mr).unwrap();
    assert!(snapshot.get("description").is_none());
    assert_eq!(files.description, mr.description.as_bytes());
    assert_eq!(files.threads, None);
    let names: Vec<&str> = files.files().iter().map(|(name, _)| *name).collect();
    assert_eq!(names, [MR_FILE, DESCRIPTION_FILE, VERSIONS_FILE, SYNC_FILE]);
    assert_eq!(read_mr(&files.mr, &mr.description), Some(mr.clone()));
    assert_eq!(read_mr(b"{", ""), None);
    // The same input gives the same bytes: an unchanged MR writes nothing.
    assert_eq!(mirror(&mr, &versions, None, &state).unwrap(), files);
    let read: SyncState = serde_json::from_slice(&files.sync).unwrap();
    assert_eq!(read, state);

    let key = MrKey::parse("acme/shop#412").unwrap();
    assert_eq!(
        mirror_dir(Path::new("/s/tasks/k3q7zm2a"), &key),
        Path::new("/s/tasks/k3q7zm2a/mrs/github-acme%2Fshop%23412")
    );
}

#[test]
fn polls_are_jittered_and_back_off_after_failures() {
    let interval = Duration::from_mins(5);
    // `random` 0.5: no jitter.
    assert_eq!(next_poll(interval, PollOutcome::Done, 0.5), interval);
    assert_eq!(
        next_poll(interval, PollOutcome::Done, 1.0),
        interval.mul_f64(1.1)
    );
    assert_eq!(
        next_poll(interval, PollOutcome::Failed { failures: 2 }, 0.5),
        interval * 4
    );
    assert_eq!(
        next_poll(
            interval,
            PollOutcome::RateLimited(Duration::from_mins(20)),
            0.5
        ),
        Duration::from_mins(20)
    );
    assert_eq!(
        next_poll(
            interval,
            PollOutcome::RateLimited(Duration::from_secs(5)),
            0.5
        ),
        interval
    );
}

#[test]
fn notes_name_the_mr_and_the_heads() {
    let key = MrKey::parse("acme/shop#412").unwrap();
    let reason = Reason::NewHead {
        from: sha(V1),
        to: sha(V2),
    };
    assert_eq!(
        reason.note(&key),
        "MR acme/shop#412: new head 84280c4 after 7ebf283, reopened"
    );
    assert_eq!(Reason::Merged.note(&key), "MR acme/shop#412: merged");
}

#[test]
fn a_host_that_several_contexts_poll_does_not_choose_the_context_of_an_mr() {
    let here: ContextName = "personal".parse().unwrap();
    let other: ContextName = "company-a".parse().unwrap();
    let finder = loom_forge::MrContext::Finder {
        context: here.clone(),
    };
    assert_eq!(belongs(&finder, &here, false), Belongs::Here);
    // Every context that polls the host can be the finder: none of them is the context.
    assert_eq!(belongs(&finder, &here, true), Belongs::Unassigned);
    let rule = loom_forge::MrContext::Host {
        context: here.clone(),
        pattern: "github.com/acme".to_owned(),
    };
    assert_eq!(belongs(&rule, &here, true), Belongs::Here);
    assert_eq!(belongs(&rule, &other, true), Belongs::Elsewhere);
    let clone = loom_forge::MrContext::Clone { context: other };
    assert_eq!(belongs(&clone, &here, false), Belongs::Elsewhere);
}

#[test]
fn the_finder_tells_the_context_only_for_tokens_of_distinct_accounts() {
    // The only context on the host.
    assert!(finder_tells(Some("me"), 0, &[]));
    assert!(finder_tells(None, 0, &[]));
    // Another account.
    assert!(finder_tells(Some("me"), 1, &[Some("me-at-client")]));
    // One token, or two tokens of one account.
    assert!(!finder_tells(Some("me"), 1, &[Some("ME")]));
    // A login not read yet, or a peer that another machine polls.
    assert!(!finder_tells(Some("me"), 1, &[None]));
    assert!(!finder_tells(Some("me"), 2, &[Some("me-at-client")]));
    assert!(!finder_tells(None, 1, &[Some("me-at-client")]));
}

fn base_version(head: &str, base: &str, merge_base: Option<&str>) -> MrVersion {
    MrVersion {
        head_sha: sha(head),
        base_sha: sha(base),
        merge_base: merge_base.map(sha),
        forge_version_id: None,
        created_at: None,
    }
}

#[test]
fn a_moved_base_tip_with_the_same_head_changes_nothing() {
    let mut known = merge_versions(&[], &[base_version(V1, V2, Some(V2))], MrState::Open, at(1));
    known[0].diff = Some(DiffKind::Local);
    // The same base: nothing changes.
    assert_eq!(
        merge_versions(&known, &[base_version(V1, V2, None)], MrState::Open, at(2)),
        known
    );
    // GitHub moves `base.sha` with the tip of the target branch: no push, no retarget.
    assert_eq!(
        merge_versions(&known, &[base_version(V1, V1, None)], MrState::Open, at(3)),
        known
    );
}

#[test]
fn a_new_target_branch_asks_for_a_new_diff_of_the_current_head() {
    let mut known = merge_versions(
        &[],
        &[
            base_version(V2, V2, Some(V2)),
            base_version(V1, V2, Some(V2)),
        ],
        MrState::Open,
        at(1),
    );
    for record in &mut known {
        record.diff = Some(DiffKind::Local);
    }
    let moved = apply_retarget(known.clone(), &base_version(V1, V1, None));
    let current = moved
        .iter()
        .find(|record| record.version.head_sha == sha(V1))
        .unwrap();
    assert_eq!(current.version.base_sha, sha(V1));
    assert_eq!(current.version.merge_base, None);
    assert_eq!(current.diff, None);
    assert_eq!(current.first_seen, at(1));
    // Another version keeps its diff.
    let other = moved
        .iter()
        .find(|record| record.version.head_sha == sha(V2))
        .unwrap();
    assert_eq!(other.diff, Some(DiffKind::Local));
}

#[test]
fn a_retarget_is_a_new_base_ref_of_an_open_mr() {
    let mr = parse_pull(&fixture("github/pull-412.v1.json")).unwrap();
    assert!(!is_retarget(None, &mr));
    assert!(!is_retarget(Some(&mr), &mr));
    // The tip moved: the same branch.
    let mut moved = mr.clone();
    moved.version.base_sha = sha(V1);
    assert!(!is_retarget(Some(&mr), &moved));
    let mut other = mr.clone();
    other.base_ref = "release".to_owned();
    assert!(is_retarget(Some(&mr), &other));
    other.state = MrState::Merged;
    assert!(!is_retarget(Some(&mr), &other));
    other.state = MrState::Closed;
    assert!(!is_retarget(Some(&mr), &other));
}

#[test]
fn a_new_gitlab_version_of_one_head_with_another_base_is_its_own_record() {
    let version = |id: u64, base: &str| MrVersion {
        forge_version_id: Some(id),
        created_at: Some(at(id)),
        ..base_version(V1, base, Some(base))
    };
    let mut known = merge_versions(&[], &[version(1, V2)], MrState::Open, at(1));
    known[0].diff = Some(DiffKind::Local);
    // The same version: nothing, whatever the rest says.
    assert_eq!(
        merge_versions(&known, &[version(1, V2)], MrState::Open, at(2)),
        known
    );
    // GitLab made a new version: its base and merge base are the ones of that version.
    let moved = merge_versions(
        &known,
        &[version(1, V2), version(2, V1)],
        MrState::Open,
        at(3),
    );
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].version.base_sha, sha(V1));
    assert_eq!(moved[0].version.merge_base, Some(sha(V1)));
    assert_eq!(moved[0].version.forge_version_id, Some(2));
    assert_eq!(moved[0].diff, None);
    // A new version of one base keeps the diff.
    let same_base = merge_versions(&known, &[version(2, V2)], MrState::Open, at(3));
    assert_eq!(same_base[0].diff, Some(DiffKind::Local));
    // A merged MR keeps the diff of its last base.
    assert_eq!(
        merge_versions(&known, &[version(2, V1)], MrState::Merged, at(3)),
        known
    );
}

#[test]
fn two_forge_versions_of_one_head_count_as_the_newest_one() {
    let older = MrVersion {
        created_at: Some(at(10)),
        ..base_version(V1, V2, Some(V2))
    };
    let newer = MrVersion {
        created_at: Some(at(20)),
        ..base_version(V1, V1, Some(V1))
    };
    let seen = [newer, older];
    let first = merge_versions(&[], &seen, MrState::Open, at(1));
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].version.base_sha, sha(V1));
    let mut done = first;
    done[0].diff = Some(DiffKind::Local);
    // A poll that lists both again keeps the record of the diff.
    assert_eq!(merge_versions(&done, &seen, MrState::Open, at(2)), done);
}

#[test]
fn the_current_version_is_added_when_the_forge_list_lacks_it() {
    let known = merge_versions(&[], &[base_version(V1, V2, None)], MrState::Open, at(1));
    // GitLab lists the versions after a background job: the head comes first.
    let added = merge_current(known.clone(), &base_version(V2, V1, Some(V1)), at(2));
    assert_eq!(added.len(), 2);
    assert_eq!(added[1].version.head_sha, sha(V2));
    assert_eq!(added[1].first_seen, at(2));
    // Known: the list decides, so the two sources do not undo each other.
    assert_eq!(
        merge_current(known.clone(), &base_version(V1, V1, Some(V1)), at(3)),
        known
    );
}

#[test]
fn the_snapshot_of_an_mr_shows_the_base_that_its_head_has_in_the_versions() {
    let mr = parse_pull(&fixture("github/pull-412.v1.json")).unwrap();
    let versions = merge_versions(&[], std::slice::from_ref(&mr.version), MrState::Open, at(1));
    // The tip moved: the same bytes as before.
    let mut moved = mr.clone();
    moved.version.base_sha = sha(V2);
    assert_eq!(with_recorded_base(&moved, &versions), mr);
    // A merge base of git is in the versions only: the snapshot does not get it.
    let mut computed = versions;
    computed[0].version.merge_base = Some(sha(V2));
    assert_eq!(with_recorded_base(&mr, &computed).version.merge_base, None);
    // The merge base of the forge follows its record.
    let mut gitlab = mr.clone();
    gitlab.version.merge_base = Some(sha(V1));
    assert_eq!(
        with_recorded_base(&gitlab, &computed).version.merge_base,
        Some(sha(V2))
    );
    // An unknown head stays as it is.
    assert_eq!(with_recorded_base(&mr, &[]), mr);
}
