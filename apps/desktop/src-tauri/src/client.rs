//! Small copy of `crates/loom/src/client.rs`: HTTP/1.1 over the unix socket, one connection per
//! request, plus what the copy lacks: raw JSON answers and an SSE stream.

use std::path::{Path, PathBuf};
use std::time::Duration;

use http_body_util::{BodyExt as _, Full};
use hyper::Request;
use hyper::body::{Bytes, Incoming};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use serde::Serialize;
use tokio::net::UnixStream;

const HOST: &str = "loomd";
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a call failed. It crosses IPC, so it is plain data with no path or body of a request.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientError {
    #[error("cannot connect to loomd: {message}")]
    Unreachable { message: String },
    #[error("loomd returned HTTP {status}: {body}")]
    Api { status: u16, body: String },
    #[error("unexpected answer from loomd: {message}")]
    Protocol { message: String },
    #[error("this route is not allowed from the app")]
    NotAllowed,
}

fn protocol(context: &str, error: &dyn std::error::Error) -> ClientError {
    ClientError::Protocol {
        message: format!("{context}: {error}"),
    }
}

async fn open(
    socket: &Path,
    method: &str,
    path: &str,
    last_event_id: Option<&str>,
    body: Option<Vec<u8>>,
) -> Result<hyper::Response<Incoming>, ClientError> {
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|e| ClientError::Unreachable {
            message: e.to_string(),
        })?;
    let (mut sender, connection) = http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| protocol("cannot start HTTP", &e))?;
    // Drives the connection; it ends with the response, or with an error `send_request` reports.
    drop(tokio::spawn(connection));
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("host", HOST);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    if let Some(id) = last_event_id {
        builder = builder.header("last-event-id", id);
    }
    let request = builder
        .body(Full::new(Bytes::from(body.unwrap_or_default())))
        .map_err(|e| protocol("invalid request", &e))?;
    sender
        .send_request(request)
        .await
        .map_err(|e| protocol("request failed", &e))
}

/// One request; the answer stays raw JSON, since the webview decodes it with generated types.
pub async fn request(
    socket: &Path,
    method: &str,
    path: &str,
    body: Option<Vec<u8>>,
) -> Result<serde_json::Value, ClientError> {
    let exchange = async {
        let response = open(socket, method, path, None, body).await?;
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|e| protocol("cannot read the body", &e))?
            .to_bytes();
        Ok::<_, ClientError>((status, bytes))
    };
    let (status, bytes) = tokio::time::timeout(REQUEST_TIMEOUT, exchange)
        .await
        .map_err(|_| ClientError::Protocol {
            message: format!("no answer within {} ms", REQUEST_TIMEOUT.as_millis()),
        })??;
    if status.is_success() {
        serde_json::from_slice(&bytes).map_err(|e| protocol("invalid JSON", &e))
    } else {
        Err(ClientError::Api {
            status: status.as_u16(),
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }
}

/// One Server-Sent Event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SseEvent {
    pub id: Option<String>,
    pub event: String,
    pub data: String,
}

/// Incremental SSE parser. Pure: bytes in, events out. Chunks may cut lines and UTF-8 anywhere.
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    id: Option<String>,
    event: String,
    data: Vec<String>,
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.buffer.drain(..=end).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line);
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(SseEvent {
                        id: self.id.take(),
                        event: std::mem::take(&mut self.event),
                        data: self.data.join("\n"),
                    });
                    self.data.clear();
                }
                self.id = None;
                self.event.clear();
            } else if !line.starts_with(':') {
                let (field, value) = line.split_once(':').unwrap_or((&line, ""));
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "id" => self.id = Some(value.to_owned()),
                    "event" => value.clone_into(&mut self.event),
                    "data" => self.data.push(value.to_owned()),
                    _ => {}
                }
            }
        }
        out
    }
}

/// Reads `/v1/events` until the stream ends. No timeout: the stream stays open. `on_event`
/// returns false to stop.
pub async fn stream_events(
    socket: &PathBuf,
    last_event_id: Option<&str>,
    mut on_event: impl FnMut(SseEvent) -> bool,
) -> Result<(), ClientError> {
    let response = tokio::time::timeout(
        REQUEST_TIMEOUT,
        open(socket, "GET", "/v1/events", last_event_id, None),
    )
    .await
    .map_err(|_| ClientError::Protocol {
        message: "no answer to GET /v1/events".to_owned(),
    })??;
    if !response.status().is_success() {
        return Err(ClientError::Api {
            status: response.status().as_u16(),
            body: String::new(),
        });
    }
    let mut body = response.into_body();
    let mut parser = SseParser::default();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| protocol("stream failed", &e))?;
        if let Some(data) = frame.data_ref() {
            for event in parser.push(data) {
                if !on_event(event) {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_cut_across_chunks() {
        let mut p = SseParser::default();
        assert!(p.push(b"id: 1-7\nevent: task.ch").is_empty());
        let out = p.push(b"anged\ndata: {\"id\":\"\xc3").into_iter().count();
        assert_eq!(out, 0);
        let out = p.push(b"\xa9\"}\n\n");
        assert_eq!(
            out,
            vec![SseEvent {
                id: Some("1-7".into()),
                event: "task.changed".into(),
                data: "{\"id\":\"é\"}".into(),
            }]
        );
    }

    #[test]
    fn ignores_heartbeats_and_keeps_crlf_apart() {
        let mut p = SseParser::default();
        assert!(p.push(b": heartbeat\n\n").is_empty());
        let out = p.push(b"event: resync\r\ndata: {}\r\n\r\n");
        assert_eq!(out[0].event, "resync");
        assert_eq!(out[0].id, None);
    }

    #[test]
    fn joins_several_data_lines() {
        let mut p = SseParser::default();
        let out = p.push(b"data: a\ndata: b\n\n");
        assert_eq!(out[0].data, "a\nb");
    }
}
