//! `loom shell-init`: the snippets printed by the real `loom`, sourced in real `zsh` and `bash`
//! with a stub `loom` on `PATH` that logs its arguments. A shell missing on the runner is
//! skipped with a message.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

mod isolation;

/// Arguments the hook passes to `loom`.
const HOOK_ARGS: &str = "repo register --on-use --background";
/// Longest wait for the detached calls.
const TIMEOUT: Duration = Duration::from_secs(10);
/// Most a directory change may cost with the hook.
const MAX_OVERHEAD: Duration = Duration::from_millis(5);

fn loom_bin() -> PathBuf {
    PathBuf::from(assert_cmd::cargo::cargo_bin!("loom"))
}

#[derive(Debug, Clone, Copy)]
enum Shell {
    Zsh,
    Bash,
}

impl Shell {
    const fn name(self) -> &'static str {
        match self {
            Self::Zsh => "zsh",
            Self::Bash => "bash",
        }
    }

    /// The shell's executable in `PATH`, `None` (with a message) when there is none.
    fn find(self) -> Option<PathBuf> {
        let found = env::var_os("PATH").and_then(|path| {
            env::split_paths(&path)
                .map(|dir| dir.join(self.name()))
                .find(|candidate| candidate.is_file())
        });
        if found.is_none() {
            eprintln!("SKIPPED: {} is not installed on this runner", self.name());
        }
        found
    }

    /// What runs after each directory change: bash runs `PROMPT_COMMAND` at each prompt, which
    /// a script has none of; zsh runs its `chpwd` hooks in `cd` itself.
    const fn prompt(self) -> &'static str {
        match self {
            Self::Zsh => ":",
            Self::Bash => "eval \"$PROMPT_COMMAND\"",
        }
    }

    /// Arguments that start the shell without any startup file.
    const fn no_rc(self) -> &'static [&'static str] {
        match self {
            Self::Zsh => &["-f"],
            Self::Bash => &["--norc", "--noprofile"],
        }
    }

    /// Arguments that make the shell interactive, as in a terminal, where a job started with
    /// a plain `&` would print notices. Not bash: without a terminal, it prints `no job
    /// control in this shell`.
    const fn interactive(self) -> &'static [&'static str] {
        match self {
            Self::Zsh => &["-i"],
            Self::Bash => &[],
        }
    }
}

/// A temporary user home with the snippet of a shell, a stub `loom` and a log of its calls.
struct Fixture {
    dir: TempDir,
    shell: Shell,
    exe: PathBuf,
}

impl Fixture {
    fn new(shell: Shell) -> Option<Self> {
        let exe = shell.find()?;
        let fixture = Self {
            dir: tempfile::tempdir().unwrap(),
            shell,
            exe,
        };
        let output = Command::new(loom_bin())
            .envs(isolation::isolated_env(&fixture.home()))
            .args(["shell-init", shell.name()])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        fs::create_dir_all(fixture.home()).unwrap();
        fs::write(fixture.snippet(), &output.stdout).unwrap();
        let bin = fixture.bin();
        fs::create_dir_all(&bin).unwrap();
        let stub = format!(
            "#!/bin/sh\nprintf '%s|%s\\n' \"$PWD\" \"$*\" >> '{}'\n",
            fixture.log().display()
        );
        fs::write(bin.join("loom"), stub).unwrap();
        fs::set_permissions(bin.join("loom"), fs::Permissions::from_mode(0o755)).unwrap();
        Some(fixture)
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn snippet(&self) -> PathBuf {
        self.dir.path().join("snippet")
    }

    fn bin(&self) -> PathBuf {
        self.dir.path().join("bin")
    }

    fn log(&self) -> PathBuf {
        self.dir.path().join("calls.log")
    }

    /// Creates the directories `names` under the home.
    fn dirs(&self, names: &[&str]) -> Vec<PathBuf> {
        names
            .iter()
            .map(|name| {
                let dir = self.home().join(name);
                fs::create_dir_all(&dir).unwrap();
                dir
            })
            .collect()
    }

    /// Runs `script` in the shell, started in the home, without startup files, with an
    /// environment of its own: `PATH` is the stub's directory (when `stub`) and the system's.
    fn run(&self, script: &str, stub: bool) -> Output {
        let path = if stub {
            format!("{}:/usr/bin:/bin", self.bin().display())
        } else {
            "/usr/bin:/bin".to_owned()
        };
        Command::new(&self.exe)
            .env_clear()
            .envs(isolation::isolated_env(&self.home()))
            .env("PATH", path)
            .env("PWD", self.home())
            .env("SNIPPET", self.snippet())
            .current_dir(self.home())
            .args(self.shell.no_rc())
            .args(self.shell.interactive())
            .arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// The calls logged so far: (directory, arguments).
    fn calls(&self) -> Vec<(String, String)> {
        fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let (dir, args) = line.split_once('|').unwrap();
                (dir.to_owned(), args.to_owned())
            })
            .collect()
    }

    /// The calls, once `expected` of them were logged (or the timeout passed) and no other
    /// came for a while.
    fn settled_calls(&self, expected: usize) -> Vec<(String, String)> {
        let deadline = Instant::now() + TIMEOUT;
        while self.calls().len() < expected && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        thread::sleep(Duration::from_millis(300));
        self.calls()
    }
}

/// Evaluates the snippet as a startup file does.
const EVAL_SNIPPET: &str = "eval \"$(cat \"$SNIPPET\")\"\n";

fn assert_silent(output: &Output) {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        output.stdout.is_empty() && output.stderr.is_empty(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn each_shell(test: impl Fn(Fixture)) {
    for shell in [Shell::Zsh, Shell::Bash] {
        if let Some(fixture) = Fixture::new(shell) {
            test(fixture);
        }
    }
}

#[test]
fn snippets_are_valid_syntax() {
    each_shell(|fixture| {
        let output = Command::new(&fixture.exe)
            .env_clear()
            .args(fixture.shell.no_rc())
            .arg("-n")
            .arg(fixture.snippet())
            .output()
            .unwrap();
        assert_silent(&output);
    });
}

#[test]
fn each_directory_is_registered_once_per_session() {
    each_shell(|fixture| {
        let shell = fixture.shell;
        let dirs = fixture.dirs(&["a", "b"]);
        let (a, b) = (dirs[0].display(), dirs[1].display());
        let prompt = shell.prompt();
        // Starts in `a`, enters `b`, then goes back and forth.
        let script = format!(
            "cd '{a}'\n{EVAL_SNIPPET}{prompt}\ncd '{b}'\n{prompt}\ncd '{a}'\n{prompt}\ncd '{b}'\n{prompt}\ncd '{a}'\n{prompt}\n"
        );
        let output = fixture.run(&script, true);
        assert_silent(&output);
        let calls = fixture.settled_calls(2);
        let mut dirs: Vec<String> = calls.iter().map(|(dir, _)| dir.clone()).collect();
        dirs.sort();
        assert_eq!(dirs, [a.to_string(), b.to_string()], "{shell:?}");
        assert!(
            calls.iter().all(|(_, args)| args == HOOK_ARGS),
            "{shell:?}: {calls:?}"
        );
    });
}

#[test]
fn the_hook_adds_under_5_ms_to_a_directory_change() {
    const CHANGES: u32 = 100;
    each_shell(|fixture| {
        let shell = fixture.shell;
        let names: Vec<String> = (0..CHANGES).map(|i| format!("d/{i}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut changes = String::new();
        for dir in fixture.dirs(&names) {
            // A directory never seen: the hook starts `loom` each time, the worst case.
            writeln!(changes, "cd '{}'\n{}", dir.display(), shell.prompt()).unwrap();
        }
        let prompt = shell.prompt();
        let base = format!("cd \"$HOME\"\n{prompt}\n{changes}");
        let hooked = format!("{EVAL_SNIPPET}cd \"$HOME\"\n{prompt}\n{changes}");
        // The fastest of several runs: the least disturbed by the rest of the machine.
        let fastest = |script: &str| {
            (0..5)
                .map(|_| {
                    let started = Instant::now();
                    let output = fixture.run(script, true);
                    let elapsed = started.elapsed();
                    assert_silent(&output);
                    elapsed
                })
                .min()
                .unwrap()
        };
        let without = fastest(&base);
        let with = fastest(&hooked);
        let overhead = with.saturating_sub(without) / CHANGES;
        println!(
            "{}: {overhead:?} per directory change with the hook ({CHANGES} changes: {with:?} with, {without:?} without)",
            shell.name()
        );
        assert!(overhead < MAX_OVERHEAD, "{shell:?}: {overhead:?}");
        // Each run with the hook called `loom` once per directory, and the initial one.
        let calls = fixture.settled_calls(5 * (CHANGES as usize + 1));
        assert_eq!(calls.len(), 5 * (CHANGES as usize + 1), "{shell:?}");
    });
}

#[test]
fn nothing_happens_without_loom() {
    each_shell(|fixture| {
        let shell = fixture.shell;
        let dirs = fixture.dirs(&["a", "b"]);
        let prompt = shell.prompt();
        let check = match shell {
            Shell::Zsh => "if (( $+functions[_loom_chpwd] )); then echo defined; fi",
            Shell::Bash => "if declare -F __loom_prompt >/dev/null; then echo defined; fi",
        };
        // Not on `PATH` when the snippet is evaluated: nothing is defined.
        let script = format!(
            "{}{check}\ncd '{}'\n{prompt}\ncd '{}'\n{prompt}\n",
            EVAL_SNIPPET,
            dirs[0].display(),
            dirs[1].display()
        );
        assert_silent(&fixture.run(&script, false));

        // Removed after: the hook runs, silently. The stub is removed once it logged the
        // start directory.
        let wait = format!(
            "i=0; while [ ! -s '{}' ] && [ $i -lt 200 ]; do sleep 0.05; i=$((i+1)); done",
            fixture.log().display()
        );
        let script = format!(
            "{}{prompt}\n{wait}\nrm '{}'\ncd '{}'\n{prompt}\ncd '{}'\n{prompt}\n",
            EVAL_SNIPPET,
            fixture.bin().join("loom").display(),
            dirs[0].display(),
            dirs[1].display()
        );
        assert_silent(&fixture.run(&script, true));
        // Only the start directory, before the stub was removed.
        let calls = fixture.settled_calls(1);
        assert_eq!(calls.len(), 1, "{shell:?}: {calls:?}");
    });
}

#[test]
fn bash_keeps_prompt_command_and_the_exit_status() {
    let Some(fixture) = Fixture::new(Shell::Bash) else {
        return;
    };
    // An existing value ending with `;`, and the snippet evaluated twice; the hook returns
    // the status it was given, for the prompt commands after it.
    let script = format!(
        "PROMPT_COMMAND='pre=$((pre+1));'\n{EVAL_SNIPPET}{EVAL_SNIPPET}eval \"$PROMPT_COMMAND\"\nfalse\n__loom_prompt\necho \"status=$? pre=$pre\"\nprintf '%s\\n' \"$PROMPT_COMMAND\"\n"
    );
    let output = fixture.run(&script, true);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "status=1 pre=1\npre=$((pre+1));\n__loom_prompt\n",
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn shell_init_prints_the_snippet_and_rejects_other_shells() {
    let home = isolation::shared_home();
    let run = |args: &[&str]| {
        Command::new(loom_bin())
            .envs(isolation::isolated_env(&home))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let zsh = run(&["shell-init", "zsh"]);
    assert_eq!(zsh.status.code(), Some(0));
    let text = String::from_utf8(zsh.stdout).unwrap();
    assert!(text.contains("add-zsh-hook chpwd"), "{text}");
    let bash = run(&["shell-init", "bash"]);
    assert!(
        String::from_utf8(bash.stdout)
            .unwrap()
            .contains("PROMPT_COMMAND")
    );
    let fish = run(&["shell-init", "fish"]);
    assert_eq!(fish.status.code(), Some(2));
}

#[test]
fn loom_go_changes_directory_in_shell() {
    each_shell(|fixture| {
        let shell = fixture.shell;
        let target_dir = fixture.dirs(&["repo_dir"])[0].clone();
        let target_str = target_dir.display().to_string();

        // Stub loom: handles `go` by writing the target directory to LOOM_CD_FILE.
        let stub = format!(
            r#"#!/bin/sh
for arg in "$@"; do
    if [ "$arg" = "go" ]; then
        for check in "$@"; do
            if [ "$check" = "fail" ]; then
                exit 42
            fi
        done
        if [ -n "$LOOM_CD_FILE" ]; then
            printf '%s\n' "{target_str}" > "$LOOM_CD_FILE"
        fi
        exit 0
    fi
done
if [ -n "$LOOM_CD_FILE" ] && [ "$SIMULATE_TUI_GO" = 1 ]; then
    printf '%s\n' "{target_str}" > "$LOOM_CD_FILE"
fi
exit 0
"#
        );
        fs::write(fixture.bin().join("loom"), stub).unwrap();
        fs::set_permissions(
            fixture.bin().join("loom"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        // 1. Nominal `loom go <id>` changes directory
        let script = format!("{EVAL_SNIPPET}loom go 123\npwd\n");
        let output = fixture.run(&script, true);
        assert_eq!(output.status.code(), Some(0), "{shell:?}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(stdout.trim(), target_str, "{shell:?}: {stdout}");

        // 2. `loom -c work go <id>` also changes directory (global options before go)
        let script = format!("{EVAL_SNIPPET}cd \"$HOME\"\nloom -c work go 123\npwd\n");
        let output = fixture.run(&script, true);
        assert_eq!(output.status.code(), Some(0), "{shell:?}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(stdout.trim(), target_str, "{shell:?}: {stdout}");

        // 3. Failing `loom go` preserves exit status and does not change directory
        let script =
            format!("{EVAL_SNIPPET}cd \"$HOME\"\nloom go fail\nrc=$?\npwd\necho \"rc=$rc\"\n");
        let output = fixture.run(&script, true);
        assert_eq!(output.status.code(), Some(0), "{shell:?}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("rc=42"), "{shell:?}: {stdout}");
        assert!(
            stdout.contains(&fixture.home().display().to_string()),
            "{shell:?}: {stdout}"
        );

        // 4. `loom` with no subcommand (TUI) provisions LOOM_CD_FILE and changes directory when g is pressed
        let script = format!("{EVAL_SNIPPET}cd \"$HOME\"\nSIMULATE_TUI_GO=1 loom\npwd\n");
        let output = fixture.run(&script, true);
        assert_eq!(output.status.code(), Some(0), "{shell:?}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(stdout.trim(), target_str, "{shell:?}: {stdout}");

        // 5. `loom -c work` with no subcommand (TUI) provisions LOOM_CD_FILE and changes directory
        let script = format!("{EVAL_SNIPPET}cd \"$HOME\"\nSIMULATE_TUI_GO=1 loom -c work\npwd\n");
        let output = fixture.run(&script, true);
        assert_eq!(output.status.code(), Some(0), "{shell:?}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(stdout.trim(), target_str, "{shell:?}: {stdout}");

        // 6. `loom --help` is not the TUI: no LOOM_CD_FILE, no directory change
        let script = format!("{EVAL_SNIPPET}cd \"$HOME\"\nSIMULATE_TUI_GO=1 loom --help\npwd\n");
        let output = fixture.run(&script, true);
        assert_eq!(output.status.code(), Some(0), "{shell:?}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            stdout.trim(),
            fixture.home().display().to_string(),
            "{shell:?}: {stdout}"
        );
    });
}
