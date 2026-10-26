//! The shutdown helpers of loomd: the registry of git process groups and the bounded join.
//! Real processes (`sh`, `sleep`), no git.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use loomd::shutdown::{ChildGroups, join_within};

/// `sh -c script`, in its own process group, as git commands run.
fn shell(script: &str) -> Command {
    let mut command = Command::new("sh");
    command
        .args(["-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Whether a process of the group `pgid` exists (`kill -0`).
fn group_alive(pgid: u32) -> bool {
    Command::new("kill")
        .args(["-0", "--", &format!("-{pgid}")])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn wait_until(deadline: Duration, done: impl Fn() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < deadline {
        if done() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    done()
}

/// Waits for `child` on a thread and then drops its registration, as `Git::output` does when
/// the command ends; the status of the child.
fn reap(
    mut child: std::process::Child,
    registration: loomd::shutdown::Registration,
) -> thread::JoinHandle<std::process::ExitStatus> {
    thread::spawn(move || {
        let status = child.wait().unwrap();
        drop(registration);
        status
    })
}

#[test]
fn terminate_stops_the_whole_group_of_a_child_that_obeys_sigterm() {
    let groups = Arc::new(ChildGroups::default());
    let (child, registration) = groups.spawn(&mut shell("sleep 30 & wait")).unwrap();
    let pid = child.id();
    assert_eq!(groups.len(), 1);
    let reaper = reap(child, registration);

    let started = Instant::now();
    groups.terminate(Duration::from_secs(5));
    assert!(started.elapsed() < Duration::from_secs(4));
    assert!(!reaper.join().unwrap().success());
    // `sleep` shares the group of the leader: it is gone too.
    assert!(
        wait_until(Duration::from_secs(15), || !group_alive(pid)),
        "a process of the group survives"
    );
    assert_eq!(groups.len(), 0);
}

#[test]
fn terminate_kills_a_child_that_ignores_sigterm_after_the_grace() {
    let groups = Arc::new(ChildGroups::default());
    // The trap is in place once the file exists: a signal sent before would kill the shell.
    let dir = tempfile::tempdir().unwrap();
    let ready = dir.path().join("ready");
    let mut command = shell("trap '' TERM; : > \"$0\"; while :; do sleep 1; done");
    command.arg(&ready);
    let (child, registration) = groups.spawn(&mut command).unwrap();
    assert!(wait_until(Duration::from_secs(5), || ready.exists()));
    let pid = child.id();
    let reaper = reap(child, registration);
    let grace = Duration::from_millis(300);

    let started = Instant::now();
    groups.terminate(grace);
    let took = started.elapsed();
    assert!(took >= grace, "{took:?}");
    assert!(took < Duration::from_secs(15), "{took:?}");
    assert!(!reaper.join().unwrap().success());
    assert!(wait_until(Duration::from_secs(15), || !group_alive(pid)));
}

#[test]
fn nothing_spawns_once_the_registry_is_terminating() {
    let groups = Arc::new(ChildGroups::default());
    groups.terminate(Duration::from_millis(10));
    let refused = groups.spawn(&mut shell("true"));
    assert!(refused.is_err());
    assert_eq!(groups.len(), 0);
}

#[test]
fn a_finished_child_leaves_the_registry() {
    let groups = Arc::new(ChildGroups::default());
    let (mut child, registration) = groups.spawn(&mut shell("true")).unwrap();
    child.wait().unwrap();
    drop(registration);
    assert_eq!(groups.len(), 0);
    // Nothing to stop: returns at once.
    let started = Instant::now();
    groups.terminate(Duration::from_secs(5));
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[test]
fn join_within_waits_for_a_thread_up_to_the_deadline() {
    let quick = thread::spawn(|| thread::sleep(Duration::from_millis(50)));
    assert!(join_within(quick, Duration::from_secs(5), "quick"));

    let (release, hold) = std::sync::mpsc::channel::<()>();
    let slow = thread::spawn(move || {
        let _ = hold.recv();
    });
    let started = Instant::now();
    assert!(!join_within(slow, Duration::from_millis(200), "slow"));
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(200), "{took:?}");
    assert!(took < Duration::from_secs(15), "{took:?}");
    drop(release);

    // A panic of the thread is reported by the log, not propagated: the thread is finished.
    let panicked = thread::spawn(|| std::panic::resume_unwind(Box::new("boom")));
    assert!(join_within(panicked, Duration::from_secs(5), "panicked"));
}
