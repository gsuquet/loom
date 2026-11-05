//! MR refs and diffs on temporary repositories built by `fixtures/forge/scenario.sh`: the git
//! commands of `loomd::mr_refs::git`, without a daemon and without a network.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use loom_api::{ContextAssignment, Remote, RemoteUrl, RepoIdentity, RepoSummary};
use loom_forge::diff::{file_hashes, reduced_patch, since_last_review};
use loom_forge::model::{MrKey, MrVersion, Sha};
use loom_forge::{github, gitlab};
use loomd::mr_refs::git::{
    LocalDiff, Reviewed, Step, clone_candidates, delete_refs, fetch_mr, has_head_ref, set_reviewed,
    version_diff,
};
use loomd::mr_refs::{MrClone, PATCH_CAP, choose_clone, head_ref, reviewed_ref};
use loomd::repo_update::GitRunner;
use tempfile::TempDir;

mod common;

use common::snapshot::Snapshot;
use common::{git, git_run};

const M1: &str = "506ce3f338acb7125973db71a5c43fd062c4f046";
const M2: &str = "ee047f5164ae63069c7237b6f24c66d34f9eff26";
const V1: &str = "7ebf283bfee25ab48f95c0c55181a4ca66b4c990";
const V2: &str = "84280c47b6aa128c645b1fbea54aded8b32e8463";
const JWT: &str = "src/auth/jwt.rs";

fn fixture(path: &str) -> Vec<u8> {
    fs::read(fixtures().join(path)).unwrap()
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/forge")
}

fn sha(text: &str) -> Sha {
    Sha::try_from(text.to_owned()).unwrap()
}

fn github_key() -> MrKey {
    MrKey::parse("acme/shop#412").unwrap()
}

fn github_version(file: &str) -> MrVersion {
    github::parse_pull(&fixture(file)).unwrap().version
}

/// A forge built by `scenario.sh`, and a clone of it. The forge starts at version 1: `main` on
/// M1, and the MR refs on V1. [`Scenario::force_push`] moves it to version 2, as the fixture says.
struct Scenario {
    dir: TempDir,
    forge: PathBuf,
    clone: PathBuf,
}

impl Scenario {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let built = Command::new(fixtures().join("scenario.sh"))
            .arg(dir.path().join("forge"))
            .env("HOME", &home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(built.status.success(), "{built:?}");
        let forge = dir.path().join("forge/shop.git");
        let scenario = Self {
            clone: dir.path().join("clone"),
            forge,
            dir,
        };
        scenario.at_version_1();
        // The clone checks out `main`: no local branch holds the commits of the MR.
        scenario.forge_git(&["symbolic-ref", "HEAD", "refs/heads/main"]);
        let (forge, clone) = (scenario.forge.clone(), scenario.clone.clone());
        git(
            &scenario.home(),
            scenario.dir.path(),
            &[
                "clone",
                "-q",
                forge.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        scenario
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn git(&self, args: &[&str]) -> String {
        git(&self.home(), &self.clone, args)
    }

    fn forge_git(&self, args: &[&str]) {
        git(&self.home(), &self.forge, args);
    }

    fn at_version_1(&self) {
        self.forge_git(&["update-ref", "refs/heads/main", M1]);
        self.forge_git(&["update-ref", "refs/heads/auth-refactor", V1]);
        self.forge_git(&["update-ref", "refs/pull/412/head", V1]);
        self.forge_git(&["update-ref", "refs/merge-requests/88/head", V1]);
    }

    fn force_push(&self) {
        self.forge_git(&["update-ref", "refs/heads/main", M2]);
        self.forge_git(&["update-ref", "refs/heads/auth-refactor", V2]);
        self.forge_git(&["update-ref", "refs/pull/412/head", V2]);
        self.forge_git(&["update-ref", "refs/merge-requests/88/head", V2]);
        // The forge no longer keeps V1.
        self.forge_git(&["update-ref", "-d", "refs/loom-fixture/v1"]);
    }

    fn origin(&self) -> MrClone {
        MrClone {
            path: self.clone.clone(),
            remote: "origin".to_owned(),
        }
    }

    fn fetch(&self) -> Step {
        fetch_mr(&runner(&self.home()), &self.origin(), &github_key(), "main")
    }

    fn diff(&self, version: &MrVersion) -> LocalDiff {
        version_diff(&runner(&self.home()), &self.clone, version, PATCH_CAP)
            .unwrap()
            .unwrap()
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot::take(&self.home(), &self.clone)
    }

    /// The snapshot, without the refs that loom may write.
    fn outside_loom(&self) -> Snapshot {
        let mut snapshot = self.snapshot();
        snapshot
            .refs
            .retain(|name, _| !name.starts_with("refs/loom/"));
        snapshot
    }

    fn head_ref_commit(&self) -> String {
        self.git(&["rev-parse", &head_ref("origin", 412)])
            .trim()
            .to_owned()
    }

    fn git_dir(&self) -> PathBuf {
        self.clone.join(".git")
    }
}

fn runner(home: &Path) -> GitRunner {
    GitRunner {
        env: vec![
            ("HOME".into(), Some(home.into())),
            ("GIT_CONFIG_GLOBAL".into(), Some("/dev/null".into())),
            ("GIT_CONFIG_NOSYSTEM".into(), Some("1".into())),
        ],
        // On a loaded machine, a stall past the grace gives an empty stderr tail, and the tests
        // read that tail. A test that needs a short grace sets its own.
        kill_grace: std::time::Duration::from_mins(1),
        ..GitRunner::from_env()
    }
}

#[test]
fn v1_then_v2_since_last_review_is_only_the_file_whose_diff_changed() {
    let scenario = Scenario::new();
    assert_eq!(scenario.fetch(), Step::Done);
    assert_eq!(scenario.head_ref_commit(), V1);
    let v1 = scenario.diff(&github_version("github/pull-412.v1.json"));
    // GitHub: the merge base of `base.sha` and the head.
    assert_eq!(v1.merge_base, sha(M1));

    scenario.force_push();
    assert_eq!(scenario.fetch(), Step::Done);
    assert_eq!(scenario.head_ref_commit(), V2);
    let v2 = scenario.diff(&github_version("github/pull-412.v2.json"));
    assert_eq!(v2.merge_base, sha(M2));

    assert_eq!(since_last_review(&v1.patch, &v2.patch), [JWT]);
    let files = |patch: &[u8]| -> Vec<String> {
        file_hashes(patch)
            .into_iter()
            .map(|file| file.path)
            .collect()
    };
    assert_eq!(files(&v2.patch), ["Cargo.lock", "src/api/login.rs", JWT]);
    // What a plain diff of the two heads would wrongly show.
    let plain = scenario.git(&["diff", "--name-only", V1, V2]);
    assert!(plain.contains("README.md"), "{plain}");
}

#[test]
fn gitlab_uses_its_own_ref_and_the_merge_base_it_gives() {
    let scenario = Scenario::new();
    let key = MrKey::parse("company-a/checkout!88").unwrap();
    scenario.force_push();
    let step = fetch_mr(&runner(&scenario.home()), &scenario.origin(), &key, "main");
    assert_eq!(step, Step::Done);
    assert_eq!(
        scenario.git(&["rev-parse", &head_ref("origin", 88)]).trim(),
        V2
    );
    let versions =
        gitlab::parse_versions(&fixture("gitlab/merge-request-88-versions.json")).unwrap();
    let v2 = versions
        .iter()
        .find(|version| version.head_sha == sha(V2))
        .unwrap();
    assert_eq!(v2.merge_base, Some(sha(M2)));
    assert_eq!(scenario.diff(v2).merge_base, sha(M2));
}

#[test]
fn the_merge_base_comes_from_the_base_of_the_version_not_from_the_fetched_base_ref() {
    let scenario = Scenario::new();
    // `main` moved to M2 before the fetch: the base ref is on M2, and V1 was made on M1.
    scenario.forge_git(&["update-ref", "refs/heads/main", M2]);
    assert_eq!(scenario.fetch(), Step::Done);
    let v1 = scenario.diff(&github_version("github/pull-412.v1.json"));
    assert_eq!(v1.merge_base, sha(M1));
    assert_eq!(
        file_hashes(&v1.patch).len(),
        3,
        "{}",
        String::from_utf8_lossy(&v1.patch)
    );
}

#[test]
fn v1_stays_readable_after_the_force_push_through_the_reviewed_ref() {
    let scenario = Scenario::new();
    let home = scenario.home();
    assert_eq!(scenario.fetch(), Step::Done);
    assert_eq!(
        set_reviewed(&runner(&home), &scenario.origin(), 412, &sha(V1)),
        Reviewed::Set
    );
    let before = scenario.diff(&github_version("github/pull-412.v1.json"));

    scenario.force_push();
    assert_eq!(scenario.fetch(), Step::Done);
    // Nothing else holds V1 in the clone: its branch does not exist here, and no reflog has it.
    scenario.git(&["update-ref", "-d", "refs/remotes/origin/auth-refactor"]);
    scenario.git(&["reflog", "expire", "--expire=now", "--all"]);
    scenario.git(&["gc", "-q", "--prune=now"]);

    assert_eq!(
        scenario
            .git(&["rev-parse", &reviewed_ref("origin", 412)])
            .trim(),
        V1
    );
    scenario.git(&["cat-file", "-e", &format!("{V1}^{{commit}}")]);
    let after = scenario.diff(&github_version("github/pull-412.v1.json"));
    assert_eq!(after, before);
}

#[test]
fn without_the_reviewed_ref_git_collects_v1() {
    // The other side of the test above: the ref is what holds V1.
    let scenario = Scenario::new();
    assert_eq!(scenario.fetch(), Step::Done);
    scenario.force_push();
    assert_eq!(scenario.fetch(), Step::Done);
    scenario.git(&["update-ref", "-d", "refs/remotes/origin/auth-refactor"]);
    scenario.git(&["reflog", "expire", "--expire=now", "--all"]);
    scenario.git(&["gc", "-q", "--prune=now"]);
    let found = git_run(&scenario.home(), &scenario.clone, &["cat-file", "-e", V1]);
    assert!(!found.status.success());
    assert_eq!(
        version_diff(
            &runner(&scenario.home()),
            &scenario.clone,
            &github_version("github/pull-412.v1.json"),
            PATCH_CAP
        ),
        Ok(None)
    );
    assert_eq!(
        set_reviewed(&runner(&scenario.home()), &scenario.origin(), 412, &sha(V1)),
        Reviewed::NoCommit
    );
}

/// A clone with local work: a staged and an unstaged change, an untracked file, a conflicted
/// merge in progress, a stash.
fn with_local_work(scenario: &Scenario) {
    let file = |name: &str, text: &str| fs::write(scenario.clone.join(name), text).unwrap();
    file("stashed", "stash\n");
    scenario.git(&["add", "stashed"]);
    scenario.git(&["-c", "user.name=a", "-c", "user.email=a@a", "stash", "-q"]);
    // A merge that stops on a conflict, whatever the content.
    scenario.git(&["checkout", "-q", "-b", "local"]);
    file("Cargo.lock", "# mine\n");
    scenario.git(&["add", "Cargo.lock"]);
    scenario.git(&[
        "-c",
        "user.name=a",
        "-c",
        "user.email=a@a",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-q",
        "-m",
        "mine",
    ]);
    let merge = git_run(
        &scenario.home(),
        &scenario.clone,
        &[
            "-c",
            "user.name=a",
            "-c",
            "user.email=a@a",
            "merge",
            "origin/auth-refactor",
        ],
    );
    assert!(!merge.status.success(), "{merge:?}");
    assert!(scenario.git_dir().join("MERGE_HEAD").exists());
    file("README.md", "dirty\n");
    file("untracked", "new\n");
    fs::write(scenario.clone.join("src/api/login.rs"), "staged\n").unwrap();
    scenario.git(&["add", "src/api/login.rs"]);
}

#[test]
fn a_dirty_tree_and_an_operation_in_progress_change_nothing_but_refs_loom() {
    let scenario = Scenario::new();
    with_local_work(&scenario);
    // Configuration that would write other refs, tags or `FETCH_HEAD` without the flags.
    for (key, value) in [
        ("fetch.prune", "true"),
        ("fetch.pruneTags", "true"),
        ("remote.origin.tagOpt", "--tags"),
        ("fetch.recurseSubmodules", "true"),
    ] {
        scenario.git(&["config", key, value]);
    }
    scenario.forge_git(&["tag", "new-upstream-tag", M1]);
    // A remote-tracking ref behind the forge: an opportunistic update would move it.
    scenario.git(&["update-ref", "refs/remotes/origin/main", M1]);
    scenario.force_push();
    let before = scenario.outside_loom();
    fs::remove_file(scenario.git_dir().join("FETCH_HEAD")).ok();

    assert_eq!(scenario.fetch(), Step::Done);
    assert_eq!(
        set_reviewed(&runner(&scenario.home()), &scenario.origin(), 412, &sha(V2)),
        Reviewed::Set
    );
    let _ = scenario.diff(&github_version("github/pull-412.v2.json"));

    assert_eq!(scenario.outside_loom(), before);
    assert!(!scenario.git_dir().join("FETCH_HEAD").exists());
    assert_eq!(scenario.head_ref_commit(), V2);
    assert_eq!(
        scenario
            .git(&["rev-parse", "refs/remotes/origin/main"])
            .trim(),
        M1
    );
    assert_eq!(
        delete_refs(&runner(&scenario.home()), &scenario.origin(), 412),
        Step::Done
    );
    assert_eq!(scenario.outside_loom(), before);
    assert_eq!(scenario.snapshot(), before);
}

#[test]
fn a_busy_lock_is_no_failure_and_is_never_removed() {
    let scenario = Scenario::new();
    assert_eq!(scenario.fetch(), Step::Done);
    let home = scenario.home();
    let run = runner(&home);
    let before = scenario.snapshot();
    let lock = scenario.git_dir().join("refs/loom/mr/origin/412/head.lock");
    fs::write(&lock, "").unwrap();
    scenario.force_push();

    assert!(
        matches!(scenario.fetch(), Step::Busy(_)),
        "{:?}",
        scenario.fetch()
    );
    assert!(lock.exists());
    assert_eq!(scenario.head_ref_commit(), V1);
    assert!(matches!(
        delete_refs(&run, &scenario.origin(), 412),
        Step::Busy(_)
    ),);
    let reviewed_lock = scenario
        .git_dir()
        .join("refs/loom/mr/origin/412/reviewed.lock");
    fs::write(&reviewed_lock, "").unwrap();
    assert!(matches!(
        set_reviewed(&run, &scenario.origin(), 412, &sha(V1)),
        Reviewed::Not(Step::Busy(_))
    ));
    assert!(reviewed_lock.exists());
    // `delete_refs` may have deleted the refs before the lock: the other refs are the same.
    let mut after = scenario.snapshot();
    let mut expected = before;
    after.refs.retain(|name, _| !name.starts_with("refs/loom/"));
    expected
        .refs
        .retain(|name, _| !name.starts_with("refs/loom/"));
    assert_eq!(after, expected);

    // The lock is gone: the next poll does the work.
    fs::remove_file(lock).unwrap();
    fs::remove_file(reviewed_lock).unwrap();
    assert_eq!(scenario.fetch(), Step::Done);
    assert_eq!(scenario.head_ref_commit(), V2);
}

fn summary(path: &Path, remotes: &[(&str, &str)]) -> RepoSummary {
    RepoSummary {
        identity: RepoIdentity {
            path: path.to_path_buf(),
            name: "shop".to_owned(),
            slug: "personal/shop".to_owned(),
            context: ContextAssignment::Unassigned,
            remotes: remotes
                .iter()
                .map(|(name, project)| Remote {
                    name: (*name).to_owned(),
                    url: format!("git@github.com:{project}.git"),
                    location: Some(RemoteUrl {
                        host: "github.com".to_owned(),
                        port: None,
                        path: (*project).to_owned(),
                    }),
                })
                .collect(),
            primary_remote: Some("origin".to_owned()),
            forge: None,
        },
        registered: false,
        last_used: None,
        state: None,
        last_update: None,
        forge_metadata: None,
    }
}

#[test]
fn a_fork_as_origin_and_the_project_as_upstream_uses_upstream() {
    let scenario = Scenario::new();
    let home = scenario.home();
    // The fork has no MR refs: a fetch from it would fail.
    let fork = scenario.dir.path().join("fork.git");
    git(
        &home,
        scenario.dir.path(),
        &[
            "clone",
            "-q",
            "--bare",
            scenario.forge.to_str().unwrap(),
            fork.to_str().unwrap(),
        ],
    );
    git(&home, &fork, &["update-ref", "-d", "refs/pull/412/head"]);
    scenario.git(&["remote", "set-url", "origin", fork.to_str().unwrap()]);
    scenario.git(&[
        "remote",
        "add",
        "upstream",
        scenario.forge.to_str().unwrap(),
    ]);

    let repos = [summary(
        &scenario.clone,
        &[("origin", "me/shop"), ("upstream", "acme/shop")],
    )];
    let candidates = clone_candidates(&repos, "github.com", "acme/shop");
    let clone = choose_clone(None, &candidates, "github.com", "acme/shop").unwrap();
    assert_eq!(clone.remote, "upstream");

    assert_eq!(
        fetch_mr(&runner(&home), &clone, &github_key(), "main"),
        Step::Done
    );
    assert_eq!(
        scenario
            .git(&["rev-parse", &head_ref("upstream", 412)])
            .trim(),
        V1
    );
    let origin_refs = scenario.git(&["for-each-ref", "refs/loom/mr/origin/"]);
    assert_eq!(origin_refs, "");
}

#[test]
fn the_worktrees_of_a_clone_are_told_apart_and_a_gone_clone_is_no_candidate() {
    let scenario = Scenario::new();
    let linked = scenario.dir.path().join("wt");
    scenario.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        linked.to_str().unwrap(),
    ]);
    let gone = scenario.dir.path().join("gone");
    let project = [("origin", "acme/shop")];
    let repos = [
        summary(&linked, &project),
        summary(&gone, &project),
        summary(&scenario.clone, &project),
        summary(&scenario.clone, &[("origin", "acme/other")]),
    ];
    let candidates = clone_candidates(&repos, "github.com", "acme/shop");
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].repo.identity.path, linked);
    assert!(!candidates[0].main);
    assert_eq!(candidates[1].repo.identity.path, scenario.clone);
    assert!(candidates[1].main);
    assert_eq!(candidates[0].common_dir, candidates[1].common_dir);
    // The main worktree, though the linked one has the shorter path.
    let chosen = choose_clone(None, &candidates, "github.com", "acme/shop").unwrap();
    assert_eq!(chosen.path, scenario.clone);
}

#[test]
fn the_patch_does_not_depend_on_the_diff_configuration_of_the_user() {
    let scenario = Scenario::new();
    assert_eq!(scenario.fetch(), Step::Done);
    let version = github_version("github/pull-412.v1.json");
    let plain = scenario.diff(&version);
    let external = scenario.dir.path().join("external-diff");
    fs::write(&external, "#!/bin/sh\necho external\n").unwrap();
    fs::set_permissions(&external, fs::Permissions::from_mode(0o755)).unwrap();
    for (key, value) in [
        ("diff.noprefix", "true"),
        ("diff.mnemonicPrefix", "true"),
        ("diff.context", "9"),
        ("diff.interHunkContext", "9"),
        ("diff.algorithm", "patience"),
        ("diff.renames", "false"),
        ("color.ui", "always"),
        ("color.diff", "always"),
        ("diff.external", external.to_str().unwrap()),
        ("diff.orderFile", "/nonexistent"),
        ("core.abbrev", "12"),
        ("diff.submodule", "log"),
        ("diff.ignoreSubmodules", "all"),
    ] {
        scenario.git(&["config", key, value]);
    }
    assert_eq!(scenario.diff(&version), plain);

    // Attributes of the working tree and of the user: not those of the commit.
    fs::write(scenario.clone.join(".gitattributes"), "*.rs -diff\n").unwrap();
    let attributes = scenario.dir.path().join("attributes");
    fs::write(&attributes, "*.lock -diff\n").unwrap();
    scenario.git(&[
        "config",
        "core.attributesFile",
        attributes.to_str().unwrap(),
    ]);
    assert_eq!(scenario.diff(&version), plain);
}

#[test]
fn a_patch_keeps_the_bytes_of_a_file_that_is_not_utf8() {
    let scenario = Scenario::new();
    assert_eq!(scenario.fetch(), Step::Done);
    fs::write(scenario.clone.join("latin1.txt"), b"t\xe9\n").unwrap();
    scenario.git(&["add", "latin1.txt"]);
    scenario.git(&[
        "-c",
        "user.name=a",
        "-c",
        "user.email=a@a",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-q",
        "-m",
        "bytes",
    ]);
    let head = sha(scenario.git(&["rev-parse", "HEAD"]).trim());
    let diff = scenario.diff(&MrVersion {
        head_sha: head,
        base_sha: sha(M1),
        merge_base: None,
        forge_version_id: None,
        created_at: None,
    });
    assert!(
        diff.patch.windows(4).any(|window| window == b"+t\xe9\n"),
        "{}",
        String::from_utf8_lossy(&diff.patch)
    );
}

#[test]
fn a_remote_that_fails_is_a_failure_that_writes_nothing() {
    let scenario = Scenario::new();
    let before = scenario.snapshot();
    let moved = scenario.dir.path().join("moved.git");
    fs::rename(&scenario.forge, &moved).unwrap();
    assert!(matches!(scenario.fetch(), Step::Failed { .. }));
    assert_eq!(scenario.snapshot(), before);
    fs::rename(&moved, &scenario.forge).unwrap();

    // A name from the forge that would write another ref is refused before git.
    let refused = fetch_mr(
        &runner(&scenario.home()),
        &scenario.origin(),
        &github_key(),
        "main:refs/heads/main",
    );
    assert!(matches!(refused, Step::Failed { .. }), "{refused:?}");
    assert_eq!(scenario.snapshot(), before);
    // A version whose base the clone does not have is a reason, not a panic.
    let error = version_diff(
        &runner(&scenario.home()),
        &scenario.clone,
        &MrVersion {
            head_sha: sha(V1),
            base_sha: sha(&"0".repeat(40)),
            merge_base: None,
            forge_version_id: None,
            created_at: None,
        },
        PATCH_CAP,
    );
    assert!(error.is_err(), "{error:?}");
}

#[test]
fn the_cleanup_deletes_the_refs_of_one_mr_only() {
    let scenario = Scenario::new();
    let home = scenario.home();
    let run = runner(&home);
    assert_eq!(scenario.fetch(), Step::Done);
    assert_eq!(
        set_reviewed(&run, &scenario.origin(), 412, &sha(V1)),
        Reviewed::Set
    );
    // Another MR, whose number starts with the same digits.
    scenario.git(&["update-ref", "refs/loom/mr/origin/4120/head", V1]);
    assert_eq!(delete_refs(&run, &scenario.origin(), 412), Step::Done);
    let left = scenario.git(&["for-each-ref", "--format=%(refname)", "refs/loom/"]);
    assert_eq!(left.trim(), "refs/loom/mr/origin/4120/head");
    // Nothing to delete is no failure.
    assert_eq!(delete_refs(&run, &scenario.origin(), 412), Step::Done);
}

#[test]
fn without_a_clone_the_diff_of_the_forge_gives_the_same_since_last_review() {
    let scenario = Scenario::new();
    assert_eq!(scenario.fetch(), Step::Done);
    let v1 = scenario
        .diff(&github_version("github/pull-412.v1.json"))
        .patch;
    let github =
        reduced_patch(&github::parse_files(&fixture("github/pull-412-files.v2.json")).unwrap());
    assert_eq!(since_last_review(&v1, &github), [JWT]);
    let gitlab = reduced_patch(
        &gitlab::parse_diffs(&fixture("gitlab/merge-request-88-diffs.json")).unwrap(),
    );
    assert_eq!(since_last_review(&v1, &gitlab), [JWT]);
}

/// A `git` that logs each command, then runs the real one. `refuse` is an option of a version
/// that does not know it: as an older git, it stops there.
fn logging_git(scenario: &Scenario, refuse: Option<&str>) -> (GitRunner, PathBuf) {
    let log = scenario.dir.path().join("git.log");
    let program = scenario.dir.path().join("logging-git");
    let refusal = refuse.map_or_else(String::new, |option| {
        format!("case \"$1\" in {option}*) echo \"unknown option: $1\" >&2; exit 129;; esac\n")
    });
    fs::write(
        &program,
        format!(
            "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$GIT_SSH_COMMAND\" \"$GIT_ATTR_NOSYSTEM\" \"$*\" >> '{}'\n{refusal}exec git \"$@\"\n",
            log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let runner = GitRunner {
        program,
        ..runner(&scenario.home())
    };
    (runner, log)
}

#[test]
fn an_older_git_without_attr_source_still_gives_the_patch() {
    let scenario = Scenario::new();
    assert_eq!(scenario.fetch(), Step::Done);
    let version = github_version("github/pull-412.v1.json");
    let plain = scenario.diff(&version);
    let (old, log) = logging_git(&scenario, Some("--attr-source"));
    let got = version_diff(&old, &scenario.clone, &version, PATCH_CAP)
        .unwrap()
        .unwrap();
    assert_eq!(got, plain);
    let log = fs::read_to_string(log).unwrap();
    assert!(log.contains("--attr-source"), "{log}");
}

#[test]
fn the_patch_is_only_kept_within_its_limit() {
    let scenario = Scenario::new();
    assert_eq!(scenario.fetch(), Step::Done);
    let version = github_version("github/pull-412.v1.json");
    let size = scenario.diff(&version).patch.len();
    let at_limit = version_diff(&runner(&scenario.home()), &scenario.clone, &version, size)
        .unwrap()
        .unwrap();
    assert!(!at_limit.over_limit);
    assert_eq!(at_limit.patch.len(), size);
    let over = version_diff(
        &runner(&scenario.home()),
        &scenario.clone,
        &version,
        size - 1,
    )
    .unwrap()
    .unwrap();
    assert!(over.over_limit);
    assert_eq!(over.patch, Vec::<u8>::new());
    assert_eq!(over.merge_base, sha(M1));
}

#[test]
fn a_base_branch_that_the_forge_no_longer_has_does_not_stop_the_head() {
    let scenario = Scenario::new();
    let step = fetch_mr(
        &runner(&scenario.home()),
        &scenario.origin(),
        &github_key(),
        "gone",
    );
    assert_eq!(step, Step::Done);
    assert_eq!(scenario.head_ref_commit(), V1);
    let refs = scenario.git(&["for-each-ref", "--format=%(refname)", "refs/loom/"]);
    assert_eq!(refs.trim(), head_ref("origin", 412));
    // The clone has the base of the version: the diff does not need the branch.
    let diff = scenario.diff(&github_version("github/pull-412.v1.json"));
    assert_eq!(diff.merge_base, sha(M1));

    // A head that the forge does not have is a failure of this MR, not of its host.
    scenario.forge_git(&["update-ref", "-d", "refs/pull/412/head"]);
    let missing = fetch_mr(
        &runner(&scenario.home()),
        &scenario.origin(),
        &github_key(),
        "main",
    );
    assert!(
        matches!(
            &missing,
            Step::Failed {
                problem: loom_api::RemoteProblem::Other,
                ..
            }
        ),
        "{missing:?}"
    );
}

/// The commit of the `moves` fixtures, on M1.
fn commit_the_moves(scenario: &Scenario) -> Sha {
    scenario.git(&["checkout", "-q", "-b", "moves", M1]);
    fs::write(scenario.clone.join("src/new.rs"), "pub fn new() {}\n").unwrap();
    scenario.git(&["rm", "-q", "src/auth/mod.rs"]);
    fs::create_dir_all(scenario.clone.join("docs")).unwrap();
    scenario.git(&["mv", "README.md", "docs/README.md"]);
    scenario.git(&["mv", "src/api/login.rs", "src/api/signin.rs"]);
    let signin = scenario.clone.join("src/api/signin.rs");
    let text = fs::read_to_string(&signin).unwrap();
    fs::write(
        &signin,
        text.replace("Response::ok(claims.sub)", "Response::ok(claims.exp)"),
    )
    .unwrap();
    scenario.git(&["add", "-A"]);
    scenario.git(&[
        "-c",
        "user.name=a",
        "-c",
        "user.email=a@a",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-q",
        "-m",
        "moves",
    ]);
    sha(scenario.git(&["rev-parse", "HEAD"]).trim())
}

#[test]
fn added_deleted_and_renamed_files_hash_the_same_from_git_and_from_both_forges() {
    let scenario = Scenario::new();
    let head = commit_the_moves(&scenario);
    let local = scenario.diff(&MrVersion {
        head_sha: head,
        base_sha: sha(M1),
        merge_base: None,
        forge_version_id: None,
        created_at: None,
    });
    let github =
        reduced_patch(&github::parse_files(&fixture("github/pull-files.moves.json")).unwrap());
    let gitlab = reduced_patch(
        &gitlab::parse_diffs(&fixture("gitlab/merge-request-diffs.moves.json")).unwrap(),
    );
    let sorted = |patch: &[u8]| {
        let mut files = file_hashes(patch);
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files
    };
    assert_eq!(sorted(&local.patch).len(), 4);
    assert!(sorted(&local.patch).iter().all(|file| file.hash.is_some()));
    assert_eq!(sorted(&github), sorted(&local.patch));
    assert_eq!(sorted(&gitlab), sorted(&local.patch));
    assert_eq!(
        since_last_review(&local.patch, &github),
        Vec::<String>::new()
    );
    assert_eq!(
        since_last_review(&gitlab, &local.patch),
        Vec::<String>::new()
    );
}

#[test]
fn the_exact_refs_of_an_mr_go_and_the_refs_of_a_remote_under_its_prefix_stay() {
    let scenario = Scenario::new();
    let home = scenario.home();
    // MR 12 of the remote `foo`, and MR 34 of the remote `foo/12`.
    scenario.git(&["update-ref", "refs/loom/mr/foo/12/head", V1]);
    scenario.git(&["update-ref", "refs/loom/mr/foo/12/34/head", V1]);
    let foo = MrClone {
        path: scenario.clone.clone(),
        remote: "foo".to_owned(),
    };
    assert_eq!(delete_refs(&runner(&home), &foo, 12), Step::Done);
    let left = scenario.git(&["for-each-ref", "--format=%(refname)", "refs/loom/"]);
    assert_eq!(left.trim(), "refs/loom/mr/foo/12/34/head");
}

#[test]
fn the_head_ref_is_seen_or_missed() {
    let scenario = Scenario::new();
    let run = runner(&scenario.home());
    assert!(!has_head_ref(&run, &scenario.origin(), 412));
    assert_eq!(scenario.fetch(), Step::Done);
    assert!(has_head_ref(&run, &scenario.origin(), 412));
}

/// A clone without blobs: `file://`, so that a filter applies.
fn blobless_clone(scenario: &Scenario) -> PathBuf {
    scenario.forge_git(&["config", "uploadpack.allowFilter", "true"]);
    scenario.forge_git(&["config", "uploadpack.allowAnySHA1InWant", "true"]);
    let partial = scenario.dir.path().join("partial");
    git(
        &scenario.home(),
        scenario.dir.path(),
        &[
            "clone",
            "-q",
            "--no-checkout",
            "--filter=blob:none",
            "--single-branch",
            &format!("file://{}", scenario.forge.display()),
            partial.to_str().unwrap(),
        ],
    );
    partial
}

#[test]
fn a_partial_clone_is_not_asked_for_a_commit_and_its_diff_can_fetch_blobs_in_batch_mode() {
    let scenario = Scenario::new();
    let partial = blobless_clone(&scenario);
    let clone = MrClone {
        path: partial.clone(),
        remote: "origin".to_owned(),
    };
    let run = runner(&scenario.home());
    // V2 is on the forge, not in the clone: a lazy fetch would bring it and say that it is here.
    assert_eq!(
        set_reviewed(&run, &clone, 412, &sha(V2)),
        Reviewed::NoCommit
    );
    let local = git(&scenario.home(), &partial, &["rev-list", "--all"]);
    assert!(!local.contains(V2), "{local}");

    assert_eq!(fetch_mr(&run, &clone, &github_key(), "main"), Step::Done);
    let (logging, log) = logging_git(&scenario, None);
    let diff = version_diff(
        &logging,
        &partial,
        &github_version("github/pull-412.v1.json"),
        PATCH_CAP,
    )
    .unwrap()
    .unwrap();
    assert_eq!(file_hashes(&diff.patch).len(), 3);
    // `git diff` read blobs through the remote: it cannot ask for a passphrase.
    let log = fs::read_to_string(log).unwrap();
    let line = log.lines().find(|line| line.contains(" diff ")).unwrap();
    // No attributes file of the system either.
    assert!(line.starts_with("ssh -o BatchMode=yes|1|"), "{line}");
    // The check of a commit: on a git that ignores `GIT_NO_LAZY_FETCH`, it can reach the remote.
    let line = log
        .lines()
        .find(|line| line.contains("cat-file -e"))
        .unwrap();
    assert!(line.starts_with("ssh -o BatchMode=yes|"), "{line}");
}

#[test]
fn a_real_submodule_is_not_fetched_and_its_diff_does_not_depend_on_the_configuration() {
    let scenario = Scenario::new();
    let home = scenario.home();
    // The submodule, with one commit, then one more that only its remote has.
    let sub_work = scenario.dir.path().join("sub-work");
    fs::create_dir_all(&sub_work).unwrap();
    let in_sub = |args: &[&str]| git(&home, &sub_work, args);
    in_sub(&["init", "-q", "-b", "main"]);
    let sub_commit = |message: &str| {
        fs::write(sub_work.join("file"), message).unwrap();
        in_sub(&["add", "-A"]);
        in_sub(&[
            "-c",
            "user.name=a",
            "-c",
            "user.email=a@a",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            message,
        ]);
        in_sub(&["rev-parse", "HEAD"]).trim().to_owned()
    };
    let s1 = sub_commit("one");
    let sub = scenario.dir.path().join("sub.git");
    git(
        &home,
        scenario.dir.path(),
        &[
            "clone",
            "-q",
            "--bare",
            sub_work.to_str().unwrap(),
            sub.to_str().unwrap(),
        ],
    );
    // The MR adds the submodule.
    scenario.git(&["submodule", "add", "-q", sub.to_str().unwrap(), "vendored"]);
    scenario.git(&[
        "-c",
        "user.name=a",
        "-c",
        "user.email=a@a",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-q",
        "-m",
        "add a submodule",
    ]);
    let head = sha(scenario.git(&["rev-parse", "HEAD"]).trim());
    scenario.git(&[
        "push",
        "-q",
        "origin",
        &format!("+{}:refs/pull/412/head", head.as_str()),
        "+HEAD:refs/heads/with-sub",
    ]);

    let s2 = sub_commit("two");
    git(
        &home,
        &sub_work,
        &["push", "-q", sub.to_str().unwrap(), "HEAD:refs/heads/main"],
    );
    let in_vendored = scenario.clone.join("vendored");
    for dir in [&scenario.clone, &in_vendored] {
        git(&home, dir, &["config", "protocol.file.allow", "always"]);
        git(&home, dir, &["config", "fetch.recurseSubmodules", "true"]);
    }
    // The configuration would fetch it without the flag.
    let before = Snapshot::take(&home, &in_vendored);
    assert_eq!(scenario.fetch(), Step::Done);
    assert_eq!(Snapshot::take(&home, &in_vendored), before);
    assert_eq!(
        git(
            &home,
            &in_vendored,
            &["rev-parse", "refs/remotes/origin/main"]
        )
        .trim(),
        s1
    );
    let unknown = git_run(&home, &in_vendored, &["cat-file", "-e", &s2]);
    assert!(!unknown.status.success());

    let version = MrVersion {
        head_sha: head,
        base_sha: sha(M1),
        merge_base: None,
        forge_version_id: None,
        created_at: None,
    };
    let plain = scenario.diff(&version);
    let text = String::from_utf8_lossy(&plain.patch).into_owned();
    assert!(text.contains(&format!("+Subproject commit {s1}")), "{text}");
    scenario.git(&["config", "diff.submodule", "log"]);
    scenario.git(&["config", "diff.ignoreSubmodules", "all"]);
    assert_eq!(scenario.diff(&version), plain);
}

/// Whether this machine has a French locale.
fn has_french_locale() -> bool {
    Command::new("locale")
        .arg("-a")
        .output()
        .is_ok_and(|locales| {
            String::from_utf8_lossy(&locales.stdout)
                .lines()
                .any(|locale| locale.eq_ignore_ascii_case("fr_FR.UTF-8"))
        })
}

#[test]
fn the_messages_of_git_are_read_in_any_locale_of_the_user() {
    let scenario = Scenario::new();
    if !has_french_locale() {
        eprintln!("skipped: no fr_FR.UTF-8 locale on this machine");
        return;
    }
    let mut french = runner(&scenario.home());
    french
        .env
        .push(("LC_ALL".into(), Some("fr_FR.UTF-8".into())));
    french.env.push(("LANGUAGE".into(), Some("fr".into())));
    // A base branch that the forge no longer has.
    assert_eq!(
        fetch_mr(&french, &scenario.origin(), &github_key(), "gone"),
        Step::Done
    );
    // A lock.
    let lock = scenario.git_dir().join("refs/loom/mr/origin/412/head.lock");
    fs::write(&lock, "").unwrap();
    scenario.force_push();
    let step = fetch_mr(&french, &scenario.origin(), &github_key(), "main");
    assert!(matches!(step, Step::Busy(_)), "{step:?}");
}

#[test]
fn a_diff_over_its_limit_stops_git_at_once() {
    let scenario = Scenario::new();
    assert_eq!(scenario.fetch(), Step::Done);
    // A slow diff: it passes the limit, then would run for longer than the test can wait.
    let program = scenario.dir.path().join("slow-git");
    fs::write(
        &program,
        "#!/bin/sh\ncase \"$*\" in\n*\" diff \"*) head -c 100 /dev/zero; sleep 60;;\n*) exec git \"$@\";;\nesac\n",
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let slow = GitRunner {
        program,
        kill_grace: std::time::Duration::from_secs(1),
        ..runner(&scenario.home())
    };
    let started = std::time::Instant::now();
    let diff = version_diff(
        &slow,
        &scenario.clone,
        &github_version("github/pull-412.v1.json"),
        10,
    )
    .unwrap()
    .unwrap();
    assert!(diff.over_limit);
    assert_eq!(diff.patch, Vec::<u8>::new());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_process_that_holds_the_output_of_a_diff_is_a_failure_not_an_empty_patch() {
    let scenario = Scenario::new();
    assert_eq!(scenario.fetch(), Step::Done);
    // git ends, and a child that it leaves behind keeps the pipe open.
    let program = scenario.dir.path().join("holding-git");
    fs::write(
        &program,
        "#!/bin/sh\ncase \"$*\" in\n*\" diff \"*) sleep 30 & exit 0;;\n*) exec git \"$@\";;\nesac\n",
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let holding = GitRunner {
        program,
        kill_grace: std::time::Duration::from_millis(300),
        ..runner(&scenario.home())
    };
    let diff = version_diff(
        &holding,
        &scenario.clone,
        &github_version("github/pull-412.v1.json"),
        PATCH_CAP,
    );
    let error = diff.unwrap_err();
    assert!(error.contains("still holds"), "{error}");
}
