//! `loom service` end to end, limited to paths that run no service manager command.
//!
//! Every test points `HOME`, `LOOM_HOME` and `XDG_CONFIG_HOME` at a temporary directory and
//! empties `PATH`, so that a regression cannot reach `launchctl` or `systemctl` on the host.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use tempfile::TempDir;

fn loom(sandbox: &Path) -> Command {
    let empty_path = sandbox.join("empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("loom"));
    command
        .env_clear()
        .env("HOME", sandbox.join("home"))
        .env("LOOM_HOME", sandbox.join("loom"))
        .env("XDG_CONFIG_HOME", sandbox.join("xdg"))
        .env("PATH", empty_path);
    command
}

fn sandbox_with_config(config: &str) -> TempDir {
    let sandbox = tempfile::tempdir().unwrap();
    fs::create_dir_all(sandbox.path().join("loom")).unwrap();
    fs::write(sandbox.path().join("loom/config.toml"), config).unwrap();
    sandbox
}

#[test]
fn install_is_refused_when_the_service_is_disabled() {
    let sandbox = sandbox_with_config("[daemon]\nservice = false\n");
    for args in [
        ["service", "install"].as_slice(),
        ["service", "install", "--dry-run"].as_slice(),
    ] {
        let assert = loom(sandbox.path()).args(args).assert().code(1);
        let output = assert.get_output();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("[daemon] service = false"), "{stderr}");
        let config = sandbox.path().join("loom/config.toml");
        assert!(stderr.contains(&*config.to_string_lossy()), "{stderr}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    }
    // Nothing written.
    assert!(!sandbox.path().join("home/Library").exists());
    assert!(!sandbox.path().join("xdg/systemd").exists());
    assert!(!sandbox.path().join("loom/log").exists());
}

#[test]
fn invalid_configuration_fails_install() {
    let sandbox = sandbox_with_config("[daemon]\nservice = \"no\"\n");
    let assert = loom(sandbox.path())
        .args(["service", "install"])
        .assert()
        .code(1);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.contains("invalid configuration"), "{stderr}");
}

#[test]
fn service_usage_errors() {
    let sandbox = tempfile::tempdir().unwrap();
    loom(sandbox.path()).arg("service").assert().code(2);
    loom(sandbox.path())
        .args(["service", "status", "--dry-run"])
        .assert()
        .code(2);
    loom(sandbox.path())
        .args(["service", "reload"])
        .assert()
        .code(2);
}

/// Fake `id`, `launchctl`, `systemctl` and `loomd`, alone on `PATH`. Each logs its arguments
/// to `calls.log`; loading creates `loaded` and `running`, unloading removes them, queries
/// read them.
const FAKE_MANAGER: &str = r#"#!/bin/sh
dir=$(cd "${0%/*}" && pwd)
echo "${0##*/} $*" >> "$dir/calls.log"
case "${0##*/} $*" in
  "id -u") echo 501 ;;
  "launchctl print"*)
    [ -e "$dir/loaded" ] || exit 3
    if [ -e "$dir/running" ]; then echo "	state = running"; else echo "	state = not running"; fi ;;
  "systemctl --user is-enabled"*) [ -e "$dir/loaded" ] || exit 3 ;;
  "systemctl --user is-active"*) [ -e "$dir/running" ] || exit 3 ;;
  "launchctl bootstrap"*|"systemctl --user enable --now"*) : > "$dir/loaded"; : > "$dir/running" ;;
  "launchctl bootout"*|"systemctl --user disable --now"*) /bin/rm -f "$dir/loaded" "$dir/running" ;;
esac
"#;

/// `kill` on `PATH`: logs its arguments to `calls.log`, then runs the real one.
const LOGGING_KILL: &str = r#"#!/bin/sh
dir=$(cd "${0%/*}" && pwd)
echo "kill $*" >> "$dir/calls.log"
exec /bin/kill "$@"
"#;

/// Test hook of `loom service restart`: how long it waits for a new loomd, in milliseconds.
const ANSWER_TIMEOUT_ENV: &str = "LOOM_TEST_RESTART_ANSWER_TIMEOUT_MS";

struct FakeHost {
    sandbox: TempDir,
}

impl FakeHost {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        let sandbox = tempfile::tempdir().unwrap();
        let bin = sandbox.path().join("empty-path");
        fs::create_dir_all(&bin).unwrap();
        for name in ["id", "launchctl", "systemctl", "loomd"] {
            let path = bin.join(name);
            fs::write(&path, FAKE_MANAGER).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let kill = bin.join("kill");
        fs::write(&kill, LOGGING_KILL).unwrap();
        fs::set_permissions(&kill, fs::Permissions::from_mode(0o755)).unwrap();
        Self { sandbox }
    }

    fn loom(&self, args: &[&str]) -> assert_cmd::assert::Assert {
        loom(self.sandbox.path()).args(args).assert()
    }

    /// `loom service restart`, waiting at most half a second for a new loomd.
    fn restart(&self) -> assert_cmd::assert::Assert {
        loom(self.sandbox.path())
            .env(ANSWER_TIMEOUT_ENV, "500")
            .args(["service", "restart"])
            .assert()
    }

    /// Commands that change state run since the last call (read-only queries left out).
    fn take_changes(&self) -> Vec<String> {
        let log = self.sandbox.path().join("empty-path/calls.log");
        let calls = fs::read_to_string(&log).unwrap_or_default();
        fs::remove_file(&log).ok();
        calls
            .lines()
            .filter(|call| {
                !call.starts_with("id ")
                    && !call.starts_with("launchctl print")
                    && !call.contains(" is-enabled ")
                    && !call.contains(" is-active ")
            })
            .map(str::to_owned)
            .collect()
    }

    fn status(&self) -> serde_json::Value {
        let assert = self.loom(&["service", "status", "--json"]).success();
        serde_json::from_slice(&assert.get_output().stdout).unwrap()
    }
}

fn stdout(assert: &assert_cmd::assert::Assert) -> String {
    String::from_utf8_lossy(&assert.get_output().stdout).into_owned()
}

fn flags(status: &serde_json::Value) -> [bool; 3] {
    ["installed", "loaded", "running"].map(|key| status[key].as_bool().unwrap())
}

#[test]
fn install_status_reinstall_uninstall_with_a_fake_manager() {
    let host = FakeHost::new();
    let status = host.status();
    assert_eq!(flags(&status), [false, false, false]);
    let unit = std::path::PathBuf::from(status["unit_path"].as_str().unwrap());
    let sandbox = fs::canonicalize(host.sandbox.path()).unwrap();
    assert!(
        unit.starts_with(host.sandbox.path()) || unit.starts_with(&sandbox),
        "{}",
        unit.display()
    );
    host.take_changes();

    // Dry run: prints the plan, changes nothing.
    let dry = host.loom(&["service", "install", "-n"]).success();
    assert!(
        stdout(&dry).contains(&format!("write {}:", unit.display())),
        "{}",
        stdout(&dry)
    );
    assert_eq!(host.take_changes(), Vec::<String>::new());
    assert!(!unit.exists());

    let first = host.loom(&["service", "install"]).success();
    assert_eq!(
        stdout(&first),
        format!("loomd service installed: {}\n", unit.display())
    );
    assert_ne!(host.take_changes(), Vec::<String>::new());
    assert!(unit.is_file());
    assert_eq!(flags(&host.status()), [true, true, true]);
    host.take_changes();

    let second = host.loom(&["service", "install"]).success();
    assert_eq!(
        stdout(&second),
        format!("loomd service already installed: {}\n", unit.display())
    );
    assert_eq!(host.take_changes(), Vec::<String>::new());

    let removed = host.loom(&["service", "uninstall"]).success();
    assert_eq!(
        stdout(&removed),
        format!("loomd service uninstalled: {}\n", unit.display())
    );
    assert!(!unit.exists());
    assert_eq!(flags(&host.status()), [false, false, false]);
    host.take_changes();

    let again = host.loom(&["service", "uninstall"]).success();
    assert!(stdout(&again).starts_with("loomd service not installed: "));
    assert_eq!(host.take_changes(), Vec::<String>::new());
}

#[test]
fn status_human_output_lists_every_field() {
    let host = FakeHost::new();
    let out = stdout(&host.loom(&["service", "status"]).success());
    let keys: Vec<&str> = out
        .lines()
        .filter_map(|line| line.split(':').next())
        .collect();
    assert_eq!(keys, ["platform", "unit", "installed", "loaded", "running"]);
}

#[test]
fn manager_failure_exits_1_with_its_stderr() {
    let host = FakeHost::new();
    let failing = "#!/bin/sh\ncase \"$*\" in\n  print*|*is-*) exit 3 ;;\nesac\necho 'Bootstrap failed: 5: Input/output error' >&2\nexit 5\n";
    for name in ["launchctl", "systemctl"] {
        fs::write(host.sandbox.path().join("empty-path").join(name), failing).unwrap();
    }
    let assert = host.loom(&["service", "install"]).code(1);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.contains("Bootstrap failed: 5"), "{stderr}");
}

fn manager_restart_commands() -> Vec<String> {
    if cfg!(target_os = "macos") {
        vec!["launchctl kickstart -k gui/501/io.github.gsuquet.loomd".to_owned()]
    } else {
        vec![
            "systemctl --user reset-failed loomd.service".to_owned(),
            "systemctl --user restart loomd.service".to_owned(),
        ]
    }
}

fn restart_failure(assert: &assert_cmd::assert::Assert) -> String {
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    let via = if cfg!(target_os = "macos") {
        "launchd"
    } else {
        "systemd"
    };
    assert!(
        stderr.contains("no new loomd answered on ")
            && stderr.contains(&format!("within 0.5 s of the restart via {via}")),
        "{stderr}"
    );
    stderr
}

#[test]
fn restart_goes_through_the_service_manager_and_waits_for_a_new_loomd() {
    let host = FakeHost::new();
    host.loom(&["service", "install"]).success();
    host.take_changes();

    // The fake manager starts no loomd: the restart waits for one, then fails.
    restart_failure(&host.restart().code(1));
    // No `kill`: the manager owns the process.
    assert_eq!(host.take_changes(), manager_restart_commands());
}

#[test]
fn restart_stops_a_loomd_running_outside_the_service_first() {
    let host = FakeHost::new();
    host.loom(&["service", "install"]).success();
    // The service is down (loaded, not running) and a `loom` command started a loomd itself,
    // which holds the lock the service's loomd would need.
    fs::remove_file(host.sandbox.path().join("empty-path/running")).unwrap();
    host.loom(&["status"]).success();
    let lock = host.sandbox.path().join("loom/run/loomd.lock");
    let outside: u32 = fs::read_to_string(&lock).unwrap().trim().parse().unwrap();
    host.take_changes();

    restart_failure(&host.restart().code(1));
    let mut expected = vec![format!("kill -TERM {outside}")];
    expected.extend(manager_restart_commands());
    assert_eq!(host.take_changes(), expected);
    let file = fs::File::open(&lock).unwrap();
    assert!(
        file.try_lock().is_ok(),
        "the loomd outside the service still runs"
    );
}
