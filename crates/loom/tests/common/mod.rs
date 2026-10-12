//! Fixtures shared by the `upgrade`, `auto_upgrade` and `telemetry` tests: a local fake of the
//! GitHub releases API, release archives, a throwaway minisign key, and a fake OTLP/HTTP
//! collector.
//!
//! Requires the `minisign` CLI (pinned in `.mise/config.toml`; `mise run test`).

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "each test crate uses part of the fixtures; a failing helper must fail the test"
)]

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

use flate2::Compression;
use flate2::write::GzEncoder;
use loom_api::VersionInfo;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{KeyValue, any_value};
use opentelemetry_proto::tonic::trace::v1::Span;
use prost::Message as _;
use tempfile::TempDir;

// --- fake GitHub API ----------------------------------------------------------------------

pub type Routes = Arc<Mutex<HashMap<String, (u16, Vec<u8>)>>>;

/// HTTP/1.1 server answering `GET <path>` from a route table, one connection at a time.
pub struct FakeApi {
    pub url: String,
    routes: Routes,
    requests: Arc<Mutex<Vec<String>>>,
}

impl FakeApi {
    pub fn start() -> Self {
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

    pub fn route(&self, path: &str, status: u16, body: impl Into<Vec<u8>>) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.to_owned(), (status, body.into()));
    }

    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    pub fn asset_requests(&self) -> usize {
        self.requests()
            .iter()
            .filter(|head| head.contains("/releases/assets/"))
            .count()
    }
}

pub const RELEASES: &str = "/repos/gsuquet/loom/releases";

// --- fixtures -----------------------------------------------------------------------------

pub fn version_json(version: &str) -> String {
    let info = VersionInfo {
        version: version.to_owned(),
        ..VersionInfo::current()
    };
    serde_json::to_string(&info).unwrap()
}

/// Stub `loom` and `loomd` scripts reporting `version`.
pub fn stub_binaries(version: &str) -> [(&'static str, Vec<u8>); 2] {
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

pub fn write_stubs(dir: &Path, version: &str) {
    fs::create_dir_all(dir).unwrap();
    for (name, data) in stub_binaries(version) {
        fs::write(dir.join(name), data).unwrap();
        fs::set_permissions(dir.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// Release archive with the stubs of `version`, `LICENSE` and `README.md`.
pub fn archive(version: &str) -> Vec<u8> {
    archive_of(stub_binaries(version))
}

/// Release archive with the given `loom` and `loomd`, `LICENSE` and `README.md`.
pub fn archive_of(binaries: [(&'static str, Vec<u8>); 2]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    let mut files: Vec<(&str, Vec<u8>, u32)> = binaries
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
pub struct Signer {
    dir: TempDir,
    pub public: String,
}

impl Signer {
    pub fn new() -> Self {
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

    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
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

// --- fake OTLP collector -----------------------------------------------------------------

/// A fake OTLP/HTTP collector: answers `200` to every request and keeps each body decoded.
pub struct Collector {
    pub url: String,
    requests: Arc<Mutex<Vec<(String, ExportTraceServiceRequest)>>>,
}

impl Collector {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = Arc::clone(&requests);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let received = Arc::clone(&received);
                thread::spawn(move || serve(&stream, &received));
            }
        });
        Self { url, requests }
    }

    /// Spans received so far, with the `service.name` of their resource.
    pub fn spans(&self) -> Vec<(String, Span)> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .flat_map(|(_, request)| request.resource_spans.iter())
            .flat_map(|resource_spans| {
                let service = resource_spans
                    .resource
                    .as_ref()
                    .and_then(|resource| string_attribute(&resource.attributes, "service.name"))
                    .unwrap_or_default();
                resource_spans
                    .scope_spans
                    .iter()
                    .flat_map(|scope| scope.spans.iter())
                    .map(move |span| (service.clone(), span.clone()))
            })
            .collect()
    }

    pub fn resource_attribute(&self, service: &str, key: &str) -> Option<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .flat_map(|(_, request)| request.resource_spans.iter())
            .filter_map(|resource_spans| resource_spans.resource.as_ref())
            .filter(|resource| {
                string_attribute(&resource.attributes, "service.name").as_deref() == Some(service)
            })
            .find_map(|resource| string_attribute(&resource.attributes, key))
    }

    pub fn paths(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|(path, _)| path.clone())
            .collect()
    }
}

/// Serves the HTTP/1.1 requests of one connection until the client closes it.
fn serve(stream: &TcpStream, received: &Mutex<Vec<(String, ExportTraceServiceRequest)>>) {
    let mut reader = BufReader::new(stream);
    loop {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
            return;
        }
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_owned();
        let mut length = 0;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let request = ExportTraceServiceRequest::decode(body.as_slice()).unwrap();
        received.lock().unwrap().push((path, request));
        let mut writer = stream;
        writer
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/x-protobuf\r\ncontent-length: 0\r\n\r\n",
            )
            .unwrap();
    }
}

pub fn string_attribute(attributes: &[KeyValue], key: &str) -> Option<String> {
    attributes
        .iter()
        .find(|kv| kv.key == key)
        .and_then(|kv| kv.value.as_ref())
        .and_then(|value| match &value.value {
            Some(any_value::Value::StringValue(text)) => Some(text.clone()),
            _ => None,
        })
}
