use std::path::PathBuf;
use std::time::Duration;

use loom_api::{ContextAssignment, ForgeKind, Remote, RemoteUrl, RepoIdentity, RepoSummary};
use loom_core::{Kind, Link, StatusName};
use loom_forge::github::{Review, ReviewState, parse_pull, parse_reviews};
use loom_forge::gitlab::parse_versions;
use loom_forge::model::{MrKey, MrState, Sha};

use super::*;

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
    let table: Vec<(Option<Option<StatusName>>, Observation, Decision)> = vec![
        // Create: an open MR where I am requested, without a task.
        (None, seen(), Decision::Create),
        (
            None,
            Observation {
                previous_head: None,
                was_requested: None,
                ..seen()
            },
            Decision::Create,
        ),
        // Ignore: not requested, or not open.
        (None, removed.clone(), Decision::Ignore),
        (None, merged.clone(), Decision::Ignore),
        (None, closed.clone(), Decision::Ignore),
        // Nothing new.
        (Some(Some(Inbox)), seen(), Decision::Keep),
        (Some(Some(Done)), seen(), Decision::Keep),
        (Some(Some(Cancelled)), seen(), Decision::Keep),
        // My verdict on the current head closes an open task.
        (
            Some(Some(Inbox)),
            verdict_now.clone(),
            set(Done, Reason::Verdict),
        ),
        (
            Some(Some(Waiting)),
            verdict_now.clone(),
            set(Done, Reason::Verdict),
        ),
        (Some(Some(Done)), verdict_now, Decision::Keep),
        // A new head after the verdict reopens, even when the forge dropped my request.
        (
            Some(Some(Done)),
            after_review_on_v1.clone(),
            set(Ready, new_head),
        ),
        // A new head of an open task changes nothing; of a cancelled one either.
        (
            Some(Some(Ready)),
            after_review_on_v1.clone(),
            Decision::Keep,
        ),
        (Some(Some(Cancelled)), after_review_on_v1, Decision::Keep),
        // Request removed: cancelled, or done after a verdict.
        (
            Some(Some(Active)),
            removed.clone(),
            set(Cancelled, Reason::RequestRemoved),
        ),
        (
            Some(Some(Ready)),
            Observation {
                verdict: Some(sha(V1)),
                ..removed.clone()
            },
            set(Done, Reason::RequestRemoved),
        ),
        // Not requested at either poll: no change (the task reopened by a new head stays).
        (
            Some(Some(Ready)),
            Observation {
                was_requested: Some(false),
                ..removed.clone()
            },
            Decision::Keep,
        ),
        // Without a sync state, no edge.
        (
            Some(Some(Ready)),
            Observation {
                was_requested: None,
                ..removed
            },
            Decision::Keep,
        ),
        // Requested again.
        (
            Some(Some(Done)),
            requested_again.clone(),
            set(Ready, Reason::RequestedAgain),
        ),
        (
            Some(Some(Cancelled)),
            requested_again.clone(),
            set(Ready, Reason::RequestedAgain),
        ),
        (Some(Some(Ready)), requested_again, Decision::Keep),
        // A terminal remote state closes an open task, and leaves a closed one.
        (Some(Some(Ready)), merged.clone(), set(Done, Reason::Merged)),
        (
            Some(Some(Inbox)),
            closed.clone(),
            set(Cancelled, Reason::Closed),
        ),
        (Some(Some(Cancelled)), merged, Decision::Keep),
        (Some(Some(Done)), closed, Decision::Keep),
        // A status of a newer loom: read-only.
        (Some(None), seen(), Decision::Keep),
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
    let (recorded, new) = next_verdict(None, on_v1.clone());
    assert!(new);
    assert_eq!(recorded, on_v1);
    // Seen again: not new.
    assert_eq!(
        next_verdict(recorded.as_ref(), on_v1.clone()),
        (on_v1.clone(), false)
    );
    // The approval is reset by a push: still remembered.
    assert_eq!(next_verdict(recorded.as_ref(), None), (on_v1, false));
    // Approved again on V2: new.
    let on_v2 = gitlab_verdict(true, &sha(V2));
    assert_eq!(
        next_verdict(recorded.as_ref(), on_v2.clone()),
        (on_v2, true)
    );
}

#[test]
fn versions_are_kept_oldest_first_and_completed() {
    let gitlab = parse_versions(&fixture("gitlab/merge-request-88-versions.json")).unwrap();
    assert_eq!(gitlab.len(), 2);
    let versions = merge_versions(&[], &gitlab, at(100));
    let heads: Vec<&str> = versions
        .iter()
        .map(|record| record.version.head_sha.as_str())
        .collect();
    assert_eq!(heads, [V1, V2]);
    assert!(versions.iter().all(|record| record.first_seen == at(100)));
    // Seen again later: unchanged.
    assert_eq!(merge_versions(&versions, &gitlab, at(200)), versions);

    // GitHub: one version per poll, without a merge base.
    let v1 = parse_pull(&fixture("github/pull-412.v1.json"))
        .unwrap()
        .version;
    let v2 = parse_pull(&fixture("github/pull-412.v2.json"))
        .unwrap()
        .version;
    let first = merge_versions(&[], std::slice::from_ref(&v1), at(100));
    let both = merge_versions(&first, std::slice::from_ref(&v2), at(200));
    assert_eq!(both.len(), 2);
    assert_eq!(both[0].first_seen, at(100));
    assert_eq!(both[1].version.head_sha, sha(V2));
    assert_eq!(both[1].first_seen, at(200));

    // A merge base learned later fills the known version.
    let mut bare = gitlab[1].clone();
    bare.merge_base = None;
    bare.forge_version_id = None;
    let known = merge_versions(&[], &[bare], at(100));
    let completed = merge_versions(&known, &gitlab, at(200));
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
    assert_eq!(new.status, Status::Inbox);
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
    let versions = merge_versions(&[], std::slice::from_ref(&mr.version), at(100));
    let state = SyncState {
        v: SyncState::VERSION,
        requested: true,
        verdict: None,
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
