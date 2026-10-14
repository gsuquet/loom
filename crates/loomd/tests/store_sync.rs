//! Sync of context stores with their backup remote, against local bare repositories in a
//! temporary directory: push, fast-forward of an empty or behind store, a refused push, a
//! divergence that is never merged, an unreachable remote. Hermetic: a temporary `HOME` whose
//! only git configuration is a `.gitconfig` with an identity.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use loom_api::{BackupOutcome, BackupStep, ContextName};
use loomd::repo_update::GitRunner;
use loomd::store_git::{LocalStep, StoreCommits, sync};
use tempfile::TempDir;

mod common;

use common::git;

struct Fixture {
    dir: TempDir,
}

/// One machine: its store and its commits.
struct Machine {
    root: PathBuf,
    commits: StoreCommits,
}

impl Fixture {
    /// A bare `remote.git` and a home with a git identity.
    fn new() -> Self {
        let fixture = Self {
            dir: TempDir::new().unwrap(),
        };
        fs::create_dir_all(fixture.home()).unwrap();
        fs::write(
            fixture.home().join(".gitconfig"),
            "[user]\n\tname = Store Owner\n\temail = owner@example.invalid\n",
        )
        .unwrap();
        git(
            &fixture.home(),
            fixture.dir.path(),
            &["init", "-q", "--bare", "remote.git"],
        );
        fixture
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn remote(&self) -> PathBuf {
        self.dir.path().join("remote.git")
    }

    fn url(&self) -> String {
        self.remote().to_str().unwrap().to_owned()
    }

    fn runner(&self) -> GitRunner {
        let mut runner = GitRunner::from_env();
        runner.env = vec![
            (OsString::from("HOME"), Some(self.home().into())),
            (
                OsString::from("XDG_CONFIG_HOME"),
                Some(self.home().join(".config").into()),
            ),
            (
                OsString::from("GIT_CONFIG_GLOBAL"),
                Some(self.home().join(".gitconfig").into()),
            ),
            (OsString::from("GIT_CONFIG_NOSYSTEM"), Some("1".into())),
        ];
        runner
    }

    /// The store `name` of a machine, with an empty `tasks/`.
    fn machine(&self, name: &str) -> Machine {
        let root = self.dir.path().join(name);
        fs::create_dir_all(root.join("tasks")).unwrap();
        Machine {
            commits: StoreCommits::new(self.runner(), BTreeMap::from([(work(), root.clone())])),
            root,
        }
    }

    /// Syncs `machine` with the remote, its local steps run directly.
    fn sync(&self, machine: &mut Machine) -> BackupOutcome {
        let commits = &mut machine.commits;
        sync(
            &self.runner(),
            &machine.root,
            &self.url(),
            &mut |step| match step {
                LocalStep::Prepare => {
                    commits.prepare_sync(&work(), "sync(work): uncommitted changes")
                }
                LocalStep::FastForward { to } => commits.fast_forward(&work(), to),
            },
        )
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        git(&self.home(), dir, args)
    }

    /// The commit of `main` on the remote; `None` when it has none.
    fn remote_main(&self) -> Option<String> {
        let output = common::git_run(
            &self.home(),
            &self.remote(),
            &["rev-parse", "--verify", "--quiet", "refs/heads/main"],
        );
        output
            .status
            .success()
            .then(|| String::from_utf8(output.stdout).unwrap().trim().to_owned())
    }
}

impl Machine {
    /// Writes a task file `id` with `text` and commits it.
    fn write(&mut self, id: &str, text: &str) {
        let dir = self.root.join("tasks").join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("task.md"), text).unwrap();
        self.commits
            .commit_task(&work(), &id.parse().unwrap(), &format!("edited({id}): x"))
            .unwrap();
    }

    fn head(&self, fixture: &Fixture) -> String {
        fixture
            .git(&self.root, &["rev-parse", "HEAD"])
            .trim()
            .to_owned()
    }
}

fn work() -> ContextName {
    "work".parse().unwrap()
}

#[test]
fn commits_are_pushed_and_pulled_into_an_empty_store() {
    let fixture = Fixture::new();
    let mut laptop = fixture.machine("laptop");
    laptop.write("aaaaaaaa", "one\n");

    assert_eq!(
        fixture.sync(&mut laptop),
        BackupOutcome::Pushed { commits: 1 }
    );
    assert_eq!(fixture.remote_main(), Some(laptop.head(&fixture)));
    assert_eq!(fixture.sync(&mut laptop), BackupOutcome::UpToDate);
    assert_eq!(
        fixture.git(&laptop.root, &["remote", "get-url", "backup"]),
        format!("{}\n", fixture.url())
    );

    // A new machine restores the store from the remote.
    let mut tower = fixture.machine("tower");
    assert_eq!(
        fixture.sync(&mut tower),
        BackupOutcome::Pulled { commits: 1 }
    );
    assert_eq!(
        fs::read_to_string(tower.root.join("tasks/aaaaaaaa/task.md")).unwrap(),
        "one\n"
    );
    assert_eq!(tower.head(&fixture), laptop.head(&fixture));

    // A change on one machine reaches the other.
    tower.write("bbbbbbbb", "two\n");
    assert_eq!(
        fixture.sync(&mut tower),
        BackupOutcome::Pushed { commits: 1 }
    );
    assert_eq!(
        fixture.sync(&mut laptop),
        BackupOutcome::Pulled { commits: 1 }
    );
    assert_eq!(
        fs::read_to_string(laptop.root.join("tasks/bbbbbbbb/task.md")).unwrap(),
        "two\n"
    );
    assert_eq!(fixture.git(&laptop.root, &["status", "--porcelain"]), "");
}

#[test]
fn a_store_without_commits_and_an_empty_remote_are_up_to_date() {
    let fixture = Fixture::new();
    let mut laptop = fixture.machine("laptop");
    assert_eq!(fixture.sync(&mut laptop), BackupOutcome::UpToDate);
    assert_eq!(fixture.remote_main(), None);
}

#[test]
fn a_refused_push_keeps_the_commits_local() {
    let fixture = Fixture::new();
    let hook = fixture.remote().join("hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\necho 'no backups here' >&2\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let mut laptop = fixture.machine("laptop");
    laptop.write("aaaaaaaa", "one\n");
    let head = laptop.head(&fixture);

    let outcome = fixture.sync(&mut laptop);
    let BackupOutcome::PushRejected { stderr_tail } = &outcome else {
        panic!("{outcome:?}");
    };
    assert!(stderr_tail.contains("[remote rejected]"), "{stderr_tail}");
    assert!(outcome.failed());
    assert_eq!(laptop.head(&fixture), head);
    assert_eq!(fixture.remote_main(), None);
    assert_eq!(fixture.git(&laptop.root, &["status", "--porcelain"]), "");
}

#[test]
fn a_divergence_is_reported_and_never_merged() {
    let fixture = Fixture::new();
    let mut laptop = fixture.machine("laptop");
    laptop.write("aaaaaaaa", "one\n");
    fixture.sync(&mut laptop);
    let mut tower = fixture.machine("tower");
    fixture.sync(&mut tower);

    laptop.write("bbbbbbbb", "laptop\n");
    tower.write("cccccccc", "tower\n");
    tower.write("cccccccc", "tower again\n");
    assert_eq!(
        fixture.sync(&mut laptop),
        BackupOutcome::Pushed { commits: 1 }
    );
    let tower_head = tower.head(&fixture);
    let remote_head = fixture.remote_main();

    for _ in 0..2 {
        assert_eq!(
            fixture.sync(&mut tower),
            BackupOutcome::Diverged {
                local: 2,
                remote: 1
            }
        );
    }
    // Nothing moved on either side, no merge, the working tree as it was.
    assert_eq!(tower.head(&fixture), tower_head);
    assert_eq!(fixture.remote_main(), remote_head);
    assert_eq!(fixture.git(&tower.root, &["status", "--porcelain"]), "");
    assert!(!tower.root.join("tasks/bbbbbbbb").exists());
    assert_eq!(
        fixture.git(&tower.root, &["rev-list", "--merges", "--count", "HEAD"]),
        "0\n"
    );
}

#[test]
fn an_unreachable_remote_fails_the_fetch() {
    let fixture = Fixture::new();
    let mut laptop = fixture.machine("laptop");
    laptop.write("aaaaaaaa", "one\n");
    let head = laptop.head(&fixture);
    let outcome = sync(
        &fixture.runner(),
        &laptop.root,
        fixture.dir.path().join("missing.git").to_str().unwrap(),
        &mut |_| Ok(()),
    );
    assert!(
        matches!(
            &outcome,
            BackupOutcome::Failed {
                step: BackupStep::Fetch,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(laptop.head(&fixture), head);
}

#[test]
fn a_store_with_uncommitted_changes_is_not_fast_forwarded() {
    let fixture = Fixture::new();
    let mut laptop = fixture.machine("laptop");
    laptop.write("aaaaaaaa", "one\n");
    fixture.sync(&mut laptop);
    let mut tower = fixture.machine("tower");
    fixture.sync(&mut tower);
    laptop.write("aaaaaaaa", "two\n");
    fixture.sync(&mut laptop);

    // A change the commit before the sync could not take: another git process holds the index.
    fs::write(tower.root.join(".git/index.lock"), "").unwrap();
    fs::write(tower.root.join("tasks/aaaaaaaa/task.md"), "local\n").unwrap();
    let outcome = fixture.sync(&mut tower);
    assert!(
        matches!(
            &outcome,
            BackupOutcome::Failed {
                step: BackupStep::Commit,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        fs::read_to_string(tower.root.join("tasks/aaaaaaaa/task.md")).unwrap(),
        "local\n"
    );
    assert!(tower.commits.pending().contains_key(&work()));
}
