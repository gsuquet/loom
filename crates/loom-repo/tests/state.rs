//! Repository state read with `gix`, compared with what git reports on the same fixture
//! repositories: `git status --porcelain=v2 --branch --untracked-files=all` and
//! `git rev-list --count --left-right`.
//!
//! Fixtures are built with the `git` CLI in temporary directories, hermetically: a temporary
//! `HOME`, no global or system configuration, no signing.

#![allow(
    clippy::unwrap_used,
    reason = "fixture helpers outside `#[test]` functions: a failing helper must fail the test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use loom_api::{MaybeKnown, RepoHead, RepoOperation, RepoState, Timestamp, Untracked, Upstream};
use loom_repo::state::GitConfig;
use loom_repo::{StateOptions, git_dirs, state};
use tempfile::TempDir;

/// A temporary directory holding a fake `HOME` and the fixture repositories.
struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            dir: TempDir::new().unwrap(),
        };
        fs::create_dir_all(fixture.home()).unwrap();
        fixture
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// `git <args>` in `dir`; its output, whatever its exit status.
    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .current_dir(dir)
            .env("HOME", self.home())
            .env("XDG_CONFIG_HOME", self.home().join(".config"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            // `git status` must not refresh the index the state is compared with.
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_EDITOR", "true")
            .env("GIT_MERGE_AUTOEDIT", "no")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .args([
                "-c",
                "user.name=Loom Test",
                "-c",
                "user.email=loom@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
                "-c",
                "advice.detachedHead=false",
            ])
            .args(args)
            .output()
            .unwrap()
    }

    /// `git <args>` in `dir`, which must succeed; its standard output.
    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let output = self.run(dir, args);
        assert!(
            output.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    /// `git <args>` in `dir`, which must fail (a conflict).
    fn git_fails(&self, dir: &Path, args: &[&str]) {
        let output = self.run(dir, args);
        assert!(!output.status.success(), "git {args:?} succeeded");
    }

    /// A new repository `name` with one commit of `f.txt`.
    fn repo(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        fs::create_dir_all(&path).unwrap();
        self.git(&path, &["init", "-q"]);
        self.commit(&path, "f.txt", "one\n", "first");
        path
    }

    /// Writes `file` with `content`, then commits everything.
    fn commit(&self, repo: &Path, file: &str, content: &str, message: &str) {
        let path = repo.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
        self.git(repo, &["add", "-A"]);
        self.git(repo, &["commit", "-q", "-m", message]);
    }

    /// `repo` with a bare `origin` it has pushed `main` to and tracks.
    fn repo_with_origin(&self, name: &str) -> PathBuf {
        let repo = self.repo(name);
        let origin = self.path(&format!("{name}-origin.git"));
        self.git(
            self.dir.path(),
            &["init", "-q", "--bare", origin.to_str().unwrap()],
        );
        self.git(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        self.git(&repo, &["push", "-q", "-u", "origin", "main"]);
        self.git(&repo, &["fetch", "-q", "origin"]);
        repo
    }

    /// A conflict on `f.txt` between `main` and `topic`, `main` checked out.
    fn conflicting(&self, name: &str) -> PathBuf {
        let repo = self.repo(name);
        self.git(&repo, &["checkout", "-q", "-b", "topic"]);
        self.commit(&repo, "f.txt", "topic\n", "topic");
        self.git(&repo, &["checkout", "-q", "main"]);
        self.commit(&repo, "f.txt", "main\n", "main");
        repo
    }

    /// The state of the repository at `workdir`, with `options`.
    fn state_with(workdir: &Path, options: &StateOptions) -> RepoState {
        let (git_dir, common_dir) = git_dirs(workdir).unwrap();
        state(&git_dir, &common_dir, workdir, now(), options)
    }

    fn state(workdir: &Path) -> RepoState {
        Self::state_with(workdir, &options())
    }

    /// What git reports for `repo`, in the shape of a state.
    fn expected(&self, repo: &Path) -> Expected {
        let status = self.git(
            repo,
            &[
                "status",
                "--porcelain=v2",
                "--branch",
                "--untracked-files=all",
            ],
        );
        let mut expected = Expected::default();
        let mut oid = String::new();
        let mut branch = String::new();
        let mut upstream = None;
        for line in status.lines() {
            if let Some(value) = line.strip_prefix("# branch.oid ") {
                value.clone_into(&mut oid);
            } else if let Some(value) = line.strip_prefix("# branch.head ") {
                value.clone_into(&mut branch);
            } else if let Some(value) = line.strip_prefix("# branch.upstream ") {
                upstream = Some(value.to_owned());
            } else if let Some(value) = line.strip_prefix("# branch.ab ") {
                let (ahead, behind) = value.split_once(' ').unwrap();
                expected.ab = Some((
                    ahead.trim_start_matches('+').parse().unwrap(),
                    behind.trim_start_matches('-').parse().unwrap(),
                ));
            } else if line.starts_with("1 ") || line.starts_with("2 ") || line.starts_with("u ") {
                expected.dirty += 1;
            } else if line.starts_with("? ") {
                expected.untracked += 1;
            } else {
                panic!("unexpected status line {line:?}");
            }
        }
        expected.head = if branch == "(detached)" {
            RepoHead::Detached {
                commit: self
                    .git(repo, &["rev-parse", "--short", "HEAD"])
                    .trim()
                    .to_owned(),
            }
        } else if oid == "(initial)" {
            RepoHead::Unborn { name: branch }
        } else {
            RepoHead::Branch { name: branch }
        };
        // git names the upstream even when its ref is missing; then it reports no counts.
        expected.upstream = upstream.filter(|_| expected.ab.is_some());
        if let Some(name) = &expected.upstream {
            let range = format!("HEAD...{name}");
            let counts = self.git(repo, &["rev-list", "--count", "--left-right", &range]);
            let (ahead, behind) = counts.trim().split_once('\t').unwrap();
            assert_eq!(
                expected.ab,
                Some((ahead.parse().unwrap(), behind.parse().unwrap())),
                "rev-list and status disagree"
            );
        }
        expected.last_commit = (oid != "(initial)").then(|| {
            let secs = self.git(repo, &["log", "-1", "--format=%ct"]);
            Timestamp::from_unix_secs(secs.trim().parse().unwrap()).unwrap()
        });
        expected
    }

    /// Compares every field git reports with the state, and checks `operation`.
    fn check(&self, repo: &Path, operation: Option<RepoOperation>) -> RepoState {
        let state = Self::state(repo);
        let expected = self.expected(repo);
        assert_eq!(state.error, None, "{state:?}");
        assert_eq!(state.head, MaybeKnown::Known(expected.head), "head");
        assert_eq!(state.dirty, MaybeKnown::Known(expected.dirty), "dirty");
        assert_eq!(
            state.untracked,
            MaybeKnown::Known(Untracked {
                count: expected.untracked,
                capped: false
            }),
            "untracked"
        );
        let upstream = expected.upstream.map(|name| {
            let (ahead, behind) = expected.ab.unwrap();
            Upstream {
                name,
                ahead: MaybeKnown::Known(ahead),
                behind: MaybeKnown::Known(behind),
            }
        });
        assert_eq!(state.upstream, MaybeKnown::Known(upstream), "upstream");
        assert_eq!(
            state.last_commit,
            MaybeKnown::Known(expected.last_commit),
            "last_commit"
        );
        assert_eq!(state.operation, MaybeKnown::Known(operation), "operation");
        state
    }
}

#[derive(Debug)]
struct Expected {
    head: RepoHead,
    dirty: u64,
    untracked: u64,
    upstream: Option<String>,
    ab: Option<(u64, u64)>,
    last_commit: Option<Timestamp>,
}

impl Default for Expected {
    fn default() -> Self {
        Self {
            head: RepoHead::Detached {
                commit: String::new(),
            },
            dirty: 0,
            untracked: 0,
            upstream: None,
            ab: None,
            last_commit: None,
        }
    }
}

const fn now() -> Timestamp {
    Timestamp::from_unix_secs(1_791_792_000).unwrap()
}

fn options() -> StateOptions {
    StateOptions {
        config: GitConfig::RepositoryOnly,
        ..StateOptions::default()
    }
}

#[test]
fn clean_repository_with_its_upstream() {
    let fixture = Fixture::new();
    let repo = fixture.repo_with_origin("clean");
    let state = fixture.check(&repo, None);
    assert_eq!(state.refreshed_at, now());
    assert_eq!(state.dirty, MaybeKnown::Known(0));
    assert_eq!(
        state
            .upstream
            .known()
            .flatten()
            .map(|upstream| upstream.name),
        Some("origin/main".to_owned())
    );
    // Fetched: FETCH_HEAD exists.
    assert!(matches!(state.last_fetch, MaybeKnown::Known(Some(_))));
    assert_eq!(
        state.default_branch,
        MaybeKnown::Known(Some("main".to_owned()))
    );
}

#[test]
fn modified_staged_deleted_and_renamed_files_are_dirty() {
    let fixture = Fixture::new();
    let repo = fixture.repo("changes");
    fixture.commit(&repo, "g.txt", "g\n", "g");
    fixture.commit(
        &repo,
        "h.txt",
        "a long enough content to be detected as a rename\n",
        "h",
    );
    fixture.commit(&repo, "k.txt", "k\n", "k");
    // Modified in the working tree.
    fs::write(repo.join("f.txt"), "two\n").unwrap();
    // Staged, then modified again: one path.
    fs::write(repo.join("g.txt"), "staged\n").unwrap();
    fixture.git(&repo, &["add", "g.txt"]);
    fs::write(repo.join("g.txt"), "staged then modified\n").unwrap();
    // Added.
    fs::write(repo.join("new.txt"), "new\n").unwrap();
    fixture.git(&repo, &["add", "new.txt"]);
    // Renamed and staged.
    fixture.git(&repo, &["mv", "h.txt", "renamed.txt"]);
    // Deleted, not staged.
    fs::remove_file(repo.join("k.txt")).unwrap();
    let state = fixture.check(&repo, None);
    assert_eq!(state.dirty, MaybeKnown::Known(5));
}

#[test]
fn untracked_files_respect_gitignore() {
    let fixture = Fixture::new();
    let repo = fixture.repo("untracked");
    fixture.commit(&repo, ".gitignore", "*.log\nbuild/\n", "ignore");
    fs::write(repo.join("a.txt"), "a").unwrap();
    fs::create_dir_all(repo.join("dir/sub")).unwrap();
    fs::write(repo.join("dir/b.txt"), "b").unwrap();
    fs::write(repo.join("dir/sub/c.txt"), "c").unwrap();
    // Ignored, not counted.
    fs::write(repo.join("debug.log"), "x").unwrap();
    fs::create_dir_all(repo.join("build/out")).unwrap();
    fs::write(repo.join("build/out/bin"), "x").unwrap();
    // Empty directories are not reported.
    fs::create_dir_all(repo.join("empty/dir")).unwrap();
    // A nested repository counts as one entry.
    let nested = repo.join("nested");
    fs::create_dir_all(&nested).unwrap();
    fixture.git(&nested, &["init", "-q"]);
    fs::write(nested.join("x.txt"), "x").unwrap();
    let state = fixture.check(&repo, None);
    assert_eq!(state.dirty, MaybeKnown::Known(0));
    assert_eq!(
        state.untracked,
        MaybeKnown::Known(Untracked {
            count: 4,
            capped: false
        })
    );
}

#[test]
fn untracked_count_stops_at_the_cap() {
    let fixture = Fixture::new();
    let repo = fixture.repo("many");
    for n in 0..15 {
        fs::write(repo.join(format!("u{n}.txt")), "u").unwrap();
    }
    let capped = Fixture::state_with(
        &repo,
        &StateOptions {
            untracked_cap: 10,
            ..options()
        },
    );
    assert_eq!(
        capped.untracked,
        MaybeKnown::Known(Untracked {
            count: 10,
            capped: true
        })
    );
    assert_eq!(capped.error, None);
    let exact = Fixture::state_with(
        &repo,
        &StateOptions {
            untracked_cap: 15,
            ..options()
        },
    );
    assert_eq!(
        exact.untracked,
        MaybeKnown::Known(Untracked {
            count: 15,
            capped: false
        })
    );
}

#[test]
fn detached_head_reports_the_short_commit() {
    let fixture = Fixture::new();
    let repo = fixture.repo_with_origin("detached");
    fixture.commit(&repo, "f.txt", "two\n", "second");
    fixture.git(&repo, &["checkout", "-q", "--detach", "HEAD~1"]);
    let state = fixture.check(&repo, None);
    assert!(matches!(
        state.head,
        MaybeKnown::Known(RepoHead::Detached { .. })
    ));
    assert_eq!(state.upstream, MaybeKnown::Known(None));
}

#[test]
fn branch_without_upstream() {
    let fixture = Fixture::new();
    let repo = fixture.repo_with_origin("no-upstream");
    fixture.git(&repo, &["checkout", "-q", "-b", "local-only"]);
    let state = fixture.check(&repo, None);
    assert_eq!(state.upstream, MaybeKnown::Known(None));
    assert_eq!(
        state.head,
        MaybeKnown::Known(RepoHead::Branch {
            name: "local-only".to_owned()
        })
    );
}

#[test]
fn ahead_behind_and_diverged() {
    let fixture = Fixture::new();
    let repo = fixture.repo_with_origin("ab");
    let other = fixture.path("ab-other");
    let origin = fixture.path("ab-origin.git");
    fixture.git(
        fixture.dir.path(),
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );

    // Ahead by 2.
    fixture.commit(&repo, "f.txt", "a1\n", "a1");
    fixture.commit(&repo, "f.txt", "a2\n", "a2");
    let state = fixture.check(&repo, None);
    assert_eq!(
        state
            .upstream
            .known()
            .flatten()
            .map(|u| (u.ahead, u.behind)),
        Some((MaybeKnown::Known(2), MaybeKnown::Known(0)))
    );

    // Behind by 3: another clone pushes, this one fetches.
    let behind = fixture.path("ab-behind");
    fixture.git(
        fixture.dir.path(),
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            behind.to_str().unwrap(),
        ],
    );
    for n in 0..3 {
        fixture.commit(&other, "o.txt", &format!("{n}\n"), "other");
    }
    fixture.git(&other, &["push", "-q", "origin", "main"]);
    fixture.git(&behind, &["fetch", "-q"]);
    let state = fixture.check(&behind, None);
    assert_eq!(
        state
            .upstream
            .known()
            .flatten()
            .map(|u| (u.ahead, u.behind)),
        Some((MaybeKnown::Known(0), MaybeKnown::Known(3)))
    );

    // Diverged: 2 ahead, 3 behind.
    fixture.git(&repo, &["fetch", "-q"]);
    let state = fixture.check(&repo, None);
    assert_eq!(
        state
            .upstream
            .known()
            .flatten()
            .map(|u| (u.ahead, u.behind)),
        Some((MaybeKnown::Known(2), MaybeKnown::Known(3)))
    );
}

#[test]
fn missing_upstream_ref_is_no_upstream() {
    let fixture = Fixture::new();
    let repo = fixture.repo_with_origin("gone");
    fixture.git(&repo, &["update-ref", "-d", "refs/remotes/origin/main"]);
    let state = fixture.check(&repo, None);
    assert_eq!(state.upstream, MaybeKnown::Known(None));
}

#[test]
fn merge_conflict() {
    let fixture = Fixture::new();
    let repo = fixture.conflicting("merge");
    fixture.git_fails(&repo, &["merge", "-q", "topic"]);
    let state = fixture.check(&repo, Some(RepoOperation::Merge));
    assert_eq!(state.dirty, MaybeKnown::Known(1));
}

#[test]
fn rebase_in_progress() {
    let fixture = Fixture::new();
    let repo = fixture.conflicting("rebase");
    fixture.git(&repo, &["checkout", "-q", "topic"]);
    fixture.git_fails(&repo, &["rebase", "main"]);
    let state = fixture.check(&repo, Some(RepoOperation::Rebase));
    // git reports a detached HEAD during a rebase.
    assert!(matches!(
        state.head,
        MaybeKnown::Known(RepoHead::Detached { .. })
    ));
}

#[test]
fn cherry_pick_in_progress() {
    let fixture = Fixture::new();
    let repo = fixture.conflicting("cherry-pick");
    fixture.git_fails(&repo, &["cherry-pick", "topic"]);
    fixture.check(&repo, Some(RepoOperation::CherryPick));
}

#[test]
fn revert_in_progress() {
    let fixture = Fixture::new();
    let repo = fixture.repo("revert");
    fixture.commit(&repo, "f.txt", "two\n", "two");
    fixture.commit(&repo, "f.txt", "three\n", "three");
    fixture.git_fails(&repo, &["revert", "--no-edit", "HEAD~1"]);
    fixture.check(&repo, Some(RepoOperation::Revert));
}

#[test]
fn bisect_in_progress() {
    let fixture = Fixture::new();
    let repo = fixture.repo("bisect");
    fixture.commit(&repo, "f.txt", "two\n", "two");
    fixture.commit(&repo, "f.txt", "three\n", "three");
    fixture.git(&repo, &["bisect", "start", "HEAD", "HEAD~2"]);
    fixture.check(&repo, Some(RepoOperation::Bisect));
}

#[test]
fn linked_worktree_has_its_own_head_and_changes() {
    let fixture = Fixture::new();
    let repo = fixture.repo_with_origin("main-tree");
    let worktree = fixture.path("linked");
    fixture.git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            worktree.to_str().unwrap(),
        ],
    );
    fs::write(worktree.join("f.txt"), "changed in the worktree\n").unwrap();
    fs::write(worktree.join("u.txt"), "u").unwrap();
    let state = fixture.check(&worktree, None);
    assert_eq!(
        state.head,
        MaybeKnown::Known(RepoHead::Branch {
            name: "feature".to_owned()
        })
    );
    assert_eq!(state.dirty, MaybeKnown::Known(1));
    // Fetched from the main working tree, whose FETCH_HEAD counts.
    assert!(matches!(state.last_fetch, MaybeKnown::Known(Some(_))));
    // The main working tree is unaffected.
    let main = fixture.check(&repo, None);
    assert_eq!(main.dirty, MaybeKnown::Known(0));
}

#[test]
fn empty_repository_is_unborn() {
    let fixture = Fixture::new();
    let repo = fixture.path("empty");
    fs::create_dir_all(&repo).unwrap();
    fixture.git(&repo, &["init", "-q"]);
    fs::write(repo.join("new.txt"), "n").unwrap();
    let state = fixture.check(&repo, None);
    assert_eq!(
        state.head,
        MaybeKnown::Known(RepoHead::Unborn {
            name: "main".to_owned()
        })
    );
    assert_eq!(state.last_commit, MaybeKnown::Known(None));
    assert_eq!(state.last_fetch, MaybeKnown::Known(None));
    assert_eq!(state.default_branch, MaybeKnown::Known(None));
    // Staged in an empty repository.
    fixture.git(&repo, &["add", "new.txt"]);
    let state = fixture.check(&repo, None);
    assert_eq!(state.dirty, MaybeKnown::Known(1));
}

#[test]
fn default_branch_from_the_remote_head_then_main_then_master() {
    let fixture = Fixture::new();
    let repo = fixture.repo_with_origin("default");
    fixture.git(&repo, &["checkout", "-q", "-b", "trunk"]);
    fixture.git(&repo, &["push", "-q", "origin", "trunk"]);
    fixture.git(&repo, &["remote", "set-head", "origin", "trunk"]);
    assert_eq!(
        Fixture::state(&repo).default_branch,
        MaybeKnown::Known(Some("trunk".to_owned()))
    );

    let master = fixture.path("master");
    fs::create_dir_all(&master).unwrap();
    fixture.git(&master, &["init", "-q", "-b", "master"]);
    fixture.commit(&master, "f.txt", "f\n", "f");
    assert_eq!(
        Fixture::state(&master).default_branch,
        MaybeKnown::Known(Some("master".to_owned()))
    );

    let other = fixture.path("other");
    fs::create_dir_all(&other).unwrap();
    fixture.git(&other, &["init", "-q", "-b", "dev"]);
    fixture.commit(&other, "f.txt", "f\n", "f");
    assert_eq!(
        Fixture::state(&other).default_branch,
        MaybeKnown::Known(None)
    );
}

#[test]
fn exhausted_budget_leaves_the_bounded_fields_unknown() {
    let fixture = Fixture::new();
    let repo = fixture.repo_with_origin("budget");
    fixture.commit(&repo, "f.txt", "ahead\n", "ahead");
    let state = Fixture::state_with(
        &repo,
        &StateOptions {
            budget: Duration::ZERO,
            ..options()
        },
    );
    assert_eq!(state.dirty, MaybeKnown::Unknown);
    assert_eq!(state.untracked, MaybeKnown::Unknown);
    let upstream = state.upstream.known().flatten().unwrap();
    assert_eq!(upstream.name, "origin/main");
    assert_eq!(
        (upstream.ahead, upstream.behind),
        (MaybeKnown::Unknown, MaybeKnown::Unknown)
    );
    // The cheap fields are still read.
    assert_eq!(
        state.head,
        MaybeKnown::Known(RepoHead::Branch {
            name: "main".to_owned()
        })
    );
    assert_eq!(state.operation, MaybeKnown::Known(None));
    assert!(matches!(state.last_commit, MaybeKnown::Known(Some(_))));
    let error = state.error.unwrap();
    assert!(error.contains("dirty: time budget"), "{error}");
    assert!(error.contains("untracked: time budget"), "{error}");
    assert!(error.contains("ahead/behind: time budget"), "{error}");
}

#[test]
fn unreadable_repository_is_all_unknown() {
    let fixture = Fixture::new();
    let repo = fixture.path("broken");
    fs::create_dir_all(repo.join(".git")).unwrap();
    let state = state(
        &repo.join(".git"),
        &repo.join(".git"),
        &repo,
        now(),
        &options(),
    );
    assert_eq!(state.head, MaybeKnown::Unknown);
    assert_eq!(state.dirty, MaybeKnown::Unknown);
    assert_eq!(state.operation, MaybeKnown::Unknown);
    assert_eq!(state.last_fetch, MaybeKnown::Known(None));
    assert!(
        state
            .error
            .as_deref()
            .unwrap()
            .starts_with("cannot open the repository"),
        "{state:?}"
    );
}
