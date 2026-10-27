//! The logs and spans of the forge credential check never hold the token. Alone in its test
//! binary: tracing caches whether a callsite is enabled, and tests on other threads without a
//! subscriber would disable the callsites for this one.

#![allow(
    clippy::unwrap_used,
    reason = "`allow-unwrap-in-tests` covers `#[test]` functions only; a failing helper must fail the test"
)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

use loom_api::{CredentialOrigin, ForgeCheckOutcome, ForgeKind};
use loom_config::forges::TokenSource;
use loomd::forge::ApiBase;
use loomd::forge::check::{ForgeTarget, check_forge};
use loomd::forge::client::HttpForge;
use loomd::forge::token::TokenResolver;
use tracing_subscriber::fmt::format::FmtSpan;

const TOKEN: &str = "fake-token-0123456789";

/// A fake forge that answers every request with `status` and `body`.
fn serve(status: u16, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
                line.clear();
            }
            let answer = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(answer.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// A writer into a shared buffer.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn logs_and_spans_never_hold_the_token() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_span_events(FmtSpan::FULL)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let check = |kind, base: String, source| {
        let target = ForgeTarget {
            context: "personal".parse().unwrap(),
            host: "forge.example".to_owned(),
            kind,
            api: ApiBase {
                rest: base,
                graphql: None,
            },
            origin: CredentialOrigin::Host,
            source,
        };
        let resolver = TokenResolver::default();
        let client = HttpForge::new(kind, target.api.clone(), "loom-test");
        check_forge(&target, &resolver, &client).result
    };
    let source = TokenSource::Command(format!("printf '{TOKEN}'"));
    // A 401: the token is resolved again, the call made twice.
    let rejected = check(
        ForgeKind::Github,
        serve(401, r#"{"message":"Bad credentials"}"#),
        source.clone(),
    );
    assert!(matches!(
        rejected,
        ForgeCheckOutcome::Rejected { status: 401, .. }
    ));
    // A GitLab token whose scopes cannot be read.
    let accepted = check(
        ForgeKind::Gitlab,
        serve(200, r#"{"username":"carol"}"#),
        source,
    );
    assert!(matches!(accepted, ForgeCheckOutcome::Authenticated { .. }));

    let output = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(output.contains("forge.check"), "{output}");
    assert!(output.contains("forge.token_cmd"), "{output}");
    assert!(!output.contains(TOKEN), "{output}");
}
