//! Auto-upgrade of loomd end to end (ADR-0011), against the fake releases API of
//! `common/mod.rs`, with a 1 s check interval and a 6 s watchdog deadline (debug-only test
//! hooks).
//!
//! The running version is a copy of the built `loom` and `loomd` in a temporary install
//! directory. The "new release" 99.0.0 is an archive of shell wrappers that `exec` the same
//! built binaries with `LOOM_TEST_BUILD_VERSION=99.0.0`; the release that fails after the
//! restart also sets `LOOM_TEST_FAIL_UNLESS_HOME_PREFIX`, so its loomd passes the pre-flight
//! (throwaway home under `/tmp/loom-preflight-`) and fails in the real home.
//!
//! Requires the `minisign` CLI and a fresh `loomd` binary: run it through `mise run test` (or
//! `cargo test --workspace`), which rebuilds `loomd`; `cargo test -p loom` alone may use a stale one.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

mod common;
mod isolation;

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use common::{Collector, FakeApi, RELEASES, Signer, archive_of, string_attribute, stub_binaries};
use loom_api::{
    ChannelSource, StatusResponse, UpdateChannel, UpdatePolicyMode, UpdateResult, UpdateState,
    VersionInfo,
};
use loom_update::release::release_target;
use loom_update::verify::sha256_hex;
use opentelemetry_proto::tonic::trace::v1::Span;
use serde_json::json;
use tempfile::TempDir;

const NEW: &str = "99.0.0";
const WAIT: Duration = Duration::from_secs(45);
const POLL: Duration = Duration::from_millis(100);

fn built(name: &str) -> PathBuf {
    let loom = PathBuf::from(assert_cmd::cargo::cargo_bin!("loom"));
    let path = loom.with_file_name(name);
    assert!(
        path.is_file(),
        "{} is missing: run `cargo test --workspace`",
        path.display()
    );
    path
}

/// `loom` and `loomd` wrappers reporting `NEW`, `exec`-ing the built binaries.
fn wrappers(extra_env: &str) -> [(&'static str, Vec<u8>); 2] {
    wrappers_of(NEW, extra_env)
}

/// `loom` and `loomd` wrappers reporting `version`, `exec`-ing the built binaries.
fn wrappers_of(version: &str, extra_env: &str) -> [(&'static str, Vec<u8>); 2] {
    ["loom", "loomd"].map(|name| {
        let env = if name == "loomd" { extra_env } else { "" };
        let script = format!(
            "#!/bin/sh\nexport LOOM_TEST_BUILD_VERSION={version}\n{env}exec '{}' \"$@\"\n",
            built(name).display()
        );
        (name, script.into_bytes())
    })
}

struct Env {
    install: PathBuf,
    _install_dir: TempDir,
    home: TempDir,
    api: FakeApi,
    signer: Signer,
    current: String,
    daemon: Option<Child>,
    log: PathBuf,
    next_asset_id: AtomicUsize,
    /// `LOOM_TEST_BUILD_VERSION` of the commands; a wrapper of a release sets its own.
    build_version: Option<String>,
}

impl Env {
    fn new(config: &str) -> Self {
        let install_dir = tempfile::tempdir().unwrap();
        let install = install_dir.path().canonicalize().unwrap();
        for name in ["loom", "loomd"] {
            fs::copy(built(name), install.join(name)).unwrap();
        }
        // Short: the socket path limit, and not under the pre-flight prefix.
        let home = tempfile::Builder::new()
            .prefix("loom-au-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::write(home.path().join("config.toml"), config).unwrap();
        let log = install_dir.path().join("test-loomd.log");
        Self {
            install,
            _install_dir: install_dir,
            home,
            api: FakeApi::start(),
            signer: Signer::new(),
            current: VersionInfo::current().version,
            daemon: None,
            log,
            next_asset_id: AtomicUsize::new(0),
            build_version: None,
        }
    }

    /// Publishes `NEW` as the latest stable release, made of `binaries`.
    fn publish(&self, binaries: [(&'static str, Vec<u8>); 2]) {
        let release = self.release(NEW, binaries);
        self.api
            .route(&format!("{RELEASES}/latest"), 200, release.to_string());
    }

    /// Publishes `releases` as the release list the dev channel reads.
    fn publish_list(&self, releases: &[serde_json::Value]) {
        self.api.route(
            &format!("{RELEASES}?per_page=30"),
            200,
            serde_json::Value::from(releases.to_vec()).to_string(),
        );
    }

    /// Serves the assets of release `version`, made of `binaries`, and returns its JSON.
    fn release(&self, version: &str, binaries: [(&'static str, Vec<u8>); 2]) -> serde_json::Value {
        let target = release_target(&VersionInfo::current().target);
        let archive_name = format!("loom-{version}-{target}.tar.gz");
        let archive = archive_of(binaries);
        let sums = format!("{}  {archive_name}\n", sha256_hex(&archive));
        let signature = self.signer.sign(sums.as_bytes());
        let mut assets = Vec::new();
        for (name, data) in [
            ("SHA256SUMS".to_owned(), sums.into_bytes()),
            ("SHA256SUMS.minisig".to_owned(), signature),
            (archive_name, archive),
        ] {
            let id = self.next_asset_id.fetch_add(1, Ordering::Relaxed);
            self.api
                .route(&format!("{RELEASES}/assets/{id}"), 200, data);
            assets.push(json!({ "id": id, "name": name }));
        }
        json!({ "tag_name": format!("v{version}"), "draft": false, "assets": assets })
    }

    /// Makes the installed binaries report `version` (test hook passed by [`Self::command`]).
    ///
    /// Not wrappers: an upgrade replaces the directory of the running `loomd`, which for a
    /// wrapper is the directory of the built binaries it `exec`s.
    fn report_version(&mut self, version: &str) {
        self.build_version = Some(version.to_owned());
        version.clone_into(&mut self.current);
    }

    fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .env("LOOM_HOME", self.home.path())
            .envs(isolation::isolated_env(self.home.path()))
            .env("LOOM_RELEASES_URL", &self.api.url)
            .env("LOOM_UPDATE_PUBKEY", &self.signer.public)
            .env("LOOM_UPDATE_CHECK_INTERVAL", "1s")
            .env("LOOM_UPDATE_HEALTH_DEADLINE", "6s")
            .env_remove("LOOM_TEST_FAIL_UNLESS_HOME_PREFIX")
            .env(
                "LOOM_TEST_BUILD_VERSION",
                self.build_version.as_deref().unwrap_or(""),
            )
            .env_remove("LOOM_GITHUB_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .env_remove("NO_COLOR")
            .stdin(Stdio::null());
        for variable in [
            "OTEL_SDK_DISABLED",
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            "OTEL_EXPORTER_OTLP_HEADERS",
            "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
            "OTEL_SERVICE_NAME",
            "OTEL_RESOURCE_ATTRIBUTES",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            command.env_remove(variable);
        }
        // Spans reach the fake collector quickly when telemetry is on.
        command.env("OTEL_BSP_SCHEDULE_DELAY", "100");
        command
    }

    /// Starts the installed loomd, as a service manager or `loom status` would.
    fn start_loomd(&mut self) {
        let log = File::create(&self.log).unwrap();
        let child = self
            .command(&self.install.join("loomd"))
            .arg("--color=never")
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        self.daemon = Some(child);
        self.wait_for(|env| env.status().is_some(), "loomd to answer");
    }

    fn loom(&self, args: &[&str]) -> Output {
        self.command(&self.install.join("loom"))
            .arg("--color=never")
            .args(args)
            .output()
            .unwrap()
    }

    /// `loom status --no-start --json`, `None` when loomd does not answer.
    fn status(&self) -> Option<StatusResponse> {
        let output = self.loom(&["status", "--no-start", "--json"]);
        output
            .status
            .success()
            .then(|| serde_json::from_slice(&output.stdout).unwrap())
    }

    fn state(&self) -> UpdateState {
        fs::read(self.home.path().join("state/update.json"))
            .map(|bytes| serde_json::from_slice(&bytes).unwrap())
            .unwrap_or_default()
    }

    fn installed(&self) -> [Vec<u8>; 2] {
        ["loom", "loomd"].map(|name| fs::read(self.install.join(name)).unwrap())
    }

    fn wait_for(&self, done: impl Fn(&Self) -> bool, what: &str) {
        let deadline = Instant::now() + WAIT;
        while !done(self) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}\nstate: {:?}\nlog:\n{}",
                self.state(),
                self.logs()
            );
            thread::sleep(POLL);
        }
    }

    fn logs(&self) -> String {
        let daemon_log = self.home.path().join("log/loomd.log");
        format!(
            "{}\n--- log/loomd.log ---\n{}",
            fs::read_to_string(&self.log).unwrap_or_default(),
            fs::read_to_string(daemon_log).unwrap_or_default()
        )
    }

    fn asset_requests(&self) -> usize {
        self.api.asset_requests()
    }
}

impl Drop for Env {
    /// Stops every loomd of the home: the test's child and the ones the watchdog started.
    fn drop(&mut self) {
        if let Some(child) = &mut self.daemon {
            let _ = child.kill();
            let _ = child.wait();
        }
        let lock = self.home.path().join("run/loomd.lock");
        let deadline = Instant::now() + Duration::from_secs(10);
        while loom_update::daemon::lock_held(&lock) && Instant::now() < deadline {
            if let Some(pid) = loom_update::daemon::holder_pid(&lock) {
                let _ = Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .status();
            }
            thread::sleep(POLL);
        }
    }
}

fn config(policy: &str) -> String {
    format!("[update]\npolicy = \"{policy}\"\nchannel = \"stable\"\n")
}

#[test]
fn auto_installs_a_new_release_and_the_watchdog_confirms_it() {
    let mut env = Env::new(&config("auto"));
    let before = env.installed();
    env.publish(wrappers(""));
    env.start_loomd();

    env.wait_for(
        |env| matches!(env.state().last_result, Some(UpdateResult::Upgraded { .. })),
        "the upgrade to complete",
    );
    let [loom, loomd] = wrappers("").map(|(_, data)| data);
    assert_eq!(env.installed(), [loom, loomd]);
    let saved = env.home.path().join("versions").join(&env.current);
    assert_eq!(fs::read(saved.join("loom")).unwrap(), before[0]);
    assert_eq!(fs::read(saved.join("loomd")).unwrap(), before[1]);

    // The old loomd stopped by itself, with code 0, for the new one to start.
    let old = env.daemon.as_mut().unwrap().wait().unwrap();
    assert_eq!(old.code(), Some(0), "{}", env.logs());

    let status = env.status().expect("the new loomd answers");
    assert_eq!(status.daemon.version, NEW);
    let update = status.update.unwrap();
    assert_eq!(update.policy, UpdatePolicyMode::Auto);
    assert_eq!(update.pending, None);
    assert_eq!(update.failed_versions, Vec::<String>::new());
}

#[test]
fn a_release_failing_the_preflight_is_not_installed_nor_retried() {
    let mut env = Env::new(&config("auto"));
    let before = env.installed();
    // Stub binaries: `loomd -V` works, `loomd` itself exits at once.
    env.publish(stub_binaries(NEW));
    env.start_loomd();

    // The next check (1 s later) may already have replaced the result: wait on the version.
    env.wait_for(
        |env| env.state().failed_versions == [NEW],
        "the version to be recorded as failed",
    );
    assert!(
        env.logs().contains("pre-flight check failed"),
        "{}",
        env.logs()
    );
    assert_eq!(env.installed(), before);
    assert!(!env.home.path().join("versions").exists());
    assert!(
        env.daemon.as_mut().unwrap().try_wait().unwrap().is_none(),
        "loomd keeps running"
    );

    // The next checks skip it without downloading it again.
    let downloads = env.asset_requests();
    env.wait_for(
        |env| {
            matches!(
                env.state().last_result,
                Some(UpdateResult::SkippedFailed { .. })
            )
        },
        "a check skipping the failed version",
    );
    assert_eq!(env.asset_requests(), downloads);
    assert_eq!(env.installed(), before);
    let status = env.status().unwrap();
    assert_eq!(status.update.unwrap().failed_versions, [NEW]);
}

#[test]
fn a_release_failing_after_the_restart_is_rolled_back_by_the_watchdog() {
    let mut env = Env::new(&config("auto"));
    let before = env.installed();
    env.publish(wrappers(
        "export LOOM_TEST_FAIL_UNLESS_HOME_PREFIX=/tmp/loom-preflight-\n",
    ));
    env.start_loomd();

    env.wait_for(
        |env| {
            matches!(
                env.state().last_result,
                Some(UpdateResult::RolledBack { .. })
            )
        },
        "the watchdog to roll back",
    );
    assert_eq!(env.installed(), before, "previous binaries restored");
    assert!(
        !env.home.path().join("versions").join(&env.current).exists(),
        "the save of the restored version is removed"
    );
    assert_eq!(env.state().failed_versions, [NEW]);
    assert_eq!(env.state().pending, None);

    // The watchdog starts the previous loomd again.
    env.wait_for(
        |env| {
            env.status()
                .is_some_and(|status| status.daemon.version == env.current)
        },
        "the previous loomd to answer",
    );
    // And it never retries the failed version.
    env.wait_for(
        |env| {
            matches!(
                env.state().last_result,
                Some(UpdateResult::SkippedFailed { .. })
            )
        },
        "a check skipping the failed version",
    );
    assert_eq!(env.installed(), before);
}

#[test]
fn notify_records_the_available_release_and_status_shows_it() {
    let mut env = Env::new(&config("notify"));
    let before = env.installed();
    env.publish(stub_binaries(NEW));
    env.start_loomd();

    env.wait_for(
        |env| env.state().available.as_deref() == Some(NEW),
        "the release to be recorded as available",
    );
    let status = env.status().unwrap();
    let update = status.update.unwrap();
    assert_eq!(update.policy, UpdatePolicyMode::Notify);
    assert_eq!(update.available.as_deref(), Some(NEW));

    let human = env.loom(&["status", "--no-start"]);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains(&format!("\navailable:   {NEW}\n")), "{text}");
    assert!(
        text.contains("\nupdates:     notify, channel stable (config), every 1s\n"),
        "{text}"
    );

    assert_eq!(env.asset_requests(), 0, "nothing downloaded");
    assert_eq!(env.installed(), before);
}

/// The bug of a real install: installed on the dev channel, auto-upgraded to a stable release
/// (above the dev build under the dev rule), then stuck on stable because the new build's
/// channel became the default. The chosen channel is remembered, so the next check still
/// follows dev and picks a higher dev build.
///
/// The test binaries are `local` builds: without the remembered channel, the stable release
/// would not check at all (a real stable build would check stable); either way it would never
/// reach the dev build.
///
/// The next dev build is made of stubs failing the pre-flight: its check is the proof, and a
/// second swap would replace the built binaries the stable wrappers `exec`.
#[test]
fn a_chosen_dev_channel_survives_an_upgrade_to_a_stable_release() {
    const DEV: &str = "1.0.0-dev.5";
    const STABLE: &str = "1.0.0";
    const NEXT_DEV: &str = "1.1.0-dev.1";
    let mut env = Env::new("[update]\npolicy = \"auto\"\n");
    env.report_version(DEV);
    let stable = env.release(STABLE, wrappers_of(STABLE, ""));
    env.publish_list(std::slice::from_ref(&stable));

    // `install.sh --channel dev` runs this after installing.
    let output = env.loom(&["upgrade", "--channel", "dev", "--check", "--quiet"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        env.state().channel.map(|chosen| chosen.name),
        Some(UpdateChannel::Dev)
    );

    env.start_loomd();
    env.wait_for(
        |env| {
            matches!(
                env.state().last_result,
                Some(UpdateResult::Upgraded { ref to, .. }) if to == STABLE
            )
        },
        "the upgrade to the stable release",
    );
    env.wait_for(
        |env| {
            env.status()
                .is_some_and(|status| status.daemon.version == STABLE)
        },
        "the stable loomd to answer",
    );
    let update = env.status().unwrap().update.unwrap();
    assert_eq!(update.channel, Some(UpdateChannel::Dev));
    assert_eq!(update.channel_source, Some(ChannelSource::Chosen));

    let next = env.release(NEXT_DEV, stub_binaries(NEXT_DEV));
    env.publish_list(&[next, stable]);
    env.wait_for(
        |env| env.state().failed_versions == [NEXT_DEV],
        "the stable loomd to pick the next dev build",
    );
    assert_eq!(
        env.status().unwrap().daemon.version,
        STABLE,
        "nothing swapped"
    );
    assert_eq!(
        env.state().channel.map(|chosen| chosen.name),
        Some(UpdateChannel::Dev),
        "still remembered"
    );
}

#[test]
fn off_never_touches_the_network() {
    let mut env = Env::new(&config("off"));
    env.publish(stub_binaries(NEW));
    env.start_loomd();
    // More than two check intervals.
    thread::sleep(Duration::from_secs(3));
    assert_eq!(env.api.requests(), Vec::<String>::new());
    assert_eq!(env.state(), UpdateState::default());
    let update = env.status().unwrap().update.unwrap();
    assert_eq!(update.policy, UpdatePolicyMode::Off);
}

fn span_named<'a>(spans: &'a [(String, Span)], service: &str, name: &str) -> Vec<&'a Span> {
    spans
        .iter()
        .filter(|(from, span)| from == service && span.name == name)
        .map(|(_, span)| span)
        .collect()
}

fn children<'a>(spans: &'a [(String, Span)], parent: &Span) -> Vec<&'a str> {
    let mut names: Vec<&str> = spans
        .iter()
        .filter(|(_, span)| span.parent_span_id == parent.span_id)
        .map(|(_, span)| span.name.as_str())
        .collect();
    names.sort_unstable();
    names
}

/// One successful auto-upgrade gives one trace in loomd (`update.check` and its steps) and
/// one in the watchdog (`loom __upgrade-watchdog` > `update.watchdog`).
#[test]
fn an_auto_upgrade_is_traced_in_loomd_and_in_the_watchdog() {
    let collector = Collector::start();
    let mut env = Env::new(&format!(
        "{}\n[telemetry]\nenabled = true\notlp_endpoint = \"{}/v1/traces\"\n",
        config("auto"),
        collector.url
    ));
    env.publish(wrappers(""));
    env.start_loomd();
    env.wait_for(
        |env| matches!(env.state().last_result, Some(UpdateResult::Upgraded { .. })),
        "the upgrade to complete",
    );
    let deadline = Instant::now() + WAIT;
    let spans = loop {
        let spans = collector.spans();
        let check_done = span_named(&spans, "loomd", "update.check")
            .iter()
            .any(|span| {
                string_attribute(&span.attributes, "update.outcome").as_deref() == Some("swapped")
            });
        if check_done && !span_named(&spans, "loom", "update.watchdog").is_empty() {
            break spans;
        }
        assert!(
            Instant::now() < deadline,
            "spans missing: {:?}",
            spans
                .iter()
                .map(|(s, span)| (s, &span.name))
                .collect::<std::collections::BTreeSet<_>>()
        );
        thread::sleep(POLL);
    };

    let check = *span_named(&spans, "loomd", "update.check")
        .iter()
        .find(|span| {
            string_attribute(&span.attributes, "update.outcome").as_deref() == Some("swapped")
        })
        .unwrap();
    assert!(
        check.parent_span_id.is_empty(),
        "update.check is a root span"
    );
    assert_eq!(
        string_attribute(&check.attributes, "update.version").as_deref(),
        Some(NEW)
    );
    assert_eq!(
        children(&spans, check),
        [
            "update.decide",
            "update.download",
            "update.preflight",
            "update.swap"
        ]
    );
    let download = span_named(&spans, "loomd", "update.download")[0];
    assert_eq!(
        children(&spans, download),
        ["update.verify", "update.verify", "update.verify"]
    );
    for (_, span) in spans.iter().filter(|(service, _)| service == "loomd") {
        if span.name.starts_with("update.") && span.name != "update.check" {
            assert_eq!(
                span.trace_id, check.trace_id,
                "{} in the check's trace",
                span.name
            );
        }
    }
    let preflight = span_named(&spans, "loomd", "update.preflight")[0];
    assert_eq!(
        string_attribute(&preflight.attributes, "update.outcome").as_deref(),
        Some("passed")
    );

    let watchdog = span_named(&spans, "loom", "update.watchdog")[0];
    assert_eq!(
        string_attribute(&watchdog.attributes, "update.outcome").as_deref(),
        Some("upgraded")
    );
    assert_eq!(
        string_attribute(&watchdog.attributes, "update.to").as_deref(),
        Some(NEW)
    );
    let root = span_named(&spans, "loom", "loom __upgrade-watchdog")[0];
    assert_eq!(root.parent_span_id, Vec::<u8>::new());
    assert_eq!(watchdog.parent_span_id, root.span_id);
}
