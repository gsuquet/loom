//! `loom` with no command prints the help and exits 0, on a pipe and on a terminal.

use std::fs::{self, File};
use std::io::{ErrorKind, Read as _};
use std::os::unix::net::UnixListener;
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

/// Runs `command` with a pseudoterminal as standard input and standard output. Returns its exit
/// code and what it wrote.
fn run_on_a_terminal(mut command: Command) -> (Option<i32>, String) {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let slave_name = ptsname(&master, Vec::new()).unwrap();
    let slave = File::from(
        rustix::fs::open(&slave_name, OFlags::RDWR | OFlags::NOCTTY, Mode::empty()).unwrap(),
    );

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
    (
        child.wait().unwrap().code(),
        String::from_utf8_lossy(&stdout).into_owned(),
    )
}

#[test]
fn on_a_terminal_it_prints_the_help_exits_zero_and_starts_no_loomd() {
    let dir = TempDir::new().unwrap();
    let (code, stdout) = run_on_a_terminal(loom(dir.path()));
    assert_eq!(code, Some(0));
    assert_is_the_help(&stdout);
    assert!(!dir.path().join(".loom/run/loomd.sock").exists());
}

/// Whether something connected to `listener` since the last call.
fn connected(listener: &UnixListener) -> bool {
    match listener.accept() {
        Ok(_) => true,
        Err(error) if error.kind() == ErrorKind::WouldBlock => false,
        Err(error) => panic!("accept: {error}"),
    }
}

#[test]
fn it_registers_nothing_on_use() {
    // Under `/tmp`: the macOS default `$TMPDIR` is too long for a socket path.
    let dir = tempfile::Builder::new()
        .prefix("loom-")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let status = Command::new("git")
        .envs(isolation::isolated_env(dir.path()))
        .args(["init", "--quiet"])
        .current_dir(&repo)
        .status()
        .unwrap();
    assert!(status.success());
    // A registration on use needs only the socket: this one stands for loomd.
    let socket = dir.path().join(".loom/run/loomd.sock");
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let in_repo = || {
        let mut command = loom(dir.path());
        command.current_dir(&repo);
        command
    };

    let output = in_repo().output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(!connected(&listener), "on a pipe");

    let (code, stdout) = run_on_a_terminal(in_repo());
    assert_eq!(code, Some(0), "{stdout}");
    assert!(!connected(&listener), "on a terminal");

    // The check can see a registration: another command in the same place sends one. It waits
    // at most 50 ms for the request, so a loaded machine can need several runs.
    let registered = (0..20).any(|_| {
        let mut command = in_repo();
        let output = command.arg("version").output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        connected(&listener)
    });
    assert!(registered, "`loom version` sent no registration on use");
}
