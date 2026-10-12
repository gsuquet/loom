//! `loom upgrade` and `loom rollback` end to end, against a local fake of the GitHub releases
//! API and fixture archives signed with a throwaway minisign key.
//!
//! The `loom` under test is a copy of the built binary in a temporary install directory, so
//! that the directory of `current_exe()` is the one replaced. The fixture binaries are shell
//! scripts answering `version --json` and `-V`.
//!
//! Requires the `minisign` CLI (pinned in `.mise/config.toml`; `mise run test`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;

use flate2::Compression;
use flate2::write::GzEncoder;
use loom_api::{DaemonRestart, RollbackReport, UpgradeDecision, UpgradeReport, VersionInfo};
use loom_update::release::release_target;
use loom_update::verify::sha256_hex;
use serde_json::{Value, json};
use tempfile::TempDir;

// --- fake GitHub API ----------------------------------------------------------------------

type Routes = Arc<Mutex<HashMap<String, (u16, Vec<u8>)>>>;

/// HTTP/1.1 server answering `GET <path>` from a route table, one connection at a time.
struct FakeApi {
    url: String,
    routes: Routes,
    requests: Arc<Mutex<Vec<String>>>,
}

impl FakeApi {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let routes: Routes = Arc::default();
        let requests: Arc<Mutex<Vec<String>>> = Arc::default();
        let (table, log) = (Arc::clone(&routes), Arc::clone(&requests));
        // Detached: lives until the test process exits.
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                let path = head.split_whitespace().nth(1).unwrap_or("").to_owned();
                log.lock().unwrap().push(head);
                let (status, body) = table
                    .lock()
                    .unwrap()
                    .get(&path)
                    .cloned()
                    .unwrap_or_else(|| (404, br#"{"message":"Not Found"}"#.to_vec()));
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
        });
        Self {
            url,
            routes,
            requests,
        }
    }

    fn route(&self, path: &str, status: u16, body: impl Into<Vec<u8>>) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.to_owned(), (status, body.into()));
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn asset_requests(&self) -> usize {
        self.requests()
            .iter()
            .filter(|head| head.contains("/releases/assets/"))
            .count()
    }
}

const RELEASES: &str = "/repos/gsuquet/loom/releases";

// --- fixtures -----------------------------------------------------------------------------

fn version_json(version: &str) -> String {
    let info = VersionInfo {
        version: version.to_owned(),
        ..VersionInfo::current()
    };
    serde_json::to_string(&info).unwrap()
}

/// Stub `loom` and `loomd` scripts reporting `version`.
fn stub_binaries(version: &str) -> [(&'static str, Vec<u8>); 2] {
    let json = version_json(version);
    [
        (
            "loom",
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  version) echo '{json}' ;;\n  -V) echo 'loom {version}' ;;\nesac\n"
            )
            .into_bytes(),
        ),
        (
            "loomd",
            format!("#!/bin/sh\n[ \"$1\" = -V ] && echo 'loomd {version}'\nexit 0\n").into_bytes(),
        ),
    ]
}

fn write_stubs(dir: &Path, version: &str) {
    fs::create_dir_all(dir).unwrap();
    for (name, data) in stub_binaries(version) {
        fs::write(dir.join(name), data).unwrap();
        fs::set_permissions(dir.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// Release archive with the stubs of `version`, `LICENSE` and `README.md`.
fn archive(version: &str) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    let mut files: Vec<(&str, Vec<u8>, u32)> = stub_binaries(version)
        .into_iter()
        .map(|(name, data)| (name, data, 0o755))
        .collect();
    files.push(("LICENSE", b"MIT".to_vec(), 0o644));
    files.push(("README.md", b"# loom".to_vec(), 0o644));
    for (name, data, mode) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        builder.append_data(&mut header, name, &data[..]).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

/// A throwaway minisign key pair, made with the `minisign` CLI.
struct Signer {
    dir: TempDir,
    public: String,
}

impl Signer {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let status = Command::new("minisign")
            .args(["-G", "-W", "-f", "-p"])
            .arg(dir.path().join("key.pub"))
            .arg("-s")
            .arg(dir.path().join("key.sec"))
            .output()
            .expect("the minisign CLI is required: run the tests with `mise run test`");
        assert!(status.status.success(), "minisign -G failed: {status:?}");
        let file = fs::read_to_string(dir.path().join("key.pub")).unwrap();
        let public = file.lines().nth(1).unwrap().trim().to_owned();
        Self { dir, public }
    }

    fn sign(&self, data: &[u8]) -> Vec<u8> {
        let message = self.dir.path().join("SHA256SUMS");
        let signature = self.dir.path().join("SHA256SUMS.minisig");
        fs::write(&message, data).unwrap();
        let output = Command::new("minisign")
            .arg("-S")
            .arg("-s")
            .arg(self.dir.path().join("key.sec"))
            .arg("-m")
            .arg(&message)
            .arg("-x")
            .arg(&signature)
            .output()
            .unwrap();
        assert!(output.status.success(), "minisign -S failed: {output:?}");
        fs::read(signature).unwrap()
    }
}

/// How to break a published release.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tamper {
    None,
    Checksum,
    Signature,
    MissingArchive,
    ArchiveHttpError,
}

// --- test environment ---------------------------------------------------------------------

struct Env {
    install: TempDir,
    home: TempDir,
    api: FakeApi,
    signer: Signer,
    current: String,
    target: String,
    next_asset_id: Mutex<u64>,
}

impl Env {
    fn new() -> Self {
        let install = tempfile::tempdir().unwrap();
        fs::copy(
            assert_cmd::cargo::cargo_bin!("loom"),
            install.path().join("loom"),
        )
        .unwrap();
        fs::write(install.path().join("loomd"), "#!/bin/sh\n# old loomd\n").unwrap();
        fs::set_permissions(
            install.path().join("loomd"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let info = VersionInfo::current();
        Self {
            install,
            home: tempfile::tempdir().unwrap(),
            api: FakeApi::start(),
            signer: Signer::new(),
            current: info.version,
            target: release_target(&info.target),
            next_asset_id: Mutex::new(100),
        }
    }

    fn loom(&self, args: &[&str]) -> Output {
        Command::new(self.install.path().join("loom"))
            .args(args)
            .env("LOOM_HOME", self.home.path())
            .env("LOOM_RELEASES_URL", &self.api.url)
            .env("LOOM_UPDATE_PUBKEY", &self.signer.public)
            .env_remove("LOOM_GITHUB_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("ALL_PROXY")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            .env_remove("all_proxy")
            .output()
            .unwrap()
    }

    fn installed(&self) -> [Vec<u8>; 2] {
        ["loom", "loomd"].map(|name| fs::read(self.install.path().join(name)).unwrap())
    }

    fn versions_dir(&self) -> PathBuf {
        self.home.path().join("versions")
    }

    fn saved(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.versions_dir())
            .map(|entries| {
                entries
                    .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn asset_requests(&self) -> usize {
        self.api.asset_requests()
    }

    fn asset_id(&self) -> u64 {
        let mut id = self.next_asset_id.lock().unwrap();
        *id += 1;
        *id
    }

    /// Serves the assets of release `version` and returns its JSON.
    fn release(&self, version: &str, tamper: Tamper) -> Value {
        let archive_name = format!("loom-{version}-{}.tar.gz", self.target);
        let archive = archive(version);
        let digest = if tamper == Tamper::Checksum {
            sha256_hex(b"something else")
        } else {
            sha256_hex(&archive)
        };
        let sums = format!("{digest}  {archive_name}\n");
        let signature = self.signer.sign(sums.as_bytes());
        let mut sums = sums.into_bytes();
        if tamper == Tamper::Signature {
            // Signed, then altered: the signature no longer matches.
            sums.extend_from_slice(b"0000  extra\n");
        }
        let mut assets = Vec::new();
        for (name, data) in [
            ("SHA256SUMS".to_owned(), sums),
            ("SHA256SUMS.minisig".to_owned(), signature),
            (archive_name.clone(), archive),
        ] {
            if tamper == Tamper::MissingArchive && name == archive_name {
                continue;
            }
            let id = self.asset_id();
            let status = if tamper == Tamper::ArchiveHttpError && name == archive_name {
                500
            } else {
                200
            };
            self.api
                .route(&format!("{RELEASES}/assets/{id}"), status, data);
            assets.push(json!({ "id": id, "name": name }));
        }
        json!({ "tag_name": format!("v{version}"), "draft": false, "assets": assets })
    }

    fn publish_latest(&self, version: &str, tamper: Tamper) {
        let release = self.release(version, tamper);
        self.api
            .route(&format!("{RELEASES}/latest"), 200, release.to_string());
    }

    fn publish_tag(&self, version: &str) {
        let release = self.release(version, Tamper::None);
        self.api.route(
            &format!("{RELEASES}/tags/v{version}"),
            200,
            release.to_string(),
        );
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        stdout(output),
        stderr(output)
    );
}

fn assert_failure(output: &Output, message: &str) {
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout: {}\nstderr: {}",
        stdout(output),
        stderr(output)
    );
    assert!(
        stderr(output).contains(message),
        "stderr does not contain {message:?}: {}",
        stderr(output)
    );
}

fn upgrade_report(output: &Output) -> UpgradeReport {
    assert_success(output);
    serde_json::from_slice(&output.stdout).unwrap()
}

fn apply(version: &str) -> UpgradeDecision {
    UpgradeDecision::Apply {
        version: version.to_owned(),
    }
}

const NEW: &str = "99.0.0";

// --- --check ------------------------------------------------------------------------------

#[test]
fn check_reports_the_decision_and_changes_nothing() {
    let env = Env::new();
    env.publish_latest(NEW, Tamper::None);
    let before = env.installed();

    let output = env.loom(&["upgrade", "--channel", "stable", "--check"]);
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!(
            "loom {NEW} is available (stable), running {}\n",
            env.current
        )
    );

    let report =
        upgrade_report(&env.loom(&["upgrade", "--channel", "stable", "--check", "--json"]));
    assert_eq!(report.decision, apply(NEW));
    assert!(report.check);
    assert_eq!(report.install, None);

    assert_eq!(env.installed(), before);
    assert_eq!(env.asset_requests(), 0, "nothing downloaded");
    assert!(!env.home.path().join("versions").exists());
    assert!(!env.home.path().join("staging").exists());
}

#[test]
fn up_to_date_installs_nothing() {
    let env = Env::new();
    env.api.route(
        &format!("{RELEASES}/latest"),
        200,
        json!({ "tag_name": format!("v{}", env.current), "assets": [] }).to_string(),
    );
    let before = env.installed();
    let output = env.loom(&["upgrade", "--channel", "stable"]);
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!("loom {} is up to date (stable)\n", env.current)
    );
    assert_eq!(env.installed(), before);
}

#[test]
fn dev_channel_picks_the_highest_run_number() {
    let env = Env::new();
    let list = json!([
        { "tag_name": "v99.0.0-dev.3", "draft": false, "assets": [] },
        { "tag_name": "v99.0.0-dev.12", "draft": false, "assets": [] },
        { "tag_name": "v99.1.0-dev.40", "draft": true, "assets": [] },
        { "tag_name": "v98.0.0", "draft": false, "assets": [] },
    ]);
    env.api
        .route(&format!("{RELEASES}?per_page=30"), 200, list.to_string());
    let report = upgrade_report(&env.loom(&["upgrade", "--channel", "dev", "--check", "--json"]));
    assert_eq!(report.decision, apply("99.0.0-dev.12"));
}

#[test]
fn dev_channel_moves_to_a_stable_release_above_every_dev_build() {
    let env = Env::new();
    let list = json!([
        env.release("99.0.0", Tamper::None),
        { "tag_name": "v99.0.0-dev.12", "draft": false, "assets": [] },
        { "tag_name": "v99.0.0-dev.3", "draft": false, "assets": [] },
        { "tag_name": "nightly", "draft": false, "assets": [] },
    ]);
    env.api
        .route(&format!("{RELEASES}?per_page=30"), 200, list.to_string());
    let report = upgrade_report(&env.loom(&["upgrade", "--channel", "dev", "--json"]));
    assert_eq!(report.decision, apply("99.0.0"));
    assert_eq!(
        env.installed(),
        stub_binaries("99.0.0").map(|(_, data)| data)
    );
    let lists = env
        .api
        .requests()
        .iter()
        .filter(|head| head.contains("?per_page=30"))
        .count();
    assert_eq!(lists, 1, "the release list is fetched once");
}

#[test]
fn dev_channel_prefers_a_dev_build_above_the_stable_release() {
    let env = Env::new();
    let list = json!([
        env.release("99.1.0-dev.3", Tamper::None),
        { "tag_name": "v99.0.0", "draft": false, "assets": [] },
        { "tag_name": "v99.0.0-dev.12", "draft": false, "assets": [] },
    ]);
    env.api
        .route(&format!("{RELEASES}?per_page=30"), 200, list.to_string());
    let report = upgrade_report(&env.loom(&["upgrade", "--channel", "dev", "--json"]));
    assert_eq!(report.decision, apply("99.1.0-dev.3"));
    assert_eq!(
        env.installed(),
        stub_binaries("99.1.0-dev.3").map(|(_, data)| data)
    );
}

#[test]
fn version_selects_its_tag() {
    let env = Env::new();
    env.publish_tag("98.0.0");
    let report =
        upgrade_report(&env.loom(&["upgrade", "--version", "98.0.0", "--check", "--json"]));
    assert_eq!(report.decision, apply("98.0.0"));

    let output = env.loom(&["upgrade", "--version", "97.0.0", "--check", "--json"]);
    let report = upgrade_report(&output);
    assert!(matches!(report.decision, UpgradeDecision::Skipped { .. }));

    let before = env.installed();
    assert_failure(
        &env.loom(&["upgrade", "--version", "97.0.0"]),
        "version 97.0.0 is not released",
    );
    assert_eq!(env.installed(), before);
}

#[test]
fn local_build_needs_a_channel() {
    if VersionInfo::current().channel != loom_api::Channel::Local {
        return;
    }
    let env = Env::new();
    assert_failure(
        &env.loom(&["upgrade"]),
        "pass --channel, --version or --from",
    );
}

// --- upgrade ------------------------------------------------------------------------------

#[test]
fn upgrade_replaces_both_binaries_and_saves_the_previous_version() {
    let env = Env::new();
    env.publish_latest(NEW, Tamper::None);
    let before = env.installed();

    let report = upgrade_report(&env.loom(&["upgrade", "--channel", "stable", "--json"]));
    assert_eq!(report.decision, apply(NEW));
    let install = report.install.unwrap();
    assert!(install.installed && install.verified);
    assert_eq!(install.version, NEW);
    assert_eq!(install.daemon, DaemonRestart::NotRunning);

    let [loom, loomd] = stub_binaries(NEW).map(|(_, data)| data);
    assert_eq!(env.installed(), [loom, loomd]);
    let mode = fs::metadata(env.install.path().join("loomd"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);

    let saved = env.versions_dir().join(&env.current);
    assert_eq!(install.saved_to.as_deref(), Some(saved.as_path()));
    assert_eq!(fs::read(saved.join("loom")).unwrap(), before[0]);
    assert_eq!(fs::read(saved.join("loomd")).unwrap(), before[1]);
    // Only the binaries in the install directory, no temporary file left.
    assert_eq!(fs::read_dir(env.install.path()).unwrap().count(), 2);
}

#[test]
fn missing_loomd_is_installed() {
    let env = Env::new();
    fs::remove_file(env.install.path().join("loomd")).unwrap();
    env.publish_latest(NEW, Tamper::None);
    assert_success(&env.loom(&["upgrade", "--channel", "stable"]));
    assert_eq!(
        fs::read(env.install.path().join("loomd")).unwrap(),
        stub_binaries(NEW)[1].1
    );
}

#[test]
fn upgrade_keeps_two_previous_versions() {
    let env = Env::new();
    write_stubs(&env.versions_dir().join("0.0.1"), "0.0.1");
    write_stubs(&env.versions_dir().join("0.0.2"), "0.0.2");
    env.publish_latest(NEW, Tamper::None);

    let report = upgrade_report(&env.loom(&["upgrade", "--channel", "stable", "--json"]));
    assert_eq!(report.install.unwrap().pruned, vec!["0.0.1".to_owned()]);
    let mut expected = vec!["0.0.2".to_owned(), env.current.clone()];
    expected.sort();
    assert_eq!(env.saved(), expected);
}

#[test]
fn failed_downloads_and_verifications_leave_the_install_untouched() {
    let cases = [
        (Tamper::Checksum, "checksum mismatch"),
        (Tamper::Signature, "invalid signature on SHA256SUMS"),
        (Tamper::MissingArchive, "has no asset loom-"),
        (Tamper::ArchiveHttpError, "HTTP 500"),
    ];
    for (tamper, message) in cases {
        let env = Env::new();
        env.publish_latest(NEW, tamper);
        let before = env.installed();
        let output = env.loom(&["upgrade", "--channel", "stable"]);
        assert_failure(&output, message);
        assert!(stderr(&output).contains("nothing was installed"));
        assert_eq!(env.installed(), before, "{message}");
        assert!(!env.versions_dir().exists(), "{message}");
        // The staging directory is cleaned up.
        let staging = env.home.path().join("staging");
        let leftovers = fs::read_dir(&staging).map_or(0, Iterator::count);
        assert_eq!(leftovers, 0, "{message}");
    }
}

#[test]
fn api_errors_fail_cleanly() {
    let env = Env::new();
    env.api.route(&format!("{RELEASES}/latest"), 500, "oops");
    let before = env.installed();
    assert_failure(&env.loom(&["upgrade", "--channel", "stable"]), "HTTP 500");
    assert_eq!(env.installed(), before);

    env.api.route(&format!("{RELEASES}/latest"), 403, "{}");
    assert_failure(
        &env.loom(&["upgrade", "--channel", "stable", "--check"]),
        "LOOM_GITHUB_TOKEN",
    );
}

#[test]
fn the_release_key_rejects_a_signature_from_another_key() {
    let env = Env::new();
    env.publish_latest(NEW, Tamper::None);
    let before = env.installed();
    let output = Command::new(env.install.path().join("loom"))
        .args(["upgrade", "--channel", "stable"])
        .env("LOOM_HOME", env.home.path())
        .env("LOOM_RELEASES_URL", &env.api.url)
        .env_remove("LOOM_UPDATE_PUBKEY")
        .env_remove("LOOM_GITHUB_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .output()
        .unwrap();
    assert_failure(&output, "invalid signature on SHA256SUMS");
    assert_eq!(env.installed(), before);
}

#[test]
fn token_is_sent_as_a_bearer() {
    let env = Env::new();
    env.publish_latest(NEW, Tamper::None);
    let output = Command::new(env.install.path().join("loom"))
        .args(["upgrade", "--channel", "stable", "--check"])
        .env("LOOM_HOME", env.home.path())
        .env("LOOM_RELEASES_URL", &env.api.url)
        .env("LOOM_GITHUB_TOKEN", "test-token")
        .env_remove("GITHUB_TOKEN")
        .output()
        .unwrap();
    assert_success(&output);
    let requests = env.api.requests();
    assert!(
        requests.iter().all(|head| head
            .to_ascii_lowercase()
            .contains("authorization: bearer test-token")),
        "{requests:?}"
    );
    assert!(!stderr(&output).contains("test-token"));
}

#[test]
fn unwritable_install_directory_fails_before_downloading() {
    let env = Env::new();
    env.publish_latest(NEW, Tamper::None);
    let dir = env.install.path();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
    // Root ignores the mode: nothing to test then.
    let writable = File::create(dir.join("probe")).is_ok();
    if !writable {
        let output = env.loom(&["upgrade", "--channel", "stable"]);
        assert_failure(&output, "the directory is not writable");
        assert_eq!(env.asset_requests(), 0);
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn dry_run_downloads_and_verifies_but_changes_nothing() {
    let env = Env::new();
    env.publish_latest(NEW, Tamper::None);
    let before = env.installed();
    let output = env.loom(&["upgrade", "--channel", "stable", "--dry-run"]);
    assert_success(&output);
    assert_eq!(
        stdout(&output),
        format!(
            "would upgrade loom and loomd from {} to {NEW} in {}\n",
            env.current,
            env.install.path().canonicalize().unwrap().display()
        )
    );
    assert!(env.asset_requests() >= 3, "downloaded and verified");
    assert_eq!(env.installed(), before);
    assert!(!env.versions_dir().exists());

    // A tampered release fails the dry run too.
    env.publish_latest(NEW, Tamper::Checksum);
    assert_failure(
        &env.loom(&["upgrade", "--channel", "stable", "-n"]),
        "checksum mismatch",
    );
}

// --- --from -------------------------------------------------------------------------------

#[test]
fn from_archive_installs_an_unverified_local_build() {
    let env = Env::new();
    let build = tempfile::tempdir().unwrap();
    let path = build.path().join("loom.tar.gz");
    fs::write(&path, archive("50.0.0")).unwrap();

    let output = env.loom(&["upgrade", "--from", path.to_str().unwrap(), "--json"]);
    assert!(
        stderr(&output).contains("not verified"),
        "{}",
        stderr(&output)
    );
    let report = upgrade_report(&output);
    assert_eq!(report.decision, apply("50.0.0"));
    assert!(!report.install.unwrap().verified);
    assert_eq!(
        env.installed(),
        stub_binaries("50.0.0").map(|(_, data)| data)
    );
}

#[test]
fn from_directory_installs_even_an_older_version() {
    let env = Env::new();
    let build = tempfile::tempdir().unwrap();
    write_stubs(build.path(), "0.0.3");
    let output = env.loom(&["upgrade", "--from", build.path().to_str().unwrap()]);
    assert_success(&output);
    assert!(stdout(&output).starts_with(&format!(
        "upgraded loom and loomd from {} to 0.0.3 in ",
        env.current
    )));
    assert_eq!(
        env.installed(),
        stub_binaries("0.0.3").map(|(_, data)| data)
    );
    assert_eq!(env.saved(), vec![env.current]);
}

#[test]
fn from_rejects_an_incomplete_build() {
    let env = Env::new();
    let build = tempfile::tempdir().unwrap();
    fs::write(build.path().join("loom"), "x").unwrap();
    let before = env.installed();
    assert_failure(
        &env.loom(&["upgrade", "--from", build.path().to_str().unwrap()]),
        "has no loomd file",
    );
    assert_eq!(env.installed(), before);
}

// --- rollback -----------------------------------------------------------------------------

#[test]
fn rollback_restores_and_saves_the_replaced_version() {
    let env = Env::new();
    write_stubs(&env.versions_dir().join("0.0.5"), "0.0.5");
    write_stubs(&env.versions_dir().join("0.0.4"), "0.0.4");
    let before = env.installed();

    let output = env.loom(&["rollback", "--json"]);
    assert_success(&output);
    let report: RollbackReport = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report.install.version, "0.0.5");
    assert!(report.install.installed && !report.install.verified);
    assert_eq!(
        env.installed(),
        stub_binaries("0.0.5").map(|(_, data)| data)
    );
    let saved = env.versions_dir().join(&env.current);
    assert_eq!(fs::read(saved.join("loom")).unwrap(), before[0]);
}

#[test]
fn rollback_dry_run_changes_nothing() {
    let env = Env::new();
    write_stubs(&env.versions_dir().join("0.0.5"), "0.0.5");
    let before = env.installed();
    let output = env.loom(&["rollback", "-n"]);
    assert_success(&output);
    assert!(stdout(&output).starts_with(&format!(
        "would roll back loom and loomd from {} to 0.0.5 in ",
        env.current
    )));
    assert_eq!(env.installed(), before);
    assert_eq!(env.saved(), vec!["0.0.5".to_owned()]);
}

#[test]
fn nothing_to_roll_back_to() {
    let env = Env::new();
    assert_failure(&env.loom(&["rollback"]), "nothing to roll back to");
    // A save of the running version is not a previous one.
    write_stubs(&env.versions_dir().join(&env.current), &env.current);
    assert_failure(&env.loom(&["rollback"]), "nothing to roll back to");
}

// --- daemon restart -----------------------------------------------------------------------

fn lock_file(env: &Env, pid: u32) -> PathBuf {
    let run = env.home.path().join("run");
    fs::create_dir_all(&run).unwrap();
    let lock = run.join("loomd.lock");
    fs::write(&lock, format!("{pid}\n")).unwrap();
    lock
}

#[test]
fn upgrade_sends_sigterm_to_a_running_loomd() {
    let env = Env::new();
    env.publish_latest(NEW, Tamper::None);
    let mut daemon = Command::new("sleep").arg("60").spawn().unwrap();
    let lock = lock_file(&env, daemon.id());
    // Held like loomd holds it.
    let held = File::open(&lock).unwrap();
    held.try_lock().unwrap();

    let output = env.loom(&["upgrade", "--channel", "stable", "--json"]);
    let report = upgrade_report(&output);
    assert_eq!(
        report.install.unwrap().daemon,
        DaemonRestart::Signalled { pid: daemon.id() }
    );
    assert_eq!(daemon.wait().unwrap().signal(), Some(15));
}

#[test]
fn stale_pid_is_not_signalled() {
    let env = Env::new();
    write_stubs(&env.versions_dir().join("0.0.5"), "0.0.5");
    let mut bystander = Command::new("sleep").arg("60").spawn().unwrap();
    // A PID in a lock file nobody holds: loomd is gone, the PID may be reused.
    lock_file(&env, bystander.id());

    let output = env.loom(&["rollback", "--json"]);
    assert_success(&output);
    let report: RollbackReport = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report.install.daemon, DaemonRestart::NotRunning);
    assert!(bystander.try_wait().unwrap().is_none(), "not signalled");
    bystander.kill().unwrap();
    bystander.wait().unwrap();
}
