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
        .args(["service", "restart"])
        .assert()
        .code(2);
}

/// Fake `id`, `launchctl`, `systemctl` and `loomd`, alone on `PATH`. Each logs its arguments
/// to `calls.log`; loading creates `loaded`, unloading removes it, queries read it.
const FAKE_MANAGER: &str = r#"#!/bin/sh
dir=$(cd "${0%/*}" && pwd)
echo "${0##*/} $*" >> "$dir/calls.log"
case "${0##*/} $*" in
  "id -u") echo 501 ;;
  "launchctl print"*|"systemctl --user is-enabled"*|"systemctl --user is-active"*)
    if [ -e "$dir/loaded" ]; then echo "	state = running"; else exit 3; fi ;;
  "launchctl bootstrap"*|"systemctl --user enable --now"*) : > "$dir/loaded" ;;
  "launchctl bootout"*|"systemctl --user disable --now"*) /bin/rm -f "$dir/loaded" ;;
esac
"#;

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
        Self { sandbox }
    }

    fn loom(&self, args: &[&str]) -> assert_cmd::assert::Assert {
        loom(self.sandbox.path()).args(args).assert()
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
