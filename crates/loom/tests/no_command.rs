//! `loom` with no command prints the help and exits 0, on a pipe and on a terminal.

use std::fs::File;
use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rustix::fs::{Mode, OFlags};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use tempfile::TempDir;

mod isolation;

const TIMEOUT: Duration = Duration::from_secs(10);

fn loom(home: &Path) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("loom"));
    command
        .envs(isolation::isolated_env(home))
        .env("LOOM_HOME", home.join(".loom"))
        .env_remove("LOOM_CONTEXT")
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .current_dir(home);
    command
}

fn assert_is_the_help(stdout: &str) {
    assert!(
        stdout.contains("Usage: loom [OPTIONS] [COMMAND]"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("Commands:"), "stdout: {stdout}");
    assert!(!stdout.contains("TUI"), "stdout: {stdout}");
}

#[test]
fn on_a_pipe_it_prints_the_help_and_exits_zero() {
    let dir = TempDir::new().unwrap();
    for args in [&[][..], &["-c", "work"][..]] {
        let output = loom(dir.path()).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        assert_is_the_help(&String::from_utf8_lossy(&output.stdout));
    }
}

#[test]
fn on_a_terminal_it_prints_the_help_exits_zero_and_starts_no_loomd() {
    let dir = TempDir::new().unwrap();
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let slave_name = ptsname(&master, Vec::new()).unwrap();
    let slave = File::from(
        rustix::fs::open(&slave_name, OFlags::RDWR | OFlags::NOCTTY, Mode::empty()).unwrap(),
    );

    let mut command = loom(dir.path());
    command
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave))
        .stderr(Stdio::null());
    let mut child = command.spawn().unwrap();
    // The last copy of the slave end is the child's: its exit ends the read below.
    drop(command);

    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut master = File::from(master);
        let mut stdout = Vec::new();
        // At the end of the child, Linux reports `EIO` and macOS the end of file.
        let _ = master.read_to_end(&mut stdout);
        let _ = sender.send(stdout);
    });
    let Ok(stdout) = receiver.recv_timeout(TIMEOUT) else {
        child.kill().unwrap();
        child.wait().unwrap();
        panic!("`loom` did not end within {TIMEOUT:?}");
    };

    assert_eq!(child.wait().unwrap().code(), Some(0));
    assert_is_the_help(&String::from_utf8_lossy(&stdout));
    assert!(!dir.path().join(".loom/run/loomd.sock").exists());
}
