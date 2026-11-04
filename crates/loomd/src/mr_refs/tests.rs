use std::path::{Path, PathBuf};

use loom_api::{
    ContextAssignment, ForgeKind, Remote, RemoteUrl, RepoIdentity, RepoSummary, Timestamp,
};
use loom_forge::model::{MrKey, MrState, MrVersion, Sha};
use loom_store::port::TaskLocation;

use super::*;
use crate::review_sync::{VersionRecord, merge_versions};

const V1: &str = "7ebf283bfee25ab48f95c0c55181a4ca66b4c990";
const V2: &str = "84280c47b6aa128c645b1fbea54aded8b32e8463";
const M1: &str = "506ce3f338acb7125973db71a5c43fd062c4f046";
const M2: &str = "ee047f5164ae63069c7237b6f24c66d34f9eff26";

fn sha(text: &str) -> Sha {
    Sha::try_from(text.to_owned()).unwrap()
}

fn at(secs: u64) -> Timestamp {
    Timestamp::from_unix_secs(secs).unwrap()
}

fn remote(name: &str, location: Option<(&str, &str)>) -> Remote {
    Remote {
        name: name.to_owned(),
        url: format!("{name}-url"),
        location: location.map(|(host, path)| RemoteUrl {
            host: host.to_owned(),
            port: None,
            path: path.to_owned(),
        }),
    }
}

fn repo(path: &str, remotes: Vec<Remote>, primary: &str, last_used: Option<u64>) -> RepoSummary {
    RepoSummary {
        identity: RepoIdentity {
            path: PathBuf::from(path),
            name: "shop".to_owned(),
            slug: format!("personal/{path}"),
            context: ContextAssignment::Unassigned,
            remotes,
            primary_remote: Some(primary.to_owned()),
            forge: Some(ForgeKind::Github),
        },
        registered: false,
        last_used: last_used.map(at),
        state: None,
        last_update: None,
        forge_metadata: None,
    }
}

fn project(path: &str, last_used: Option<u64>) -> RepoSummary {
    repo(
        path,
        vec![remote("origin", Some(("github.com", "acme/shop")))],
        "origin",
        last_used,
    )
}

/// A clone of its own: main worktree, common directory under its path.
fn main_of(repo: &RepoSummary) -> CloneCandidate<'_> {
    CloneCandidate {
        repo,
        common_dir: repo.identity.path.join(".git"),
        main: true,
    }
}

fn choose(kept: Option<&MrClone>, candidates: &[CloneCandidate<'_>]) -> Option<MrClone> {
    choose_clone(kept, candidates, "github.com", "acme/shop")
}

fn clone_at(path: &str, remote: &str) -> MrClone {
    MrClone {
        path: PathBuf::from(path),
        remote: remote.to_owned(),
    }
}

#[test]
fn the_remote_is_the_one_that_names_the_project_not_the_primary() {
    let fork = repo(
        "/work/shop",
        vec![
            remote("origin", Some(("github.com", "me/shop"))),
            remote("upstream", Some(("github.com", "acme/shop"))),
        ],
        "origin",
        None,
    );
    assert_eq!(
        project_remote(&fork.identity, "github.com", "acme/shop"),
        Some("upstream")
    );
    assert_eq!(
        project_remote(&fork.identity, "github.com", "acme/other"),
        None
    );
    assert_eq!(
        project_remote(&fork.identity, "gitlab.com", "acme/shop"),
        None
    );
    // The same project under another case: both forges ignore it.
    assert_eq!(
        project_remote(&fork.identity, "GitHub.com", "Acme/Shop"),
        Some("upstream")
    );
}

#[test]
fn the_primary_remote_wins_when_two_remotes_name_the_project() {
    let both = repo(
        "/work/shop",
        vec![
            remote("mirror", Some(("github.com", "acme/shop"))),
            remote("origin", Some(("github.com", "acme/shop"))),
        ],
        "origin",
        None,
    );
    assert_eq!(
        project_remote(&both.identity, "github.com", "acme/shop"),
        Some("origin")
    );
    let not_primary = repo(
        "/work/shop",
        vec![
            remote("origin", None),
            remote("first", Some(("github.com", "acme/shop"))),
            remote("second", Some(("github.com", "acme/shop"))),
        ],
        "origin",
        None,
    );
    assert_eq!(
        project_remote(&not_primary.identity, "github.com", "acme/shop"),
        Some("first")
    );
}

#[test]
fn no_clone_of_the_project_is_no_choice() {
    let other = repo(
        "/work/other",
        vec![remote("origin", Some(("github.com", "acme/other")))],
        "origin",
        Some(5),
    );
    assert_eq!(choose(None, &[main_of(&other)]), None);
    assert_eq!(choose(None, &[]), None);
}

#[test]
fn the_most_recently_used_clone_wins_then_the_shortest_path() {
    let (long, short, used) = (
        project("/work/long/shop", None),
        project("/work/shop", None),
        project("/a/very/long/path/to/shop", Some(10)),
    );
    assert_eq!(
        choose(None, &[main_of(&long), main_of(&short)]),
        Some(clone_at("/work/shop", "origin"))
    );
    assert_eq!(
        choose(None, &[main_of(&long), main_of(&short), main_of(&used)]),
        Some(clone_at("/a/very/long/path/to/shop", "origin"))
    );
}

#[test]
fn the_main_worktree_of_a_clone_wins_even_when_a_linked_one_is_used_last() {
    let main = project("/work/a-rather-long-name/shop", Some(1));
    let linked = project("/work/wt", Some(9));
    let other = project("/work/other-clone", Some(5));
    let common = main.identity.path.join(".git");
    let candidates = [
        CloneCandidate {
            repo: &linked,
            common_dir: common.clone(),
            main: false,
        },
        CloneCandidate {
            repo: &main,
            common_dir: common,
            main: true,
        },
        main_of(&other),
    ];
    // The clone is used at 9 through its linked worktree: it wins over the other clone (5).
    assert_eq!(
        choose(None, &candidates),
        Some(clone_at("/work/a-rather-long-name/shop", "origin"))
    );
    // Only the linked worktree is indexed: it holds the same refs.
    let only_linked = [CloneCandidate {
        repo: &linked,
        common_dir: PathBuf::from("/work/main/.git"),
        main: false,
    }];
    assert_eq!(
        choose(None, &only_linked),
        Some(clone_at("/work/wt", "origin"))
    );
}

#[test]
fn the_kept_clone_stays_until_it_is_gone() {
    let (kept_repo, better) = (project("/work/kept-clone", None), project("/w", Some(99)));
    let kept = clone_at("/work/kept-clone", "origin");
    assert_eq!(
        choose(Some(&kept), &[main_of(&better), main_of(&kept_repo)]),
        Some(kept.clone())
    );
    // Gone from the index or from the disk: not a candidate.
    assert_eq!(
        choose(Some(&kept), &[main_of(&better)]),
        Some(clone_at("/w", "origin"))
    );
    assert_eq!(choose(Some(&kept), &[]), None);
    // A remote with a new name: the same clone, with that name.
    let renamed = repo(
        "/work/kept-clone",
        vec![remote("forge", Some(("github.com", "acme/shop")))],
        "forge",
        None,
    );
    assert_eq!(
        choose(Some(&kept), &[main_of(&better), main_of(&renamed)]),
        Some(clone_at("/work/kept-clone", "forge"))
    );
    // The clone no longer has a remote of the project: gone for this MR.
    let moved = repo(
        "/work/kept-clone",
        vec![remote("origin", Some(("github.com", "me/shop")))],
        "origin",
        None,
    );
    assert_eq!(
        choose(Some(&kept), &[main_of(&better), main_of(&moved)]),
        Some(clone_at("/w", "origin"))
    );
}

#[test]
fn the_refspecs_name_the_ref_of_each_forge_in_the_namespace_of_loom() {
    let github = MrKey::parse("acme/shop#412").unwrap();
    assert_eq!(
        fetch_refspecs(&github, "upstream", "main").unwrap(),
        [
            "+refs/pull/412/head:refs/loom/mr/upstream/412/head",
            "+refs/heads/main:refs/loom/mr/upstream/412/base",
        ]
    );
    let gitlab = MrKey::parse("company-a/checkout!88").unwrap();
    assert_eq!(
        fetch_refspecs(&gitlab, "origin", "release/1.2").unwrap(),
        [
            "+refs/merge-requests/88/head:refs/loom/mr/origin/88/head",
            "+refs/heads/release/1.2:refs/loom/mr/origin/88/base",
        ]
    );
    assert_eq!(ref_prefix("origin", 88), "refs/loom/mr/origin/88/");
    assert_eq!(
        reviewed_ref("origin", 88),
        "refs/loom/mr/origin/88/reviewed"
    );
    assert_eq!(head_ref("origin", 88), "refs/loom/mr/origin/88/head");
}

#[test]
fn a_name_from_outside_that_is_no_ref_name_is_refused() {
    let key = MrKey::parse("acme/shop#412").unwrap();
    for base in [
        "",
        "a:refs/heads/evil",
        "a b",
        "a..b",
        "*",
        "a?",
        "a[b",
        "a\\b",
        "a^",
        "a~1",
        "a\nb",
        "-x",
        "/a",
        "a/",
        "a//b",
        "a.lock",
        "@{a",
        "a.",
    ] {
        assert!(fetch_refspecs(&key, "origin", base).is_err(), "{base:?}");
    }
    assert!(fetch_refspecs(&key, "up stream", "main").is_err());
    assert!(fetch_refspecs(&key, "forks/me", "feature/a-b_c.d").is_ok());
}

#[test]
fn a_diff_is_made_once_per_head_while_the_mr_is_open() {
    use DiffKind::{Local, Reduced};
    use DiffPlan::{FromClone, FromForge, Keep};
    let open = MrState::Open;
    // (state, recorded, patch on disk, clone, plan)
    for (state, recorded, patch, clone, plan) in [
        (open, None, false, true, FromClone),
        (open, None, false, false, FromForge),
        (open, Some(Local), true, true, Keep),
        (open, Some(Reduced), true, false, Keep),
        // A clone came after the reduced patch: the full diff replaces it.
        (open, Some(Reduced), true, true, FromClone),
        // The clone is gone: the full patch stays.
        (open, Some(Local), true, false, Keep),
        // The record of an archived task, without its patch here.
        (open, Some(Local), false, true, FromClone),
        (open, Some(Local), false, false, FromForge),
        // A patch without a record is from a write that stopped before its record.
        (open, None, true, true, FromClone),
        (open, None, true, false, FromForge),
        // No more versions come.
        (MrState::Merged, None, false, true, Keep),
        (MrState::Closed, None, false, false, Keep),
    ] {
        assert_eq!(
            diff_plan(state, recorded, patch, clone),
            plan,
            "{state:?} {recorded:?} {patch} {clone}"
        );
    }
}

fn version(head: &str, merge_base: Option<&str>) -> MrVersion {
    MrVersion {
        head_sha: sha(head),
        base_sha: sha(M1),
        merge_base: merge_base.map(sha),
        forge_version_id: None,
        created_at: None,
    }
}

#[test]
fn a_diff_is_recorded_on_its_version_and_the_rest_stays() {
    let known = merge_versions(&[], &[version(V1, None)], at(1));
    let known = merge_versions(&known, &[version(V2, Some(M1))], at(2));
    let recorded = record_diffs(
        &known,
        &[
            VersionDiff {
                head: sha(V1),
                merge_base: Some(sha(M2)),
                kind: DiffKind::Local,
                patch: b"p".to_vec(),
            },
            // The merge base of the forge stays: it decides the hunks of the forge.
            VersionDiff {
                head: sha(V2),
                merge_base: Some(sha(M2)),
                kind: DiffKind::Reduced,
                patch: b"q".to_vec(),
            },
            // The poll does not know this head: nothing to record it on.
            VersionDiff {
                head: sha(M2),
                merge_base: None,
                kind: DiffKind::Local,
                patch: Vec::new(),
            },
        ],
    );
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].version.merge_base, Some(sha(M2)));
    assert_eq!(recorded[0].diff, Some(DiffKind::Local));
    assert_eq!(recorded[0].first_seen, at(1));
    assert_eq!(recorded[1].version.merge_base, Some(sha(M1)));
    assert_eq!(recorded[1].diff, Some(DiffKind::Reduced));
}

#[test]
fn the_diff_kind_is_written_only_when_known_and_a_poll_keeps_it() {
    let known = merge_versions(&[], &[version(V1, None)], at(1));
    let text = serde_json::to_string(&known).unwrap();
    assert!(!text.contains("\"diff\""), "{text}");
    let recorded = record_diffs(
        &known,
        &[VersionDiff {
            head: sha(V1),
            merge_base: None,
            kind: DiffKind::Reduced,
            patch: Vec::new(),
        }],
    );
    let text = serde_json::to_string(&recorded).unwrap();
    assert!(text.contains("\"diff\":\"reduced\""), "{text}");
    let read: Vec<VersionRecord> = serde_json::from_str(&text).unwrap();
    assert_eq!(read, recorded);
    // The next poll merges over what is recorded: the same bytes.
    assert_eq!(merge_versions(&read, &[version(V1, None)], at(9)), recorded);
}

#[test]
fn the_clone_in_mr_json_is_read_set_and_removed_without_other_changes() {
    let bytes = b"{\n  \"key\": \"acme/shop#412\",\n  \"title\": \"x\"\n}\n";
    assert_eq!(read_clone(bytes), None);
    let clone = clone_at("/work/shop", "upstream");
    let set = with_clone(bytes, Some(&clone)).unwrap();
    assert_eq!(read_clone(&set), Some(clone.clone()));
    assert_eq!(with_clone(&set, Some(&clone)).unwrap(), set);
    assert_eq!(with_clone(&set, None).unwrap(), bytes);
    assert!(with_clone(b"{", None).is_err());
    assert_eq!(read_clone(b"{"), None);
}

#[test]
fn a_patch_is_named_by_its_head() {
    assert_eq!(
        patch_name(&sha(V1)),
        "7ebf283bfee25ab48f95c0c55181a4ca66b4c990.patch"
    );
    assert_eq!(
        patch_path(Path::new("/m"), &sha(V1)),
        Path::new("/m/diffs/7ebf283bfee25ab48f95c0c55181a4ca66b4c990.patch")
    );
}

fn review(id: &str, context: &str, key: &str, location: TaskLocation) -> ReviewRow {
    ReviewRow {
        id: id.parse().unwrap(),
        context: context.parse().unwrap(),
        key: MrKey::parse(key).unwrap(),
        host: Some("github.com".to_owned()),
        location,
    }
}

#[test]
fn the_refs_of_an_archived_task_go_unless_an_active_task_has_the_mr() {
    let archived = TaskLocation::Archived(2026);
    let rows = [
        review("r1000000", "personal", "acme/shop#1", archived),
        // A new task after the archive.
        review("r2000000", "personal", "acme/shop#2", archived),
        review("r3000000", "personal", "acme/shop#2", TaskLocation::Active),
        // Another account, in another context, has its own task of the MR.
        review("r4000000", "personal", "acme/shop#3", archived),
        review("r5000000", "company-a", "acme/shop#3", TaskLocation::Active),
        // Another context: its own poll.
        review("r6000000", "company-a", "acme/shop#4", archived),
        // Not of this host.
        ReviewRow {
            host: Some("gitlab.com".to_owned()),
            ..review("r7000000", "personal", "acme/shop#5", archived)
        },
    ];
    let personal: ContextName = "personal".parse().unwrap();
    let ids: Vec<String> = refs_to_clean(&rows, &personal, "github.com")
        .into_iter()
        .map(|row| row.id.to_string())
        .collect();
    assert_eq!(ids, ["r1000000"]);
}
