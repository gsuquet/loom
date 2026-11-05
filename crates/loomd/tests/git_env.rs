//! The forge tokens of `token_env` stay out of the processes that `loomd` starts for git
//! (ADR-0016, decision 6), against a real daemon, without network, in temporary directories.
//!
//! Each test gives `loomd` two token variables and one other variable. A process that git
//! starts writes its environment to a file: a hook in a repository, and a filter or an SSH
//! command in a store, whose git commands disable hooks.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::{Daemon, POLL, request};
use loom_api::RemoteCheckOutcome;
use loom_config::config::parse_config;
use loomd::repo_update::{GitRunner, check_remote};
use tempfile::TempDir;

mod common;

const GLOBAL_TOKEN: &str = "LOOM_TEST_GLOBAL_TOKEN";
const CONTEXT_TOKEN: &str = "LOOM_TEST_CONTEXT_TOKEN";
const TOKEN_VALUE: &str = "not-a-real-token";
/// Stays in the environment: proves that a dump comes from the environment of `loomd`.
const OTHER_VARIABLE: &str = "LOOM_TEST_OTHER_VARIABLE";

/// `[contexts.personal]` overrides the host table, so that no credential of a context names the
/// variable of the host table: it must go as well.
const FORGES: &str = r#"
[contexts.personal.forges."forge.example.invalid"]
token_env = "LOOM_TEST_CONTEXT_TOKEN"

[forges."forge.example.invalid"]
kind = "gitlab"
token_env = "LOOM_TEST_GLOBAL_TOKEN"
"#;

/// A loom home and a user home under `/tmp` (the macOS `$TMPDIR` is too long for a socket path).
struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    /// `context` is the body of `[contexts.personal]`.
    fn new(context: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("loomd-git-env-")
            .tempdir_in("/tmp")
            .unwrap();
        let sandbox = Self { dir };
        fs::create_dir_all(sandbox.loom_home()).unwrap();
        fs::create_dir_all(sandbox.user_home().join("perso")).unwrap();
        fs::create_dir_all(sandbox.bin()).unwrap();
        fs::create_dir_all(sandbox.dumps()).unwrap();
        fs::write(
            sandbox.loom_home().join("config.toml"),
            format!(
                "[repos]\nroots = [\"~/perso\"]\n\n[contexts.personal]\nmatch.paths = [\"~/perso\"]\n{context}\n{FORGES}"
            ),
        )
        .unwrap();
        sandbox.write_gitconfig("");
        sandbox
    }

    fn loom_home(&self) -> PathBuf {
        self.dir.path().join("l")
    }

    fn user_home(&self) -> PathBuf {
        self.dir.path().join("u")
    }

    fn bin(&self) -> PathBuf {
        self.dir.path().join("bin")
    }

    fn dumps(&self) -> PathBuf {
        self.dir.path().join("dumps")
    }

    fn socket(&self) -> PathBuf {
        self.loom_home().join("run/loomd.sock")
    }

    fn write_gitconfig(&self, extra: &str) {
        fs::write(
            self.user_home().join(".gitconfig"),
            format!("[user]\n\tname = Store Owner\n\temail = owner@example.invalid\n{extra}"),
        )
        .unwrap();
    }

    /// An executable at `path` that writes its environment to the dump `dump`, then runs `then`.
    fn dump_script(&self, path: &Path, dump: &str, then: &str) {
        let file = self.dumps().join(dump);
        let file = file.display();
        fs::write(
            path,
            format!("#!/bin/sh\nenv > '{file}.tmp.'$$\nmv '{file}.tmp.'$$ '{file}'\n{then}\n"),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// `ssh` first in the `PATH` of `loomd`: writes the dump `ssh-g` for `ssh -G` and answers
    /// with a host name, and writes the dump `ssh` for a connection, which it refuses.
    fn fake_ssh(&self) {
        let path = self.bin().join("ssh");
        let dumps = self.dumps();
        let dumps = dumps.display();
        fs::write(
            &path,
            format!(
                "#!/bin/sh\n\
                 case \"$1\" in\n\
                 -G) name=ssh-g ;;\n\
                 *) name=ssh ;;\n\
                 esac\n\
                 env > '{dumps}/'$name.tmp.$$\n\
                 mv '{dumps}/'$name.tmp.$$ '{dumps}/'$name\n\
                 if [ \"$name\" = ssh-g ]; then echo 'hostname forge.example.invalid'; exit 0; fi\n\
                 echo 'Permission denied (publickey).' >&2\n\
                 exit 255\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        common::git(&self.user_home(), dir, args);
    }

    /// A clone `~/perso/<name>` of a new bare repository with one commit, returned with the
    /// clone that seeded it.
    fn clone_with_remote(&self, name: &str) -> (PathBuf, PathBuf) {
        let top = self.dir.path();
        let origin = top.join("origin.git");
        self.git(top, &["init", "-q", "--bare", origin.to_str().unwrap()]);
        let url = format!("file://{}", origin.display());
        let seed = top.join("seed");
        self.git(top, &["clone", "-q", &url, seed.to_str().unwrap()]);
        self.push(&seed, "one\n");
        let repo = self.user_home().join("perso").join(name);
        self.git(top, &["clone", "-q", &url, repo.to_str().unwrap()]);
        (repo, seed)
    }

    fn push(&self, seed: &Path, content: &str) {
        fs::write(seed.join("f.txt"), content).unwrap();
        self.git(seed, &["add", "-A"]);
        self.git(seed, &["commit", "-q", "-m", "change"]);
        self.git(seed, &["push", "-q", "origin", "HEAD:main"]);
    }

    fn start(&self) -> Daemon {
        self.start_with(&[])
    }

    /// With these variables set in the environment of `loomd`.
    fn start_with(&self, variables: &[(&str, &Path)]) -> Daemon {
        let mut path = vec![self.bin()];
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let mut command = Command::new(env!("CARGO_BIN_EXE_loomd"));
        command
            .env("LOOM_HOME", self.loom_home())
            .env("HOME", self.user_home())
            .env("XDG_CONFIG_HOME", self.user_home().join(".config"))
            .env("PATH", std::env::join_paths(path).unwrap())
            .env("GIT_CONFIG_GLOBAL", self.user_home().join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(GLOBAL_TOKEN, TOKEN_VALUE)
            .env(CONTEXT_TOKEN, TOKEN_VALUE)
            .env(OTHER_VARIABLE, "kept")
            // No network: the forge is a closed local port, and no poll starts in a test.
            .env("LOOM_TEST_FORGE_API", "http://127.0.0.1:1")
            .env("LOOM_TEST_REVIEW_START_DELAY", "1h")
            .env_remove("GIT_SSH_COMMAND")
            .env_remove("GIT_SSH")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .envs(variables.iter().copied())
            .arg("--color=never")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Daemon::start(&mut command, &self.socket())
    }

    /// Waits for the dump `name`, then checks it: the other variable is there, the tokens are
    /// not.
    fn assert_no_token(&self, name: &str) {
        let path = self.dumps().join(name);
        let deadline = Instant::now() + common::TIMEOUT;
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "no process wrote the dump {name}"
            );
            thread::sleep(POLL);
        }
        let dump = fs::read_to_string(&path).unwrap();
        assert!(
            dump.contains(&format!("{OTHER_VARIABLE}=kept")),
            "{name}: not the environment of loomd"
        );
        for token in [GLOBAL_TOKEN, CONTEXT_TOKEN] {
            assert!(!dump.contains(token), "{name} sees {token}");
        }
        assert!(!dump.contains(TOKEN_VALUE), "{name} sees a token");
    }
}

/// The hooks of an automatic update: the fetch moves a ref, the fast-forward runs `post-merge`.
#[test]
fn hooks_of_an_automatic_update_do_not_see_the_tokens() {
    let sandbox = Sandbox::new("");
    let (repo, seed) = sandbox.clone_with_remote("app");
    let hooks = repo.join(".git/hooks");
    sandbox.dump_script(
        &hooks.join("reference-transaction"),
        "reference-transaction",
        "exit 0",
    );
    sandbox.dump_script(&hooks.join("post-merge"), "post-merge", "exit 0");
    sandbox.push(&seed, "two\n");
    let _daemon = sandbox.start();

    sandbox.assert_no_token("reference-transaction");
    sandbox.assert_no_token("post-merge");
}

/// `git ls-remote` runs no hook: its child is the SSH command. No update runs here.
#[test]
fn the_ssh_command_of_a_remote_check_does_not_see_the_tokens() {
    let sandbox = Sandbox::new("update = \"off\"");
    sandbox.fake_ssh();
    let (repo, _seed) = sandbox.clone_with_remote("app");
    sandbox.git(
        &repo,
        &["remote", "set-url", "origin", "git@auth.invalid:x/y.git"],
    );
    let _daemon = sandbox.start();
    let deadline = Instant::now() + common::TIMEOUT;
    while !sandbox.dumps().join("ssh").exists() {
        assert!(Instant::now() < deadline, "the remote check ran no ssh");
        let response = request(
            &sandbox.socket(),
            "POST",
            "/v1/diagnostics/remote-check",
            None,
        );
        assert_eq!(response.status, 200, "{}", response.body);
        thread::sleep(POLL);
    }

    sandbox.assert_no_token("ssh");
}

/// A store sync runs no hook (`core.hooksPath=/dev/null`): the child of its fetch is the SSH
/// command.
#[test]
fn the_ssh_command_of_a_store_sync_does_not_see_the_tokens() {
    let sandbox = Sandbox::new("store.backup.remote = \"git@auth.invalid:x/y.git\"");
    sandbox.fake_ssh();
    let _daemon = sandbox.start();

    sandbox.assert_no_token("ssh");
}

/// A store commit runs no hook (`core.hooksPath=/dev/null`): the child of its `git add` is a
/// clean filter of the user's git configuration.
#[test]
fn a_filter_of_a_store_commit_does_not_see_the_tokens() {
    let sandbox = Sandbox::new("");
    let filter = sandbox.bin().join("clean-filter");
    sandbox.dump_script(&filter, "clean-filter", "exec cat");
    let attributes = sandbox.user_home().join(".gitattributes");
    fs::write(&attributes, "* filter=dump\n").unwrap();
    sandbox.write_gitconfig(&format!(
        "[core]\n\tattributesFile = {}\n[filter \"dump\"]\n\tclean = {}\n",
        attributes.display(),
        filter.display()
    ));
    let _daemon = sandbox.start();

    let created = request(
        &sandbox.socket(),
        "POST",
        "/v1/tasks",
        Some(&serde_json::json!({
            "title": "Ship it",
            "kind": "chore",
            "context": "personal",
            "status": "ready",
        })),
    );
    assert_eq!(created.status, 200, "{}", created.body);

    sandbox.assert_no_token("clean-filter");
}

/// `ssh -G` runs the `Match exec` commands of the SSH configuration.
#[test]
fn the_ssh_alias_lookup_does_not_see_the_tokens() {
    let sandbox = Sandbox::new("update = \"off\"");
    sandbox.fake_ssh();
    let (repo, _seed) = sandbox.clone_with_remote("app");
    sandbox.git(
        &repo,
        &["remote", "set-url", "origin", "git@myalias:x/y.git"],
    );
    let _daemon = sandbox.start();

    sandbox.assert_no_token("ssh-g");
}

/// A runner keeps the removal when a caller gives `env` over it, also for a variable set there.
#[test]
fn a_runner_removes_the_tokens_over_its_env() {
    let sandbox = Sandbox::new("");
    let config = parse_config(
        &format!("[contexts.personal]\n{FORGES}"),
        Path::new("config.toml"),
    )
    .unwrap();
    let (repo, _seed) = sandbox.clone_with_remote("app");
    sandbox.git(
        &repo,
        &["remote", "set-url", "origin", "git@auth.invalid:x/y.git"],
    );
    let ssh = sandbox.bin().join("repo-ssh");
    sandbox.dump_script(&ssh, "repo-ssh", "exit 255");
    sandbox.git(&repo, &["config", "core.sshCommand", ssh.to_str().unwrap()]);
    let set = |name: &str, value: &str| (name.into(), Some(value.into()));
    let runner = GitRunner {
        env: vec![
            set("GIT_CONFIG_GLOBAL", "/dev/null"),
            set("GIT_CONFIG_NOSYSTEM", "1"),
            set(GLOBAL_TOKEN, TOKEN_VALUE),
            set(CONTEXT_TOKEN, TOKEN_VALUE),
            set(OTHER_VARIABLE, "kept"),
        ],
        ..GitRunner::from_config(&config)
    };

    let outcome = check_remote(&repo, "origin", &runner, Duration::from_secs(20));

    assert!(
        matches!(outcome, RemoteCheckOutcome::Failed { .. }),
        "{outcome:?}"
    );
    sandbox.assert_no_token("repo-ssh");
}

/// git never gets these variables (`REPOSITORY_VARIABLES`): a state read must not use them
/// either. `gix` reads some of them from the environment of the process.
#[test]
fn the_repository_variables_of_loomd_do_not_change_a_state_read() {
    let sandbox = Sandbox::new("update = \"off\"");
    let (repo, _seed) = sandbox.clone_with_remote("app");
    // A submodule with an edit: `Known(1)` needs the index, the objects and the working tree.
    let top = sandbox.dir.path();
    let sub = top.join("sub-origin");
    sandbox.git(top, &["init", "-q", sub.to_str().unwrap()]);
    fs::write(sub.join("s.txt"), "one\n").unwrap();
    sandbox.git(&sub, &["add", "-A"]);
    sandbox.git(&sub, &["commit", "-q", "-m", "first"]);
    sandbox.git(
        &repo,
        &["submodule", "add", "-q", sub.to_str().unwrap(), "sub"],
    );
    sandbox.git(&repo, &["commit", "-q", "-m", "submodule"]);
    fs::write(repo.join("sub/s.txt"), "two\n").unwrap();
    let elsewhere = top.join("elsewhere");
    sandbox.git(top, &["init", "-q", elsewhere.to_str().unwrap()]);
    let dot_git = elsewhere.join(".git");
    let objects = dot_git.join("objects");
    let index = dot_git.join("no-index");
    let namespace = PathBuf::from("other");

    for (name, value) in [
        ("GIT_DIR", &dot_git),
        ("GIT_WORK_TREE", &elsewhere),
        ("GIT_INDEX_FILE", &index),
        ("GIT_OBJECT_DIRECTORY", &objects),
        ("GIT_ALTERNATE_OBJECT_DIRECTORIES", &objects),
        ("GIT_COMMON_DIR", &dot_git),
        ("GIT_NAMESPACE", &namespace),
        ("GIT_PREFIX", &elsewhere),
    ] {
        let daemon = sandbox.start_with(&[(name, value.as_path())]);
        let deadline = Instant::now() + common::TIMEOUT;
        let body = loop {
            let response = request(
                &sandbox.socket(),
                "GET",
                "/v1/repos/personal/app?refresh=true",
                None,
            );
            if response.status == 200 {
                break response.body;
            }
            assert!(Instant::now() < deadline, "{name}: {}", response.body);
            thread::sleep(POLL);
        };
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        let state = &body["state"];
        assert_eq!(state["dirty"]["known"], 1, "{name}: {state}");
        assert_eq!(state["untracked"]["known"]["count"], 0, "{name}: {state}");
        assert_eq!(state["head"]["known"]["name"], "main", "{name}: {state}");
        daemon.stop();
    }
}
