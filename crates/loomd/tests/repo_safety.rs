//! The repository safety suite: scanning, reading the state and updating repositories never
//! lose local work (ADR-0012).
//!
//! One fixture per risky state, each a clone of a local bare remote of its own, under the
//! scan root of a temporary `HOME`; the remote always has commits the clone lacks. The real
//! `loomd` runs against them, driven through its API, in two rounds:
//!
//! 1. the update every repository gets when `loomd` starts (the only scheduled update a test
//!    can reach: no hook shortens `[repos] fetch_interval`);
//! 2. after new commits on every remote: `POST /v1/repos/scan`, a state read
//!    (`GET /v1/repos/{context}/{name}?refresh=true`) of each repository, then
//!    `POST /v1/repos/sync`.
//!
//! Each round snapshots every repository before and after ([`common::snapshot`]) and allows
//! only these differences: remote-tracking refs, `FETCH_HEAD`, `ORIG_HEAD` and reflogs (not
//! even these for policy `off`), plus, for the fixtures that must be fast-forwarded, the
//! checked-out branch, the index and the tracked files moving exactly to the upstream commit
//! read after the update. Every fixture is checked before the test fails, and the failure lists
//! every difference of every fixture.

#![allow(
    clippy::unwrap_used,
    reason = "fixture helpers outside `#[test]` functions: a failing helper must fail the test"
)]

mod common;

use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Instant;

use common::snapshot::{Allowed, FastForward, Snapshot, compare};
use common::{Daemon, POLL, TIMEOUT, git, git_run, ok, request};
use loom_api::{
    RepoList, RepoOperation, RepoSummary, ScanResult, SkipReason, StateField, SyncOutcome,
    SyncResult, UpdateOutcome, UpdateStep,
};
use serde_json::json;
use tempfile::TempDir;

const BRANCH: &str = "refs/heads/main";
const UPSTREAM: &str = "refs/remotes/origin/main";

/// A loom home and a user home under `/tmp`: the macOS default `$TMPDIR` is too long for a
/// socket path.
struct Suite {
    dir: TempDir,
}

impl Suite {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("loomd-safety-")
            .tempdir_in("/tmp")
            .unwrap();
        let suite = Self { dir };
        fs::create_dir_all(suite.loom_home()).unwrap();
        fs::create_dir_all(suite.user_home().join("perso")).unwrap();
        fs::create_dir_all(suite.dir.path().join("f")).unwrap();
        suite
    }

    fn loom_home(&self) -> PathBuf {
        self.dir.path().join("l")
    }

    fn user_home(&self) -> PathBuf {
        self.dir.path().join("u")
    }

    fn socket(&self) -> PathBuf {
        self.loom_home().join("run/loomd.sock")
    }

    /// `config.toml`: one context over the scan root, and the policies the fixtures set.
    fn configure(&self, fixtures: &[Fixture]) {
        let mut config = String::from(
            "[repos]\nroots = [\"~/perso\"]\n\n[contexts.personal]\nmatch.paths = [\"~/perso\"]\n",
        );
        for fixture in fixtures {
            if let Some(policy) = fixture.policy {
                write!(
                    config,
                    "\n[repos.\"~/perso/{}\"]\nupdate = \"{policy}\"\n",
                    fixture.name
                )
                .unwrap();
            }
        }
        fs::write(self.loom_home().join("config.toml"), config).unwrap();
    }

    fn start(&self) -> Daemon {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loomd"));
        command
            .env("LOOM_HOME", self.loom_home())
            .env("HOME", self.user_home())
            .env("XDG_CONFIG_HOME", self.user_home().join(".config"))
            // The git commands of the updates: no global or system configuration.
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_SSH_COMMAND")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("NO_COLOR")
            .arg("--color=never")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Daemon::start(&mut command, &self.socket())
    }

    fn list(&self) -> RepoList {
        ok(&request(&self.socket(), "GET", "/v1/repos", None))
    }

    /// Waits until each of the `count` repositories of the index has a last update.
    fn wait_for_updates(&self, count: usize) -> RepoList {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let list = self.list();
            if list.repos.len() == count && list.repos.iter().all(|repo| repo.last_update.is_some())
            {
                return list;
            }
            assert!(
                Instant::now() < deadline,
                "not every repository was updated: {:?}",
                list.repos
                    .iter()
                    .map(|repo| (&repo.identity.slug, repo.last_update.is_some()))
                    .collect::<Vec<_>>()
            );
            thread::sleep(POLL);
        }
    }
}

/// What an update must report for a fixture.
#[derive(Clone, Copy)]
enum Expect {
    /// `HEAD`'s branch fast-forwarded to the commit just pushed.
    FastForward,
    /// Fetched, nothing moved.
    Fetched,
    /// Fetched, then skipped for a reason matching the predicate (described by the string).
    Skipped(fn(&SkipReason) -> bool, &'static str),
    /// Policy `off`: no git command at all.
    Off,
}

/// Another working tree whose snapshot must not change but for a fetch, when `fetch`.
struct Extra {
    label: &'static str,
    path: PathBuf,
    fetch: bool,
}

/// A repository in a risky state, its own bare remote, and a seed clone that pushes to it.
struct Fixture {
    name: &'static str,
    home: PathBuf,
    root: PathBuf,
    repo: PathBuf,
    expect: Expect,
    /// Branch of the remote that gets new commits.
    incoming: &'static str,
    /// Files every incoming commit writes besides `f.txt` and `incoming-<round>.txt`.
    incoming_files: Vec<(&'static str, &'static str)>,
    policy: Option<&'static str>,
    extras: Vec<Extra>,
    /// A file a filter driver writes when it runs: must never exist.
    filter_marker: Option<PathBuf>,
    /// A file the `post-merge` hook writes: must exist after a fast-forward.
    hook_marker: Option<PathBuf>,
    /// A tag the remote moves onto every incoming commit.
    moved_tag: Option<&'static str>,
    /// A lock file held by "another git process", and its content: must never change.
    lock: Option<(PathBuf, &'static str)>,
}

impl Fixture {
    /// The remote `up.git` with three commits on `main` (`.gitignore`, `f.txt`, `keep.txt`) and
    /// a branch `other`, pushed from `seed`. The working clone is made by [`Self::clone_work`].
    fn new(suite: &Suite, name: &'static str, expect: Expect) -> Self {
        let root = suite.dir.path().join("f").join(name);
        fs::create_dir_all(&root).unwrap();
        let fixture = Self {
            name,
            home: suite.user_home(),
            repo: suite.user_home().join("perso").join(name),
            root,
            expect,
            incoming: "main",
            incoming_files: Vec::new(),
            policy: None,
            extras: Vec::new(),
            filter_marker: None,
            hook_marker: None,
            moved_tag: None,
            lock: None,
        };
        fixture.git_in(&fixture.root, &["init", "-q", "--bare", "up.git"]);
        fixture.git_in(&fixture.root, &["clone", "-q", &fixture.url(), "seed"]);
        let seed = fixture.seed();
        fixture.commit(
            &seed,
            &[
                (".gitignore", "*.log\n"),
                ("f.txt", "1\n"),
                ("keep.txt", "keep\n"),
            ],
            "base 1",
        );
        fixture.commit(&seed, &[("f.txt", "2\n")], "base 2");
        fixture.commit(&seed, &[("f.txt", "3\n")], "base 3");
        fixture.git_in(&seed, &["push", "-q", "origin", "HEAD:main", "HEAD:other"]);
        fixture
    }

    fn url(&self) -> String {
        format!("file://{}", self.root.join("up.git").display())
    }

    fn seed(&self) -> PathBuf {
        self.root.join("seed")
    }

    /// Clones the remote into the scan root, and adds an ignored file.
    fn clone_work(&self) {
        self.git_in(
            &self.root,
            &["clone", "-q", &self.url(), self.repo.to_str().unwrap()],
        );
        self.write("debug.log", "ignored\n");
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> String {
        git(&self.home, dir, args).trim().to_owned()
    }

    /// `git <args>` in the working clone, which must succeed.
    fn git(&self, args: &[&str]) -> String {
        self.git_in(&self.repo, args)
    }

    /// `git <args>` in the working clone, which must fail (a conflict that stops an operation).
    fn git_fails(&self, args: &[&str]) {
        let output = git_run(&self.home, &self.repo, args);
        assert!(
            !output.status.success(),
            "{}: git {args:?} was expected to stop",
            self.name
        );
    }

    fn write(&self, file: &str, content: &str) {
        let path = self.repo.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    /// Writes `files` in `dir` and commits them all.
    fn commit(&self, dir: &Path, files: &[(&str, &str)], message: &str) {
        for (file, content) in files {
            let path = dir.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        self.git_in(dir, &["add", "-A"]);
        self.git_in(dir, &["commit", "-q", "-m", message]);
    }

    /// A commit of `files` on `branch` of the remote; its id.
    fn push(&self, branch: &str, files: &[(&str, &str)], message: &str) -> String {
        let seed = self.seed();
        self.git_in(&seed, &["fetch", "-q", "origin"]);
        self.git_in(
            &seed,
            &["checkout", "-q", "-B", branch, &format!("origin/{branch}")],
        );
        self.commit(&seed, files, message);
        self.git_in(
            &seed,
            &["push", "-q", "origin", &format!("{branch}:{branch}")],
        );
        self.git_in(&seed, &["rev-parse", "HEAD"])
    }

    /// The new commits of `round` on the incoming branch; the id of the last one.
    fn push_incoming(&self, round: u32) -> String {
        let content = format!("round {round}\n");
        let file = format!("incoming-{round}.txt");
        let mut files = vec![("f.txt", content.as_str()), (file.as_str(), "new\n")];
        files.extend(self.incoming_files.iter().copied());
        let pushed = self.push(self.incoming, &files, &format!("round {round}"));
        if let Some(tag) = self.moved_tag {
            let seed = self.seed();
            self.git_in(&seed, &["tag", "-f", tag, &pushed]);
            self.git_in(&seed, &["push", "-q", "-f", "origin", tag]);
        }
        pushed
    }

    /// An executable script in the fixture directory, outside every working tree.
    fn script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn snapshots(&self) -> Vec<Snapshot> {
        std::iter::once(&self.repo)
            .chain(self.extras.iter().map(|extra| &extra.path))
            .map(|path| Snapshot::take(&self.home, path))
            .collect()
    }

    fn slug(&self) -> String {
        format!("personal/{}", self.name)
    }

    /// Every way the round went wrong for this fixture: outcomes other than expected, and
    /// differences in a snapshot that the outcome does not allow.
    fn check(
        &self,
        outcomes: &[UpdateOutcome],
        pushed: &str,
        before: &[Snapshot],
        after: &[Snapshot],
    ) -> Vec<String> {
        let mut problems = Vec::new();
        let from = before[0].head_id.as_deref().unwrap_or_default();
        let expected_ok = match self.expect {
            Expect::FastForward => matches!(
                outcomes,
                [UpdateOutcome::FastForwarded { branch, from: old, to }]
                    if branch == "main" && old == from && to == pushed
            ),
            Expect::Fetched => outcomes == [UpdateOutcome::Fetched],
            Expect::Skipped(matches, _) => {
                matches!(outcomes, [UpdateOutcome::Skipped { reason }] if matches(reason))
            }
            Expect::Off => {
                outcomes
                    == [UpdateOutcome::Skipped {
                        reason: SkipReason::PolicyOff,
                    }]
            }
        };
        if !expected_ok {
            problems.push(format!(
                "outcomes {outcomes:?}, expected {}",
                self.describe()
            ));
        }
        let incoming_ref = format!("refs/remotes/origin/{}", self.incoming);
        let fetched = after[0].commit_of(&incoming_ref) == Some(pushed);
        match self.expect {
            Expect::Off if fetched => problems.push(format!("{incoming_ref} fetched")),
            Expect::Off => {}
            _ if !fetched => problems.push(format!(
                "{incoming_ref} is {:?}, not the pushed {pushed}",
                after[0].commit_of(&incoming_ref)
            )),
            _ => {}
        }
        let allowed = Allowed {
            fetch: !matches!(self.expect, Expect::Off),
            fast_forward: matches!(self.expect, Expect::FastForward).then_some(FastForward {
                branch: BRANCH,
                upstream: UPSTREAM,
            }),
        };
        problems.extend(compare(&before[0], &after[0], allowed));
        for (index, extra) in self.extras.iter().enumerate() {
            let allowed = Allowed {
                fetch: extra.fetch,
                fast_forward: None,
            };
            problems.extend(
                compare(&before[index + 1], &after[index + 1], allowed)
                    .into_iter()
                    .map(|diff| format!("{}: {diff}", extra.label)),
            );
        }
        if let Some(marker) = &self.filter_marker
            && marker.exists()
        {
            problems.push(format!(
                "the filter driver ran: {}",
                fs::read_to_string(marker).unwrap_or_default().trim()
            ));
        }
        if let Some(marker) = &self.hook_marker
            && !marker.exists()
        {
            problems.push("the post-merge hook did not run".to_owned());
        }
        if let Some((lock, content)) = &self.lock
            && fs::read_to_string(lock).ok().as_deref() != Some(*content)
        {
            problems.push(format!("the lock {} was touched", lock.display()));
        }
        problems
    }

    const fn describe(&self) -> &'static str {
        match self.expect {
            Expect::FastForward => "fast-forwarded",
            Expect::Fetched => "fetched",
            Expect::Skipped(_, reason) => reason,
            Expect::Off => "skipped: update policy is off",
        }
    }
}

/// Every risky state, built as a user would leave it, in the order of the issue.
const FIXTURES: [fn(&Suite) -> Fixture; 25] = [
    clean_behind,
    dirty_tracked,
    staged_only,
    untracked_colliding,
    untracked_free,
    conflicted,
    ahead,
    diverged,
    detached,
    no_upstream,
    rebase,
    merge,
    cherry_pick,
    bisect,
    am,
    worktree,
    submodule,
    filter,
    hook_fails,
    tag,
    moved_tag,
    locked,
    stash,
    policy_fetch,
    policy_off,
];

/// Control: clean and behind, an untracked and an ignored file around.
fn clean_behind(suite: &Suite) -> Fixture {
    let f = Fixture::new(suite, "clean-behind", Expect::FastForward);
    f.clone_work();
    f
}

fn dirty_tracked(suite: &Suite) -> Fixture {
    let f = Fixture::new(
        suite,
        "dirty-tracked",
        Expect::Skipped(
            |reason| *reason == SkipReason::Dirty { paths: 1 },
            "skipped: 1 changed tracked path",
        ),
    );
    f.clone_work();
    // A file the incoming commits do not touch: git alone would carry it over.
    f.write("keep.txt", "my uncommitted edit\n");
    f
}

fn staged_only(suite: &Suite) -> Fixture {
    let f = Fixture::new(
        suite,
        "staged-only",
        Expect::Skipped(
            |reason| *reason == SkipReason::Dirty { paths: 1 },
            "skipped: 1 changed tracked path",
        ),
    );
    f.clone_work();
    f.write("keep.txt", "my staged edit\n");
    f.git(&["add", "keep.txt"]);
    f
}

fn untracked_colliding(suite: &Suite) -> Fixture {
    let mut f = Fixture::new(
        suite,
        "untracked-colliding",
        Expect::Skipped(
            |reason| {
                matches!(reason, SkipReason::MergeRefused { stderr_tail }
                    if stderr_tail.contains("collide.txt"))
            },
            "skipped: git refused the fast-forward (collide.txt)",
        ),
    );
    f.clone_work();
    f.write("collide.txt", "mine, never committed\n");
    f.incoming_files.push(("collide.txt", "theirs\n"));
    f
}

fn untracked_free(suite: &Suite) -> Fixture {
    let f = Fixture::new(suite, "untracked-free", Expect::FastForward);
    f.clone_work();
    f.write("notes/mine.txt", "mine, never committed\n");
    f
}

/// Unmerged paths without an operation: a stash applied with a conflict.
fn conflicted(suite: &Suite) -> Fixture {
    let f = Fixture::new(
        suite,
        "conflicted",
        Expect::Skipped(
            |reason| matches!(reason, SkipReason::Dirty { .. }),
            "skipped: changed tracked paths",
        ),
    );
    f.clone_work();
    f.git(&["checkout", "-q", "-b", "side"]);
    f.commit(&f.repo, &[("keep.txt", "side\n")], "side");
    f.write("keep.txt", "stashed\n");
    f.git(&["stash", "push", "-q"]);
    f.git(&["checkout", "-q", "main"]);
    f.git(&["branch", "-q", "-D", "side"]);
    f.git_fails(&["stash", "apply"]);
    f
}

/// Ahead only: the incoming commits are on another branch.
fn ahead(suite: &Suite) -> Fixture {
    let mut f = Fixture::new(suite, "ahead", Expect::Fetched);
    f.clone_work();
    f.commit(&f.repo, &[("local.txt", "local\n")], "local work");
    f.incoming = "other";
    f
}

fn diverged(suite: &Suite) -> Fixture {
    let f = Fixture::new(
        suite,
        "diverged",
        Expect::Skipped(
            |reason| matches!(reason, SkipReason::Diverged { branch, ahead: 1, .. } if branch == "main"),
            "skipped: main diverged from its upstream",
        ),
    );
    f.clone_work();
    f.commit(&f.repo, &[("local.txt", "local\n")], "local work");
    f
}

/// No local `main` left: the default branch is not a candidate either.
fn detached(suite: &Suite) -> Fixture {
    let f = Fixture::new(
        suite,
        "detached",
        Expect::Skipped(
            |reason| *reason == SkipReason::Detached,
            "skipped: HEAD detached",
        ),
    );
    f.clone_work();
    f.git(&["checkout", "-q", "--detach"]);
    f.git(&["branch", "-q", "-D", "main"]);
    f
}

fn no_upstream(suite: &Suite) -> Fixture {
    let f = Fixture::new(
        suite,
        "no-upstream",
        Expect::Skipped(
            |reason| matches!(reason, SkipReason::NoUpstream { branch } if branch == "topic"),
            "skipped: branch topic has no upstream",
        ),
    );
    f.clone_work();
    f.git(&["checkout", "-q", "-b", "topic"]);
    // `main` checked out nowhere would otherwise be moved as the default branch.
    f.git(&["branch", "-q", "--unset-upstream", "main"]);
    f
}

fn rebase(suite: &Suite) -> Fixture {
    let f = Fixture::new(
        suite,
        "rebase",
        Expect::Skipped(
            |reason| {
                *reason
                    == SkipReason::Operation {
                        operation: RepoOperation::Rebase,
                    }
            },
            "skipped: rebase in progress",
        ),
    );
    f.clone_work();
    f.commit(&f.repo, &[("f.txt", "mine\n")], "local work");
    f.push("main", &[("f.txt", "theirs\n")], "theirs");
    f.git(&["fetch", "-q", "origin"]);
    f.git_fails(&["rebase", "origin/main"]);
    f
}

fn merge(suite: &Suite) -> Fixture {
    let f = Fixture::new(
        suite,
        "merge",
        Expect::Skipped(
            |reason| {
                *reason
                    == SkipReason::Operation {
                        operation: RepoOperation::Merge,
                    }
            },
            "skipped: merge in progress",
        ),
    );
    f.clone_work();
    f.git(&["checkout", "-q", "-b", "side"]);
    f.commit(&f.repo, &[("side.txt", "side\n")], "side");
    f.git(&["checkout", "-q", "main"]);
    f.git(&["merge", "-q", "--no-ff", "--no-commit", "side"]);
    f
}

fn cherry_pick(suite: &Suite) -> Fixture {
    let f = Fixture::new(
        suite,
        "cherry-pick",
        Expect::Skipped(
            |reason| {
                *reason
                    == SkipReason::Operation {
                        operation: RepoOperation::CherryPick,
                    }
            },
            "skipped: cherry-pick in progress",
        ),
    );
    f.clone_work();
    f.git(&["checkout", "-q", "-b", "side"]);
    f.commit(&f.repo, &[("keep.txt", "side 1\n")], "side 1");
    f.commit(&f.repo, &[("keep.txt", "side 2\n")], "side 2");
    f.git(&["checkout", "-q", "main"]);
    f.git_fails(&["cherry-pick", "side"]);
    f
}

fn bisect(suite: &Suite) -> Fixture {
    let f = Fixture::new(
        suite,
        "bisect",
        Expect::Skipped(
            |reason| {
                *reason
                    == SkipReason::Operation {
                        operation: RepoOperation::Bisect,
                    }
            },
            "skipped: bisect in progress",
        ),
    );
    f.clone_work();
    // Checks out `HEAD~1`, detached; `main` is being bisected.
    f.git(&["bisect", "start", "HEAD", "HEAD~2"]);
    f
}

fn am(suite: &Suite) -> Fixture {
    let f = Fixture::new(
        suite,
        "am",
        Expect::Skipped(
            |reason| {
                *reason
                    == SkipReason::Operation {
                        operation: RepoOperation::Am,
                    }
            },
            "skipped: am in progress",
        ),
    );
    f.clone_work();
    f.git(&["checkout", "-q", "-b", "side"]);
    f.commit(&f.repo, &[("keep.txt", "side 1\n")], "side 1");
    f.commit(&f.repo, &[("keep.txt", "side 2\n")], "side 2");
    let patches = f.root.join("patches");
    f.git(&["format-patch", "-q", "-1", "-o", patches.to_str().unwrap()]);
    f.git(&["checkout", "-q", "main"]);
    f.git(&["branch", "-q", "-D", "side"]);
    let patch = fs::read_dir(&patches)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    f.git_fails(&["am", patch.to_str().unwrap()]);
    f
}

/// The default branch checked out in a linked worktree outside the scan root, the main
/// worktree on another branch with nothing incoming.
fn worktree(suite: &Suite) -> Fixture {
    let mut f = Fixture::new(suite, "worktree", Expect::Fetched);
    f.clone_work();
    f.git(&["checkout", "-q", "-b", "other", "--track", "origin/other"]);
    let linked = f.root.join("linked");
    f.git(&["worktree", "add", "-q", linked.to_str().unwrap(), "main"]);
    f.extras.push(Extra {
        label: "linked worktree on main",
        path: linked,
        fetch: true,
    });
    f
}

/// A clean parent with a dirty submodule.
fn submodule(suite: &Suite) -> Fixture {
    let mut f = Fixture::new(
        suite,
        "submodule",
        Expect::Skipped(
            |reason| matches!(reason, SkipReason::Dirty { .. }),
            "skipped: changed tracked paths",
        ),
    );
    let sub_seed = f.root.join("sub-seed");
    f.git_in(&f.root, &["init", "-q", "--bare", "sub.git"]);
    f.git_in(&f.root, &["init", "-q", sub_seed.to_str().unwrap()]);
    f.commit(&sub_seed, &[("s.txt", "sub\n")], "sub");
    let sub_url = format!("file://{}", f.root.join("sub.git").display());
    f.git_in(&sub_seed, &["push", "-q", &sub_url, "HEAD:main"]);
    let seed = f.seed();
    f.git_in(&seed, &["submodule", "add", "-q", &sub_url, "sub"]);
    f.git_in(&seed, &["commit", "-q", "-m", "submodule"]);
    f.git_in(&seed, &["push", "-q", "origin", "HEAD:main"]);
    f.clone_work();
    f.git(&["submodule", "update", "-q", "--init"]);
    f.write("sub/s.txt", "my uncommitted edit in the submodule\n");
    f.extras.push(Extra {
        label: "submodule",
        path: f.repo.join("sub"),
        fetch: false,
    });
    f
}

/// A filter driver that records each run; the filtered file changed on disk.
fn filter(suite: &Suite) -> Fixture {
    let mut f = Fixture::new(
        suite,
        "filter",
        Expect::Skipped(
            |reason| {
                matches!(
                    reason,
                    SkipReason::Unknown {
                        field: StateField::Dirty
                    } | SkipReason::Dirty { .. }
                )
            },
            "skipped: dirty unknown or changed tracked paths",
        ),
    );
    let seed = f.seed();
    f.commit(
        &seed,
        &[
            (".gitattributes", "*.dat filter=fake\n"),
            ("data.dat", "data\n"),
        ],
        "filtered data",
    );
    f.git_in(&seed, &["push", "-q", "origin", "HEAD:main"]);
    f.clone_work();
    let marker = f.root.join("filter-ran");
    let filter = f.script(
        "filter",
        &format!("echo \"$@\" >> '{}'\ncat", marker.display()),
    );
    let filter = filter.to_str().unwrap();
    f.git(&["config", "filter.fake.clean", &format!("{filter} clean")]);
    f.git(&["config", "filter.fake.smudge", &format!("{filter} smudge")]);
    f.write("data.dat", "data changed on disk, longer than before\n");
    f.filter_marker = Some(marker);
    f
}

/// A `post-merge` hook that fails: the fast-forward is done before it runs.
fn hook_fails(suite: &Suite) -> Fixture {
    let mut f = Fixture::new(suite, "hook-fails", Expect::FastForward);
    f.clone_work();
    let marker = f.root.join("hook-ran");
    let hook = f.repo.join(".git/hooks/post-merge");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\necho ran >> '{}'\necho 'post-merge failed' >&2\nexit 1\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    f.hook_marker = Some(marker);
    f
}

/// A local tag that differs from the remote one: kept, and the fetch does not fail.
fn tag(suite: &Suite) -> Fixture {
    let f = Fixture::new(suite, "tag", Expect::FastForward);
    let seed = f.seed();
    f.git_in(&seed, &["tag", "v1"]);
    f.git_in(&seed, &["push", "-q", "origin", "v1"]);
    f.clone_work();
    f.git(&["tag", "-f", "v1", "HEAD~1"]);
    f
}

/// A tag the remote moves onto the commits being fetched, with `fetch.pruneTags` and
/// `tagOpt = --tags` set: a plain fetch would refuse to clobber the local tag and fail;
/// updates fetch no tag, so the local tag stays and the branch moves.
fn moved_tag(suite: &Suite) -> Fixture {
    let mut f = Fixture::new(suite, "moved-tag", Expect::FastForward);
    let seed = f.seed();
    f.git_in(&seed, &["tag", "1.0.0"]);
    f.git_in(&seed, &["push", "-q", "origin", "1.0.0"]);
    f.clone_work();
    f.git(&["config", "fetch.prune", "true"]);
    f.git(&["config", "fetch.pruneTags", "true"]);
    f.git(&["config", "remote.origin.tagOpt", "--tags"]);
    f.moved_tag = Some("1.0.0");
    f
}

/// `index.lock` held by another git process (an editor's automatic fetch): the fast-forward
/// is skipped as busy, and the lock is left alone.
fn locked(suite: &Suite) -> Fixture {
    let mut f = Fixture::new(
        suite,
        "locked",
        Expect::Skipped(
            |reason| {
                matches!(
                    reason,
                    SkipReason::Busy {
                        step: UpdateStep::FastForward,
                        ..
                    }
                )
            },
            "skipped: repository busy",
        ),
    );
    f.clone_work();
    let lock = f.repo.join(".git/index.lock");
    let content = "held by another git process\n";
    fs::write(&lock, content).unwrap();
    f.lock = Some((lock, content));
    f
}

fn stash(suite: &Suite) -> Fixture {
    let f = Fixture::new(suite, "stash", Expect::FastForward);
    f.clone_work();
    f.write("keep.txt", "stashed\n");
    f.git(&["stash", "push", "-q", "-m", "my stash"]);
    f
}

fn policy_fetch(suite: &Suite) -> Fixture {
    let mut f = Fixture::new(suite, "policy-fetch", Expect::Fetched);
    f.clone_work();
    f.policy = Some("fetch");
    f
}

fn policy_off(suite: &Suite) -> Fixture {
    let mut f = Fixture::new(suite, "policy-off", Expect::Off);
    f.clone_work();
    f.policy = Some("off");
    f
}

/// The problems of every fixture in a round, prefixed by its name.
fn check_round(
    round: &str,
    fixtures: &[Fixture],
    outcomes: &[Vec<UpdateOutcome>],
    pushed: &[String],
    before: &[Vec<Snapshot>],
    after: &[Vec<Snapshot>],
) -> Vec<String> {
    let mut problems = Vec::new();
    for (index, fixture) in fixtures.iter().enumerate() {
        let found = fixture.check(
            &outcomes[index],
            &pushed[index],
            &before[index],
            &after[index],
        );
        eprintln!(
            "{round}: {:<20} {} {}",
            fixture.name,
            if found.is_empty() { "ok    " } else { "FAILED" },
            outcomes[index]
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        );
        problems.extend(
            found
                .into_iter()
                .map(|problem| format!("{round}: {}: {problem}", fixture.name)),
        );
    }
    problems
}

/// The last update of each fixture, from the index.
fn last_outcomes(list: &RepoList, fixtures: &[Fixture]) -> Vec<Vec<UpdateOutcome>> {
    fixtures
        .iter()
        .map(|fixture| {
            let repo: &RepoSummary = list
                .repos
                .iter()
                .find(|repo| repo.identity.slug == fixture.slug())
                .unwrap_or_else(|| panic!("{} not indexed", fixture.slug()));
            repo.last_update.as_ref().unwrap().outcomes.clone()
        })
        .collect()
}

/// `run` on every item, each on a thread of its own; the results in the order of `items`. The
/// fixtures are independent: building and reading them in parallel keeps the suite short.
fn in_parallel<T: Sync, R: Send>(items: &[T], run: impl Fn(&T) -> R + Sync) -> Vec<R> {
    thread::scope(|scope| {
        let handles: Vec<_> = items.iter().map(|item| scope.spawn(|| run(item))).collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    })
}

#[test]
fn scans_state_reads_and_updates_never_lose_local_work() {
    let started = Instant::now();
    let suite = Suite::new();
    let fixtures = in_parallel(&FIXTURES, |build| build(&suite));
    suite.configure(&fixtures);
    let snapshots = || in_parallel(&fixtures, Fixture::snapshots);
    let pushed = in_parallel(&fixtures, |fixture| fixture.push_incoming(1));
    let built = started.elapsed();

    // Round 1: the update of every repository when loomd starts.
    let before = snapshots();
    let daemon = suite.start();
    let list = suite.wait_for_updates(fixtures.len());
    let after = snapshots();
    let mut problems = check_round(
        "on start",
        &fixtures,
        &last_outcomes(&list, &fixtures),
        &pushed,
        &before,
        &after,
    );
    let first_round = started.elapsed();

    // Round 2: new commits, then a scan, a state read of each repository, and a sync.
    let pushed = in_parallel(&fixtures, |fixture| fixture.push_incoming(2));
    let before = snapshots();
    let socket = suite.socket();
    ok::<ScanResult>(&request(&socket, "POST", "/v1/repos/scan", None));
    for fixture in &fixtures {
        let path = format!("/v1/repos/{}?refresh=true", fixture.slug());
        let repo: RepoSummary = ok(&request(&socket, "GET", &path, None));
        assert!(repo.state.is_some(), "{}: no state", fixture.name);
    }
    let synced: SyncResult = ok(&request(
        &socket,
        "POST",
        "/v1/repos/sync",
        Some(&json!({})),
    ));
    let after = snapshots();
    let outcomes: Vec<Vec<UpdateOutcome>> = fixtures
        .iter()
        .map(|fixture| {
            let result = synced
                .repos
                .iter()
                .find(|repo| repo.slug == fixture.slug())
                .unwrap_or_else(|| panic!("{} not synced", fixture.slug()));
            match &result.result {
                SyncOutcome::Updated { update } => update.outcomes.clone(),
                SyncOutcome::Planned { .. } => panic!("{}: planned, not updated", fixture.name),
            }
        })
        .collect();
    problems.extend(check_round(
        "sync", &fixtures, &outcomes, &pushed, &before, &after,
    ));
    daemon.stop();
    eprintln!(
        "{} fixtures: built in {built:?}, first round done at {first_round:?}, total {:?}",
        fixtures.len(),
        started.elapsed()
    );

    assert!(
        problems.is_empty(),
        "local work was touched or an outcome is wrong:\n  {}",
        problems.join("\n  ")
    );
}
