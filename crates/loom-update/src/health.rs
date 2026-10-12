//! Health check of a loomd: `GET /v1/status` on its unix socket, blocking, without an async
//! runtime. Used by the pre-flight check and the watchdog.

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use loom_api::StatusResponse;

/// Largest response read.
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

/// `GET /v1/status` on `socket`, within `timeout` for each of connect, write and read.
///
/// # Errors
///
/// A message: nothing listens, no answer in time, not HTTP 200, or not a `StatusResponse`.
pub fn status(socket: &Path, timeout: Duration) -> Result<StatusResponse, String> {
    let mut stream = UnixStream::connect(socket)
        .map_err(|error| format!("cannot connect to {}: {error}", socket.display()))?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| format!("cannot set a timeout on {}: {error}", socket.display()))?;
    stream
        .write_all(b"GET /v1/status HTTP/1.1\r\nHost: loomd\r\nConnection: close\r\n\r\n")
        .map_err(|error| format!("cannot send the request: {error}"))?;
    let mut response = Vec::new();
    stream
        .take(MAX_RESPONSE_BYTES)
        .read_to_end(&mut response)
        .map_err(|error| format!("no complete answer: {error}"))?;
    parse_response(&response)
}

/// Body of an HTTP/1.1 response with status 200 and a `content-length`, as a
/// `StatusResponse`. Pure.
fn parse_response(response: &[u8]) -> Result<StatusResponse, String> {
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "the answer is not HTTP".to_owned())?;
    let head = String::from_utf8_lossy(&response[..split]);
    let body = &response[split + 4..];
    let status_line = head.lines().next().unwrap_or_default();
    if status_line.split_whitespace().nth(1) != Some("200") {
        return Err(format!("unexpected answer `{status_line}`"));
    }
    serde_json::from_slice(body).map_err(|error| format!("invalid status body: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::thread;

    fn body() -> String {
        serde_json::json!({
            "api_version": "v1",
            "daemon": {
                "version": "0.2.0", "commit": "unknown", "commit_date": "unknown",
                "target": "x", "channel": "stable", "profile": "release"
            },
            "pid": 7,
            "started_at": "2026-10-12T08:00:00Z",
            "uptime_secs": 1,
            "loom_home": "/h",
            "socket_path": "/h/run/loomd.sock",
            "config_path": "/h/config.toml"
        })
        .to_string()
    }

    #[test]
    fn parses_a_200_with_a_status_body() {
        let body = body();
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        let status = parse_response(response.as_bytes()).unwrap();
        assert_eq!(status.daemon.version, "0.2.0");
        assert_eq!(status.update, None);
    }

    #[test]
    fn rejects_errors_and_garbage() {
        assert!(parse_response(b"garbage").is_err());
        let error = parse_response(b"HTTP/1.1 500 Internal\r\n\r\n{}").unwrap_err();
        assert!(error.contains("500"), "{error}");
        assert!(parse_response(b"HTTP/1.1 200 OK\r\n\r\n{}").is_err());
    }

    #[test]
    fn queries_a_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let body = body();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 256];
            let read = stream.read(&mut request).unwrap();
            assert!(request[..read].starts_with(b"GET /v1/status HTTP/1.1\r\n"));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let status = status(&socket, Duration::from_secs(5)).unwrap();
        assert_eq!(status.pid, 7);
        server.join().unwrap();
        assert!(super::status(&dir.path().join("missing.sock"), Duration::from_secs(1)).is_err());
    }
}
