//! The update of one repository against local bare remotes (`file://`), hermetically: a
//! temporary `HOME`, no global or system git configuration, no signing, fake `ssh` commands for
//! the remotes that hang or reject authentication.
//!
//! Every test checks that nothing else changed: refs other than the ones the update may move
//! (remote-tracking refs, tags, the branch it fast-forwards), the index, the files of the
//! working tree and the stash list.

#![allow(
    clippy::unwrap_used,
    reason = "fixture helpers outside `#[test]` functions: a failing helper must fail the test"
)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use loom_api::{
    RemoteCheckOutcome, RemoteProblem, RepoOperation, RepoUpdatePolicy, SkipReason, Timestamp,
    UpdateOutcome, UpdateStep,
};
use loom_repo::StateOptions;
use loom_repo::state::GitConfig;
use loomd::repo_update::{FetchStatus, GitRunner, Report, Target, check_remote, update};
use loomd::schedule::{Candidate, Schedule};
use tempfile::TempDir;

mod common;

const NOW: u64 = 1_791_792_000;

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    /// A bare `origin.git` with one commit of `f.txt` on `main`, pushed from `seed`.
    fn new() -> Self {
        let fixture = Self {
            dir: TempDir::new().unwrap(),
        };
        fs::create_dir_all(fixture.home()).unwrap();
        let origin = fixture.origin();
        fixture.git(
            fixture.dir.path(),
            &["init", "-q", "--bare", origin.to_str().unwrap()],
        );
        let seed = fixture.seed();
        fixture.git(
            fixture.dir.path(),
            &["clone", "-q", &fixture.origin_url(), seed.to_str().unwrap()],
        );
        fixture.commit(&seed, "f.txt", "one\n", "first");
        fixture.git(&seed, &["push", "-q", "origin", "HEAD:main"]);
        fixture
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn origin(&self) -> PathBuf {
        self.dir.path().join("origin.git")
    }

    fn origin_url(&self) -> String {
        format!("file://{}", self.origin().display())
    }

    fn seed(&self) -> PathBuf {
        self.dir.path().join("seed")
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// The environment of every git command: a temporary home, no global or system config.
    fn env(&self) -> Vec<(OsString, Option<OsString>)> {
        let set = |name: &str, value: &Path| (name.into(), Some(value.as_os_str().to_owned()));
        vec![
            set("HOME", &self.home()),
            set("XDG_CONFIG_HOME", &self.home().join(".config")),
            set("GIT_CONFIG_GLOBAL", Path::new("/dev/null")),
            set("GIT_CONFIG_NOSYSTEM", Path::new("1")),
            ("GIT_SSH_COMMAND".into(), None),
            ("GIT_SSH".into(), None),
        ]
    }

    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        common::git_run(&self.home(), dir, args)
    }

    /// `git <args>` in `dir`, which must succeed; its standard output, trimmed.
    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let output = self.run(dir, args);
        assert!(
            output.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn commit(&self, repo: &Path, file: &str, content: &str, message: &str) {
        let path = repo.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
        self.git(repo, &["add", "-A"]);
        self.git(repo, &["commit", "-q", "-m", message]);
    }

    /// A commit of `file` on `branch` of the remote.
    fn advance(&self, branch: &str, file: &str, content: &str) -> String {
        let seed = self.seed();
        self.git(&seed, &["fetch", "-q", "origin"]);
        let start = format!("origin/{branch}");
        if self
            .run(&seed, &["rev-parse", "--verify", "-q", &start])
            .status
            .success()
        {
            self.git(&seed, &["checkout", "-q", "-B", branch, &start]);
        } else {
            self.git(&seed, &["checkout", "-q", "-B", branch]);
        }
        self.commit(&seed, file, content, &format!("{file} on {branch}"));
        self.git(
            &seed,
            &["push", "-q", "origin", &format!("{branch}:{branch}")],
        );
        self.git(&seed, &["rev-parse", "HEAD"])
    }

    /// A clone of the remote at `name`, on `main`.
    fn clone(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        self.git(
            self.dir.path(),
            &["clone", "-q", &self.origin_url(), path.to_str().unwrap()],
        );
        path
    }

    fn runner(&self) -> GitRunner {
        GitRunner {
            program: PathBuf::from("git"),
            env: self.env(),
            ssh_in_env: false,
            fetch_timeout: Duration::from_secs(60),
            merge_timeout: Duration::from_secs(30),
            kill_grace: Duration::from_secs(2),
            children: None,
        }
    }

    fn update_with(repo: &Path, policy: RepoUpdatePolicy, runner: &GitRunner) -> Report {
        let options = StateOptions {
            config: GitConfig::RepositoryOnly,
            ..StateOptions::default()
        };
        let clock = || Timestamp::from_unix_secs(NOW).unwrap();
        update(
            Target {
                path: repo,
                slug: "test/work",
                primary_remote: Some("origin"),
                policy,
            },
            runner,
            &options,
            &clock,
        )
    }

    fn update(&self, repo: &Path, policy: RepoUpdatePolicy) -> Report {
        Self::update_with(repo, policy, &self.runner())
    }

    fn rev(&self, repo: &Path, rev: &str) -> String {
        self.git(repo, &["rev-parse", rev])
    }

    /// Everything an update must not touch, as it is now.
    fn snapshot(&self, repo: &Path) -> Snapshot {
        let refs = self
            .git(repo, &["for-each-ref", "--format=%(refname) %(objectname)"])
            .lines()
            .map(|line| {
                let (name, id) = line.split_once(' ').unwrap();
                (name.to_owned(), id.to_owned())
            })
            .collect();
        let mut files = BTreeMap::new();
        collect_files(repo, repo, &mut files);
        Snapshot {
            head: self.git(repo, &["rev-parse", "--symbolic-full-name", "HEAD"]),
            head_id: self.rev(repo, "HEAD"),
            refs,
            index: self.git(repo, &["ls-files", "--stage"]),
            files,
            stash: self.git(repo, &["stash", "list"]),
        }
    }

    /// Writes an executable script `name` and returns its path.
    fn script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.path(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

/// Work tree files (`.git` excluded), path to content.
fn collect_files(root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_name() == ".git" {
            continue;
        }
        if entry.file_type().unwrap().is_dir() {
            collect_files(root, &path, files);
        } else {
            files.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                fs::read(&path).unwrap(),
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    head: String,
    head_id: String,
    refs: BTreeMap<String, String>,
    index: String,
    files: BTreeMap<PathBuf, Vec<u8>>,
    stash: String,
}

impl Snapshot {
    /// Refs a fetch may change: remote-tracking refs and tags.
    fn local_refs(&self, moved: &[&str]) -> BTreeMap<String, String> {
        self.refs
            .iter()
            .filter(|(name, _)| {
                !name.starts_with("refs/remotes/")
                    && !name.starts_with("refs/tags/")
                    && !moved.contains(&name.as_str())
            })
            .map(|(name, id)| (name.clone(), id.clone()))
            .collect()
    }

    /// Nothing but remote-tracking refs and tags changed.
    fn assert_untouched(&self, after: &Self) {
        assert_eq!(after.head, self.head, "HEAD");
        assert_eq!(after.head_id, self.head_id, "HEAD commit");
        assert_eq!(after.local_refs(&[]), self.local_refs(&[]), "refs");
        assert_eq!(after.index, self.index, "index");
        assert_eq!(after.files, self.files, "work tree files");
        assert_eq!(after.stash, self.stash, "stash");
    }
}

fn outcomes(report: &Report) -> &[UpdateOutcome] {
    &report.update.outcomes
}

const fn skipped(reason: SkipReason) -> UpdateOutcome {
    UpdateOutcome::Skipped { reason }
}

#[test]
fn a_clean_branch_is_fast_forwarded_and_its_post_merge_hook_runs() {
    let fixture = Fixture::new();
    let repo = fixture.clone("work");
    let from = fixture.rev(&repo, "HEAD");
    fs::write(repo.join("notes.txt"), "mine\n").unwrap();
    let hook = repo.join(".git/hooks/post-merge");
    fs::write(
        &hook,
        "#!/bin/sh\necho \"$1\" > \"$(git rev-parse --git-dir)/post-merge-ran\"\n",
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let to = fixture.advance("main", "f.txt", "two\n");
    let before = fixture.snapshot(&repo);

    let report = fixture.update(&repo, RepoUpdatePolicy::Ff);

    assert_eq!(
        outcomes(&report),
        [UpdateOutcome::FastForwarded {
            branch: "main".to_owned(),
            from,
            to: to.clone(),
        }]
    );
    assert_eq!(report.fetch, FetchStatus::Succeeded);
    assert_eq!(report.update.policy, RepoUpdatePolicy::Ff);
    assert_eq!(report.update.time.unix_secs(), NOW);
    let after = fixture.snapshot(&repo);
    assert_eq!(after.head, "refs/heads/main");
    assert_eq!(after.head_id, to);
    assert_eq!(
        after.local_refs(&["refs/heads/main"]),
        before.local_refs(&["refs/heads/main"])
    );
    assert_eq!(after.refs["refs/heads/main"], to);
    assert_eq!(after.stash, before.stash);
    // The incoming change, and the untracked file kept.
    assert_eq!(fs::read_to_string(repo.join("f.txt")).unwrap(), "two\n");
    assert_eq!(
        fs::read_to_string(repo.join("notes.txt")).unwrap(),
        "mine\n"
    );
    assert_eq!(after.files.len(), before.files.len());
    // `git status` is clean: the index matches the new commit.
    assert_eq!(
        fixture.git(&repo, &["status", "--porcelain", "--untracked-files=no"]),
        ""
    );
    // The hook ran, as on a manual pull (`0`: not a squash merge).
    assert_eq!(
        fs::read_to_string(repo.join(".git/post-merge-ran")).unwrap(),
        "0\n"
    );
    // The state read after the update shows it.
    let state = report.state.unwrap();
    assert_eq!(
        state.upstream.known().flatten().unwrap().behind.known(),
        Some(0)
    );
}

#[test]
fn a_local_tag_that_differs_from_the_remote_one_does_not_fail_the_update() {
    let fixture = Fixture::new();
    let tagged = fixture.advance("main", "f.txt", "tagged\n");
    let seed = fixture.seed();
    fixture.git(&seed, &["tag", "v1", &tagged]);
    fixture.git(&seed, &["push", "-q", "origin", "v1"]);
    let repo = fixture.clone("work");
    // Moved locally: an explicit `git fetch --tags` would refuse to clobber it and fail.
    fixture.git(&repo, &["tag", "-f", "v1", "HEAD~1"]);
    let local_tag = fixture.rev(&repo, "v1");
    assert_ne!(local_tag, tagged);
    let to = fixture.advance("main", "f.txt", "after the tag\n");
    let rejected = fixture.run(&repo, &["fetch", "--tags", "origin"]);
    assert!(
        !rejected.status.success(),
        "git fetch --tags clobbers no tag"
    );

    let report = fixture.update(&repo, RepoUpdatePolicy::Ff);

    assert_eq!(
        report.fetch,
        FetchStatus::Succeeded,
        "{:?}",
        outcomes(&report)
    );
    assert!(
        matches!(
            outcomes(&report),
            [UpdateOutcome::FastForwarded { to: head, .. }] if *head == to
        ),
        "{:?}",
        outcomes(&report)
    );
    // The local tag is kept.
    assert_eq!(fixture.rev(&repo, "v1"), local_tag);
}

#[test]
fn a_tag_moved_upstream_does_not_fail_the_update_whatever_the_tag_configuration() {
    let fixture = Fixture::new();
    let seed = fixture.seed();
    fixture.git(&seed, &["tag", "1.0.0"]);
    fixture.git(&seed, &["push", "-q", "origin", "1.0.0"]);
    let repo = fixture.clone("work");
    // The configuration of the machine where it happened: `fetch.pruneTags` adds
    // `refs/tags/*:refs/tags/*` to every fetch of the remote, even with `--no-prune`, and
    // `remote.<name>.tagOpt = --tags` asks for every tag.
    fixture.git(&repo, &["config", "fetch.prune", "true"]);
    fixture.git(&repo, &["config", "fetch.pruneTags", "true"]);
    fixture.git(&repo, &["config", "remote.origin.tagOpt", "--tags"]);
    let local_tag = fixture.rev(&repo, "1.0.0");
    // Upstream, the tag moves onto a new commit: the history the next fetch brings.
    let to = fixture.advance("main", "f.txt", "two\n");
    fixture.git(&seed, &["tag", "-f", "1.0.0", &to]);
    fixture.git(&seed, &["push", "-q", "-f", "origin", "1.0.0"]);
    // A plain fetch refuses to clobber the local tag, and fails as a whole.
    let clobbered = fixture.run(&repo, &["fetch", "--no-prune", "origin"]);
    assert!(
        !clobbered.status.success(),
        "the fetch was expected to fail"
    );
    let stderr = String::from_utf8_lossy(&clobbered.stderr);
    assert!(stderr.contains("would clobber existing tag"), "{stderr}");
    // `--no-tags` alone does not stop the tag refspec of `fetch.pruneTags`.
    let no_tags = fixture.run(&repo, &["fetch", "--no-prune", "--no-tags", "origin"]);
    assert!(!no_tags.status.success(), "the fetch was expected to fail");
    assert_eq!(fixture.rev(&repo, "1.0.0"), local_tag);
    let before = fixture.snapshot(&repo);

    let report = fixture.update(&repo, RepoUpdatePolicy::Ff);

    assert_eq!(report.fetch, FetchStatus::Succeeded, "{report:?}");
    assert!(
        matches!(
            outcomes(&report),
            [UpdateOutcome::FastForwarded { to: head, .. }] if *head == to
        ),
        "{:?}",
        outcomes(&report)
    );
    // Only branches moved: the local tag is the user's.
    let after = fixture.snapshot(&repo);
    assert_eq!(after.refs["refs/tags/1.0.0"], local_tag);
    assert_eq!(
        after.local_refs(&["refs/heads/main"]),
        before.local_refs(&["refs/heads/main"])
    );
    let tags: Vec<&String> = after
        .refs
        .keys()
        .filter(|name| name.starts_with("refs/tags/"))
        .collect();
    assert_eq!(tags, ["refs/tags/1.0.0"]);
}

#[test]
fn a_lock_held_by_another_git_process_is_a_busy_skip_and_stays() {
    // `index.lock`: the fetch succeeds, the fast-forward meets the lock.
    let fixture = Fixture::new();
    let repo = fixture.clone("work");
    let remote = fixture.advance("main", "f.txt", "two\n");
    let lock = repo.join(".git/index.lock");
    fs::write(&lock, "held\n").unwrap();
    let before = fixture.snapshot(&repo);

    let report = fixture.update(&repo, RepoUpdatePolicy::Ff);

    let [
        UpdateOutcome::Skipped {
            reason: SkipReason::Busy { step, stderr_tail },
        },
    ] = outcomes(&report)
    else {
        panic!("{:?}", outcomes(&report));
    };
    assert_eq!(*step, UpdateStep::FastForward);
    assert!(stderr_tail.contains("index.lock"), "{stderr_tail}");
    assert_eq!(report.fetch, FetchStatus::Succeeded);
    assert!(!report.update.failed());
    assert_eq!(fs::read_to_string(&lock).unwrap(), "held\n");
    let after = fixture.snapshot(&repo);
    before.assert_untouched(&after);
    assert_eq!(after.refs["refs/remotes/origin/main"], remote);

    // Released: the next update fast-forwards.
    fs::remove_file(&lock).unwrap();
    let report = fixture.update(&repo, RepoUpdatePolicy::Ff);
    assert!(
        matches!(outcomes(&report), [UpdateOutcome::FastForwarded { .. }]),
        "{:?}",
        outcomes(&report)
    );

    // A ref lock: the fetch meets it; no backoff.
    let fixture = Fixture::new();
    let repo = fixture.clone("work");
    fixture.advance("main", "f.txt", "two\n");
    let lock = repo.join(".git/refs/remotes/origin/main.lock");
    fs::write(&lock, "held\n").unwrap();
    let before = fixture.snapshot(&repo);

    let report = fixture.update(&repo, RepoUpdatePolicy::Ff);

    assert!(
        matches!(
            outcomes(&report),
            [UpdateOutcome::Skipped {
                reason: SkipReason::Busy {
                    step: UpdateStep::Fetch,
                    ..
                }
            }]
        ),
        "{:?}",
        outcomes(&report)
    );
    assert_eq!(report.fetch, FetchStatus::Busy);
    assert_eq!(fs::read_to_string(&lock).unwrap(), "held\n");
    before.assert_untouched(&fixture.snapshot(&repo));
}

#[test]
fn a_remote_check_tells_a_reachable_remote_from_a_missing_one() {
    let fixture = Fixture::new();
    let timeout = Duration::from_secs(20);
    let local = fixture.clone("local");
    assert_eq!(
        check_remote(&local, "origin", &fixture.runner(), timeout),
        RemoteCheckOutcome::Reachable
    );

    // Empty remote: no `HEAD`, still reachable.
    let empty = fixture.path("empty.git");
    fixture.git(
        fixture.dir.path(),
        &["init", "-q", "--bare", empty.to_str().unwrap()],
    );
    let empty_url = format!("file://{}", empty.display());
    fixture.git(&local, &["remote", "add", "empty", &empty_url]);
    assert_eq!(
        check_remote(&local, "empty", &fixture.runner(), timeout),
        RemoteCheckOutcome::Reachable
    );

    // Gone.
    let gone_url = format!("file://{}", fixture.path("gone.git").display());
    fixture.git(&local, &["remote", "add", "gone", &gone_url]);
    let RemoteCheckOutcome::Failed { problem, .. } =
        check_remote(&local, "gone", &fixture.runner(), timeout)
    else {
        panic!("a missing remote is not reachable");
    };
    assert_eq!(problem, RemoteProblem::NotFound);
}

#[test]
fn a_remote_check_sees_the_git_configuration_of_the_remote() {
    let fixture = Fixture::new();
    let timeout = Duration::from_secs(20);
    // An SSH remote whose key comes from `core.sshCommand`, set for its URL only through
    // `includeIf "hasconfig:remote.*.url:…"` in the global configuration: git applies it,
    // and the check runs git, so it sees it. A plain `ssh` (first in `PATH`) is refused.
    let keyed = fixture.clone("keyed");
    fixture.git(
        &keyed,
        &[
            "remote",
            "set-url",
            "origin",
            "ssh://git@keyed.invalid/x.git",
        ],
    );
    let bin = fixture.path("bin");
    fs::create_dir_all(&bin).unwrap();
    fixture.script(
        "bin/ssh",
        "echo 'git@keyed.invalid: Permission denied (publickey).' >&2\nexit 255",
    );
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&path));
    let mut runner = fixture.runner();
    runner
        .env
        .push(("PATH".into(), Some(std::env::join_paths(paths).unwrap())));
    let RemoteCheckOutcome::Failed { problem, .. } =
        check_remote(&keyed, "origin", &runner, timeout)
    else {
        panic!("the plain ssh was expected to be refused");
    };
    assert_eq!(problem, RemoteProblem::SshAuth);

    let keyed_ssh = fixture.script(
        "keyed-ssh",
        &format!("exec git upload-pack '{}'", fixture.origin().display()),
    );
    let include = fixture.path("keyed.gitconfig");
    fs::write(
        &include,
        format!("[core]\n\tsshCommand = {}\n", keyed_ssh.display()),
    )
    .unwrap();
    let global = fixture.path("global.gitconfig");
    fs::write(
        &global,
        format!(
            "[includeIf \"hasconfig:remote.*.url:ssh://git@keyed.invalid/**\"]\n\tpath = {}\n",
            include.display()
        ),
    )
    .unwrap();
    runner
        .env
        .push(("GIT_CONFIG_GLOBAL".into(), Some(global.into_os_string())));
    assert_eq!(
        check_remote(&keyed, "origin", &runner, timeout),
        RemoteCheckOutcome::Reachable
    );
    // The repository is untouched: `ls-remote` writes nothing.
    assert!(!keyed.join(".git/FETCH_HEAD").exists());

    // No answer: stopped after its timeout, a network problem.
    let hanging = fixture.script("hanging-ssh", "exec sleep 60");
    fixture.git(
        &keyed,
        &["config", "core.sshCommand", hanging.to_str().unwrap()],
    );
    let runner = GitRunner {
        kill_grace: Duration::from_millis(500),
        ..fixture.runner()
    };
    let started = Instant::now();
    let outcome = check_remote(&keyed, "origin", &runner, Duration::from_secs(2));
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(
        matches!(
            &outcome,
            RemoteCheckOutcome::Failed { problem: RemoteProblem::Network, error, .. }
                if error == "no answer within 2s"
        ),
        "{outcome:?}"
    );
}

#[test]
fn local_changes_block_the_fast_forward() {
    for (name, change) in [
        ("modified", "modified" as &str),
        ("staged", "staged"),
        ("deleted", "deleted"),
    ] {
        let fixture = Fixture::new();
        let repo = fixture.clone(name);
        match change {
            "modified" => fs::write(repo.join("f.txt"), "local\n").unwrap(),
            "staged" => {
                fs::write(repo.join("g.txt"), "new\n").unwrap();
                fixture.git(&repo, &["add", "g.txt"]);
            }
            _ => fs::remove_file(repo.join("f.txt")).unwrap(),
        }
        let remote = fixture.advance("main", "f.txt", "two\n");
        let before = fixture.snapshot(&repo);

        let report = fixture.update(&repo, RepoUpdatePolicy::Ff);

        assert_eq!(
            outcomes(&report),
            [skipped(SkipReason::Dirty { paths: 1 })],
            "{name}"
        );
        assert_eq!(report.fetch, FetchStatus::Succeeded);
        let after = fixture.snapshot(&repo);
        before.assert_untouched(&after);
        // Fetched all the same.
        assert_eq!(after.refs["refs/remotes/origin/main"], remote, "{name}");
    }
}

#[test]
fn git_refuses_when_an_untracked_file_would_be_overwritten() {
    let fixture = Fixture::new();
    let repo = fixture.clone("work");
    fs::write(repo.join("incoming.txt"), "my own, never committed\n").unwrap();
    fixture.advance("main", "incoming.txt", "from the remote\n");
    let before = fixture.snapshot(&repo);

    let report = fixture.update(&repo, RepoUpdatePolicy::Ff);

    let [
        UpdateOutcome::Skipped {
            reason: SkipReason::MergeRefused { stderr_tail },
        },
    ] = outcomes(&report)
    else {
        panic!("{:?}", outcomes(&report));
    };
    assert!(stderr_tail.contains("incoming.txt"), "{stderr_tail}");
    // A refusal is not a fetch failure: no backoff.
    assert_eq!(report.fetch, FetchStatus::Succeeded);
    before.assert_untouched(&fixture.snapshot(&repo));
    assert_eq!(
        fs::read_to_string(repo.join("incoming.txt")).unwrap(),
        "my own, never committed\n"
    );
    // Asked again, the same answer: reported, never looped on.
    let again = fixture.update(&repo, RepoUpdatePolicy::Ff);
    assert!(matches!(
        outcomes(&again),
        [UpdateOutcome::Skipped {
            reason: SkipReason::MergeRefused { .. }
        }]
    ));
    before.assert_untouched(&fixture.snapshot(&repo));
}

#[test]
fn a_branch_ahead_of_its_upstream_is_never_moved() {
    let fixture = Fixture::new();
    let repo = fixture.clone("work");
    fixture.commit(&repo, "local.txt", "local\n", "local work");

    // Ahead only: nothing to bring.
    let before = fixture.snapshot(&repo);
    let report = fixture.update(&repo, RepoUpdatePolicy::Ff);
    assert_eq!(outcomes(&report), [UpdateOutcome::Fetched]);
    before.assert_untouched(&fixture.snapshot(&repo));

    // Diverged.
    fixture.advance("main", "f.txt", "two\n");
    let before = fixture.snapshot(&repo);
    let report = fixture.update(&repo, RepoUpdatePolicy::Ff);
    assert_eq!(
        outcomes(&report),
        [skipped(SkipReason::Diverged {
            branch: "main".to_owned(),
            ahead: 1,
            behind: 1,
        })]
    );
    before.assert_untouched(&fixture.snapshot(&repo));
}

#[test]
fn detached_head_and_branch_without_upstream_are_never_moved() {
    let fixture = Fixture::new();
    let repo = fixture.clone("detached");
    fixture.git(&repo, &["checkout", "-q", "--detach"]);
    fixture.advance("main", "f.txt", "two\n");
    let before = fixture.snapshot(&repo);
    let report = fixture.update(&repo, RepoUpdatePolicy::Ff);
    // `main` is checked out nowhere and behind: the default branch moves, HEAD does not.
    assert_eq!(outcomes(&report)[0], skipped(SkipReason::Detached));
    let after = fixture.snapshot(&repo);
    assert_eq!(after.head, before.head);
    assert_eq!(after.head_id, before.head_id);
    assert_eq!(after.files, before.files);
    assert_eq!(after.index, before.index);

    let repo = fixture.clone("no-upstream");
    fixture.git(&repo, &["checkout", "-q", "-b", "topic"]);
    // `main` is now behind but checked out nowhere: keep it out of this case.
    fixture.git(&repo, &["branch", "-q", "--unset-upstream", "main"]);
    fixture.advance("main", "f.txt", "three\n");
    let before = fixture.snapshot(&repo);
    let report = fixture.update(&repo, RepoUpdatePolicy::Ff);
    assert_eq!(
        outcomes(&report),
        [skipped(SkipReason::NoUpstream {
            branch: "topic".to_owned()
        })]
    );
    before.assert_untouched(&fixture.snapshot(&repo));
}

#[test]
fn a_rebase_in_progress_is_never_touched() {
    let fixture = Fixture::new();
    let repo = fixture.clone("work");
    fixture.commit(&repo, "f.txt", "mine\n", "local");
    fixture.advance("main", "f.txt", "theirs\n");
    fixture.git(&repo, &["fetch", "-q", "origin"]);
    let rebase = fixture.run(&repo, &["rebase", "origin/main"]);
    assert!(
        !rebase.status.success(),
        "the rebase must stop on a conflict"
    );
    fixture.advance("main", "other.txt", "more\n");
    let before = fixture.snapshot(&repo);

    let report = fixture.update(&repo, RepoUpdatePolicy::Ff);

    assert_eq!(
        outcomes(&report)[0],
        skipped(SkipReason::Operation {
            operation: RepoOperation::Rebase
        })
    );
    let after = fixture.snapshot(&repo);
    assert_eq!(after.head_id, before.head_id);
    assert_eq!(after.index, before.index);
    assert_eq!(after.files, before.files);
    assert_eq!(after.stash, before.stash);
    // The branch being rebased is in use: never moved as the default branch.
    assert_eq!(
        after.refs["refs/heads/main"],
        before.refs["refs/heads/main"]
    );
    assert!(repo.join(".git/rebase-merge").exists() || repo.join(".git/rebase-apply").exists());
}

#[test]
fn the_default_branch_moves_when_no_working_tree_has_it() {
    let fixture = Fixture::new();
    fixture.advance("feat", "feat.txt", "feat\n");
    let repo = fixture.clone("work");
    fixture.git(
        &repo,
        &["checkout", "-q", "-b", "feat", "--track", "origin/feat"],
    );
    let from = fixture.rev(&repo, "main");
    let to = fixture.advance("main", "f.txt", "two\n");
    let before = fixture.snapshot(&repo);

    let report = fixture.update(&repo, RepoUpdatePolicy::Ff);

    assert_eq!(
        outcomes(&report),
        [
            UpdateOutcome::Fetched,
            UpdateOutcome::DefaultBranchMoved {
                branch: "main".to_owned(),
                from,
                to: to.clone(),
            }
        ]
    );
    let after = fixture.snapshot(&repo);
    assert_eq!(after.refs["refs/heads/main"], to);
    assert_eq!(
        after.local_refs(&["refs/heads/main"]),
        before.local_refs(&["refs/heads/main"])
    );
    assert_eq!(after.head, "refs/heads/feat");
    assert_eq!(after.head_id, before.head_id);
    assert_eq!(after.files, before.files);
    assert_eq!(after.index, before.index);
    assert_eq!(after.stash, before.stash);

    // Both at once: `feat` behind too.
    let feat = fixture.advance("feat", "feat.txt", "feat 2\n");
    let to = fixture.advance("main", "f.txt", "three\n");
    let report = fixture.update(&repo, RepoUpdatePolicy::Ff);
    assert!(matches!(
        outcomes(&report),
        [
            UpdateOutcome::FastForwarded { to: head, .. },
            UpdateOutcome::DefaultBranchMoved { to: main, .. },
        ] if *head == feat && *main == to
    ));
}

#[test]
fn the_default_branch_checked_out_in_a_linked_worktree_is_not_moved() {
    let fixture = Fixture::new();
    fixture.advance("feat", "feat.txt", "feat\n");
    let repo = fixture.clone("work");
    fixture.git(
        &repo,
        &["checkout", "-q", "-b", "feat", "--track", "origin/feat"],
    );
    let linked = fixture.path("linked");
    fixture.git(
        &repo,
        &["worktree", "add", "-q", linked.to_str().unwrap(), "main"],
    );
    fixture.advance("main", "f.txt", "two\n");
    let before = fixture.snapshot(&repo);
    let linked_before = fixture.snapshot(&linked);

    let report = fixture.update(&repo, RepoUpdatePolicy::Ff);

    assert_eq!(outcomes(&report), [UpdateOutcome::Fetched]);
    before.assert_untouched(&fixture.snapshot(&repo));
    linked_before.assert_untouched(&fixture.snapshot(&linked));

    // The linked worktree, updated on its own, fast-forwards its `main`.
    let report = fixture.update(&linked, RepoUpdatePolicy::Ff);
    assert!(
        matches!(
            outcomes(&report),
            [UpdateOutcome::FastForwarded { branch, .. }] if branch == "main"
        ),
        "{:?}",
        outcomes(&report)
    );
}

#[test]
fn policy_fetch_never_moves_a_branch_and_off_never_fetches() {
    let fixture = Fixture::new();
    fixture.advance("feat", "feat.txt", "feat\n");
    let repo = fixture.clone("work");
    fixture.git(
        &repo,
        &["checkout", "-q", "-b", "feat", "--track", "origin/feat"],
    );
    fixture.advance("feat", "feat.txt", "feat 2\n");
    let remote_main = fixture.advance("main", "f.txt", "two\n");
    let before = fixture.snapshot(&repo);

    let report = fixture.update(&repo, RepoUpdatePolicy::Off);
    assert_eq!(outcomes(&report), [skipped(SkipReason::PolicyOff)]);
    assert_eq!(report.fetch, FetchStatus::NotRun);
    assert_eq!(report.state, None);
    let after = fixture.snapshot(&repo);
    // Not even the remote-tracking refs.
    assert_eq!(after, before);
    assert!(!repo.join(".git/FETCH_HEAD").exists());

    let report = fixture.update(&repo, RepoUpdatePolicy::Fetch);
    assert_eq!(outcomes(&report), [UpdateOutcome::Fetched]);
    assert_eq!(report.fetch, FetchStatus::Succeeded);
    let after = fixture.snapshot(&repo);
    before.assert_untouched(&after);
    assert_eq!(after.refs["refs/remotes/origin/main"], remote_main);
    assert!(repo.join(".git/FETCH_HEAD").exists());
}

#[test]
fn a_hanging_remote_times_out_and_its_processes_are_stopped() {
    let fixture = Fixture::new();
    let repo = fixture.clone("work");
    let pid_file = fixture.path("ssh.pid");
    let ssh = fixture.script(
        "hanging-ssh",
        &format!("echo $$ > '{}'\nexec sleep 60", pid_file.display()),
    );
    fixture.git(
        &repo,
        &["remote", "set-url", "origin", "ssh://hang.invalid/x.git"],
    );
    fixture.git(&repo, &["config", "core.sshCommand", ssh.to_str().unwrap()]);
    let before = fixture.snapshot(&repo);
    let runner = GitRunner {
        fetch_timeout: Duration::from_secs(5),
        kill_grace: Duration::from_millis(500),
        ..fixture.runner()
    };

    let started = Instant::now();
    let report = Fixture::update_with(&repo, RepoUpdatePolicy::Ff, &runner);

    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    let [UpdateOutcome::Failed { step, error, .. }] = outcomes(&report) else {
        panic!("{:?}", outcomes(&report));
    };
    assert_eq!(*step, UpdateStep::Fetch);
    assert_eq!(error, "timed out after 5s");
    assert_eq!(report.fetch, FetchStatus::Failed);
    before.assert_untouched(&fixture.snapshot(&repo));
    // The fake ssh, a grandchild of loomd, was stopped with its process group.
    let pid = fs::read_to_string(&pid_file)
        .unwrap_or_else(|error| panic!("{error}: {report:?}"))
        .trim()
        .to_owned();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Command::new("kill")
        .args(["-0", &pid])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
    {
        assert!(Instant::now() < deadline, "ssh {pid} still runs");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_rejected_authentication_fails_backs_off_its_host_only() {
    let fixture = Fixture::new();
    let denied = fixture.clone("denied");
    let ssh = fixture.script(
        "denying-ssh",
        "echo 'git@auth.invalid: Permission denied (publickey).' >&2\nexit 255",
    );
    fixture.git(
        &denied,
        &["remote", "set-url", "origin", "ssh://auth.invalid/x.git"],
    );
    fixture.git(
        &denied,
        &["config", "core.sshCommand", ssh.to_str().unwrap()],
    );
    let reachable = fixture.clone("reachable");
    fixture.advance("main", "f.txt", "two\n");
    let denied_before = fixture.snapshot(&denied);

    let report = fixture.update(&denied, RepoUpdatePolicy::Ff);
    let [
        UpdateOutcome::Failed {
            step, stderr_tail, ..
        },
    ] = outcomes(&report)
    else {
        panic!("{:?}", outcomes(&report));
    };
    assert_eq!(*step, UpdateStep::Fetch);
    assert!(stderr_tail.contains("Permission denied"), "{stderr_tail}");
    assert_eq!(report.fetch, FetchStatus::Failed);
    denied_before.assert_untouched(&fixture.snapshot(&denied));

    // The schedule backs off `auth.invalid`; the local remote is still updated at once.
    let interval = Duration::from_secs(3_600);
    let mut schedule = Schedule::new(interval);
    let local = fixture.origin_url();
    let candidates = [
        Candidate {
            path: &denied,
            host: "auth.invalid",
            last_update: None,
        },
        Candidate {
            path: &reachable,
            host: &local,
            last_update: None,
        },
    ];
    let now = Instant::now();
    let mut middle = || 0.5;
    let due = schedule.due(&candidates[..1], now, NOW, 4, &mut middle);
    assert_eq!(due, std::slice::from_ref(&denied));
    schedule.start(&denied);
    schedule.finish(&denied, "auth.invalid", now, now, report.fetch, &mut middle);
    assert_eq!(
        schedule.host_backoff("auth.invalid"),
        Some((now + interval * 2, 1))
    );
    let due = schedule.due(&candidates, now, NOW, 4, &mut middle);
    assert_eq!(due, std::slice::from_ref(&reachable));
    let report = fixture.update(&reachable, RepoUpdatePolicy::Ff);
    assert!(matches!(
        outcomes(&report),
        [UpdateOutcome::FastForwarded { .. }]
    ));
}

#[test]
fn ssh_runs_in_batch_mode_unless_the_user_configured_a_command() {
    let fixture = Fixture::new();
    let repo = fixture.clone("work");
    fixture.git(
        &repo,
        &["remote", "set-url", "origin", "ssh://batch.invalid/x.git"],
    );
    // A fake `ssh` first in `PATH` records its arguments.
    let bin = fixture.path("bin");
    fs::create_dir_all(&bin).unwrap();
    let args = fixture.path("ssh-args");
    let ssh = fixture.script(
        "bin/ssh",
        &format!("echo \"$@\" > '{}'\nexit 255", args.display()),
    );
    assert!(ssh.starts_with(&bin));
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&path));
    let mut runner = fixture.runner();
    runner
        .env
        .push(("PATH".into(), Some(std::env::join_paths(paths).unwrap())));

    let report = Fixture::update_with(&repo, RepoUpdatePolicy::Ff, &runner);
    assert_eq!(report.fetch, FetchStatus::Failed);
    let recorded = fs::read_to_string(&args).unwrap();
    assert!(recorded.starts_with("-o BatchMode=yes "), "{recorded}");

    // `core.sshCommand` set: used as is.
    fs::remove_file(&args).unwrap();
    let custom = fixture.script(
        "custom-ssh",
        &format!("echo \"custom $@\" > '{}'\nexit 255", args.display()),
    );
    fixture.git(
        &repo,
        &["config", "core.sshCommand", custom.to_str().unwrap()],
    );
    Fixture::update_with(&repo, RepoUpdatePolicy::Ff, &runner);
    let recorded = fs::read_to_string(&args).unwrap();
    assert!(recorded.starts_with("custom "), "{recorded}");
    assert!(!recorded.contains("BatchMode"), "{recorded}");
}

#[test]
fn a_repository_without_remote_or_unreadable_runs_no_git() {
    let fixture = Fixture::new();
    let repo = fixture.clone("work");
    let runner = GitRunner {
        // Any git command would fail to start.
        program: fixture.path("no-such-git"),
        ..fixture.runner()
    };
    let options = StateOptions {
        config: GitConfig::RepositoryOnly,
        ..StateOptions::default()
    };
    let clock = || Timestamp::from_unix_secs(NOW).unwrap();
    let report = update(
        Target {
            path: &repo,
            slug: "test/work",
            primary_remote: None,
            policy: RepoUpdatePolicy::Ff,
        },
        &runner,
        &options,
        &clock,
    );
    assert_eq!(outcomes(&report), [skipped(SkipReason::NoRemote)]);
    assert_eq!(report.fetch, FetchStatus::NotRun);

    let gone = fixture.path("gone");
    let report = update(
        Target {
            path: &gone,
            slug: "test/gone",
            primary_remote: Some("origin"),
            policy: RepoUpdatePolicy::Ff,
        },
        &runner,
        &options,
        &clock,
    );
    assert!(matches!(
        outcomes(&report),
        [UpdateOutcome::Skipped {
            reason: SkipReason::Unreadable { .. }
        }]
    ));
    // A missing git is a failure of the fetch, not a crash.
    let report = update(
        Target {
            path: &repo,
            slug: "test/work",
            primary_remote: Some("origin"),
            policy: RepoUpdatePolicy::Fetch,
        },
        &runner,
        &options,
        &clock,
    );
    let [UpdateOutcome::Failed { step, error, .. }] = outcomes(&report) else {
        panic!("{:?}", outcomes(&report));
    };
    assert_eq!(*step, UpdateStep::Fetch);
    assert!(error.starts_with("cannot run git"), "{error}");
}
