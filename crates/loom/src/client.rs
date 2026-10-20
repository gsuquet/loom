//! Client of the loomd API: HTTP/1.1 over the unix socket, one connection per request.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use http_body_util::{BodyExt as _, Full};
use hyper::body::{Bytes, Incoming};
use hyper::client::conn::http1;
use hyper::{Method, Request, header};
use hyper_util::rt::TokioIo;
use loom_api::{ApiError, ContextName, EventCursor, StatusResponse};
use loom_telemetry::propagation::inject_context;
use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::net::UnixStream;
use tracing::field;
use tracing::{Instrument as _, Span, info_span};

use crate::sse::{SseFrame, SseParser};

/// Longest a request may take, connection included.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Path of the status endpoint.
const STATUS_PATH: &str = "/v1/status";
/// Path of the SSE change events endpoint.
#[allow(dead_code)]
const EVENTS_PATH: &str = "/v1/events";
/// Header sent on SSE reconnect to resume from the last seen event.
#[allow(dead_code)]
const LAST_EVENT_ID_HEADER: &str = "last-event-id";
/// Default maximum duration between incoming frames (heartbeats or events) before timing out.
#[allow(dead_code)]
pub const DEFAULT_SSE_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(45);
/// `host` header of every request: required by HTTP/1.1, ignored by loomd.
const HOST: &str = "loomd";

/// Why a request to loomd failed.
#[derive(Debug, Error)]
pub enum ClientError {
    /// Nothing accepts connections on the socket: it is missing, or no process listens on it.
    #[error("cannot connect to loomd at {}: {source}", socket.display())]
    Unreachable {
        /// The socket path.
        socket: PathBuf,
        /// The connection error.
        #[source]
        source: io::Error,
    },
    /// loomd answered with an error status and an [`ApiError`] body.
    #[error("loomd returned an error (HTTP {status}, {}): {}", error.code, error.message)]
    Api {
        /// HTTP status code.
        status: u16,
        /// The error body.
        error: ApiError,
    },
    /// Anything else: no answer in time, broken connection, unexpected status or body.
    #[error("unexpected answer from loomd: {0}")]
    Protocol(String),
}

/// Client of the loomd API listening on a unix socket.
#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
    timeout: Duration,
}

impl Client {
    /// Client of the daemon listening on `socket`, with [`REQUEST_TIMEOUT`].
    #[must_use]
    pub const fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            timeout: REQUEST_TIMEOUT,
        }
    }

    /// The same client, each request taking at most `timeout`, connection included.
    #[must_use]
    pub fn with_timeout(&self, timeout: Duration) -> Self {
        Self {
            socket: self.socket.clone(),
            timeout,
        }
    }

    /// Path of the socket.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// `GET /v1/status`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] if loomd cannot be reached or returns an error.
    pub async fn status(&self) -> Result<StatusResponse, ClientError> {
        self.get(STATUS_PATH).await
    }

    /// `GET path`, with the JSON body of a successful response decoded as `T`. Traced as a
    /// client span whose context is sent to loomd.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] if the request fails or decoding fails.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ClientError> {
        self.call(Method::GET, path, None).await
    }

    /// `POST path` with `body` as JSON (an empty body when `None`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] if encoding, the request or decoding fails.
    pub async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: Option<&(impl Serialize + Sync)>,
    ) -> Result<T, ClientError> {
        let body = body.map(serde_json::to_vec).transpose().map_err(|error| {
            ClientError::Protocol(format!("cannot encode the request: {error}"))
        })?;
        self.call(Method::POST, path, body).await
    }

    /// `PATCH path` with `body` as JSON.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] if encoding, the request or decoding fails.
    pub async fn patch<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &(impl Serialize + Sync),
    ) -> Result<T, ClientError> {
        let body = serde_json::to_vec(body).map_err(|error| {
            ClientError::Protocol(format!("cannot encode the request: {error}"))
        })?;
        self.call(Method::PATCH, path, Some(body)).await
    }

    /// `DELETE path`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] if the request fails or decoding fails.
    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T, ClientError> {
        self.call(Method::DELETE, path, None).await
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<T, ClientError> {
        let span = info_span!(
            "http.client",
            otel.name = %format!("{method} {path}"),
            otel.kind = "client",
            otel.status_code = field::Empty,
            otel.status_description = field::Empty,
            http.request.method = method.as_str(),
            url.path = path,
            http.response.status_code = field::Empty,
        );
        let result = self
            .request(&method, path, body)
            .instrument(span.clone())
            .await;
        if let Err(error) = &result {
            span.record("otel.status_code", "ERROR");
            span.record("otel.status_description", error.to_string());
        }
        result
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: &Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<T, ClientError> {
        let (status, body) = tokio::time::timeout(self.timeout, self.exchange(method, path, body))
            .await
            .map_err(|_| {
                ClientError::Protocol(format!(
                    "no answer to {method} {path} within {} ms",
                    self.timeout.as_millis()
                ))
            })??;
        if status.is_success() {
            return serde_json::from_slice(&body).map_err(|error| {
                ClientError::Protocol(format!("invalid body for {method} {path}: {error}"))
            });
        }
        let error = serde_json::from_slice::<ApiError>(&body).map_or_else(
            |_| {
                ClientError::Protocol(format!(
                    "HTTP {status} for {method} {path} without an error body"
                ))
            },
            |error| ClientError::Api {
                status: status.as_u16(),
                error,
            },
        );
        Err(error)
    }

    /// Connects, sends the request and reads the whole response.
    async fn exchange(
        &self,
        method: &Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<(hyper::StatusCode, Bytes), ClientError> {
        let stream =
            UnixStream::connect(&self.socket)
                .await
                .map_err(|source| ClientError::Unreachable {
                    socket: self.socket.clone(),
                    source,
                })?;
        let (mut sender, connection) = http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|error| protocol("cannot start HTTP", &error))?;
        // Drives the connection; it ends with the response or with an error that
        // `send_request` reports too.
        drop(tokio::spawn(connection));

        let mut builder = Request::builder()
            .method(method.clone())
            .uri(path)
            .header(header::HOST, HOST);
        if body.is_some() {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        let mut request = builder
            .body(Full::new(Bytes::from(body.unwrap_or_default())))
            .map_err(|error| protocol("invalid request", &error))?;
        inject_context(&Span::current(), request.headers_mut());
        let response = sender
            .send_request(request)
            .await
            .map_err(|error| protocol("request failed", &error))?;
        let status = response.status();
        Span::current().record("http.response.status_code", i64::from(status.as_u16()));
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|error| protocol("cannot read the body", &error))?
            .to_bytes();
        Ok((status, body))
    }

    /// Subscribes to `GET /v1/events`, optionally filtered by `context` and resuming after
    /// `last_event_id` via the `Last-Event-ID` header.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] if the connection fails or protocol negotiation fails.
    #[allow(dead_code)]
    pub async fn subscribe_events(
        &self,
        context: Option<&ContextName>,
        last_event_id: Option<EventCursor>,
    ) -> Result<EventStream, ClientError> {
        let path = context.map_or_else(
            || EVENTS_PATH.to_owned(),
            |ctx| format!("{EVENTS_PATH}?context={ctx}"),
        );
        tokio::time::timeout(self.timeout, self.open_events(&path, last_event_id))
            .await
            .map_err(|_| {
                ClientError::Protocol(format!(
                    "no answer to GET {path} within {} ms",
                    self.timeout.as_millis()
                ))
            })?
    }

    #[allow(dead_code)]
    async fn open_events(
        &self,
        path: &str,
        last_event_id: Option<EventCursor>,
    ) -> Result<EventStream, ClientError> {
        let stream =
            UnixStream::connect(&self.socket)
                .await
                .map_err(|source| ClientError::Unreachable {
                    socket: self.socket.clone(),
                    source,
                })?;
        let (mut sender, connection) = http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|error| protocol("cannot start HTTP", &error))?;
        drop(tokio::spawn(connection));

        let mut builder = Request::builder()
            .method(Method::GET)
            .uri(path)
            .header(header::HOST, HOST)
            .header(header::ACCEPT, "text/event-stream");
        if let Some(cursor) = last_event_id {
            builder = builder.header(LAST_EVENT_ID_HEADER, cursor.to_string());
        }
        let mut request = builder
            .body(Full::new(Bytes::new()))
            .map_err(|error| protocol("invalid request", &error))?;
        inject_context(&Span::current(), request.headers_mut());

        let response = sender
            .send_request(request)
            .await
            .map_err(|error| protocol("request failed", &error))?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .into_body()
                .collect()
                .await
                .map_err(|error| protocol("cannot read the body", &error))?
                .to_bytes();
            let error = serde_json::from_slice::<ApiError>(&body).map_or_else(
                |_| {
                    ClientError::Protocol(format!(
                        "HTTP {status} for GET {path} without an error body"
                    ))
                },
                |error| ClientError::Api {
                    status: status.as_u16(),
                    error,
                },
            );
            return Err(error);
        }

        Ok(EventStream {
            body: response.into_body(),
            parser: SseParser::new(),
            pending: VecDeque::new(),
            last_event_id,
            heartbeat_timeout: DEFAULT_SSE_HEARTBEAT_TIMEOUT,
        })
    }
}

/// Open Server-Sent Events stream from `GET /v1/events`.
#[allow(dead_code)]
pub struct EventStream {
    body: Incoming,
    parser: SseParser,
    pending: VecDeque<SseFrame>,
    last_event_id: Option<EventCursor>,
    heartbeat_timeout: Duration,
}

#[allow(dead_code)]
impl EventStream {
    /// Overrides the idle heartbeat timeout for this stream (defaults to 45 seconds).
    #[must_use]
    pub const fn with_heartbeat_timeout(mut self, timeout: Duration) -> Self {
        self.heartbeat_timeout = timeout;
        self
    }

    /// Last received event cursor, if any.
    #[must_use]
    pub const fn last_event_id(&self) -> Option<EventCursor> {
        self.last_event_id
    }

    /// Reads the next [`SseFrame`] from the stream, or `Ok(None)` when the server closes the
    /// stream.
    ///
    /// Times out if no data or heartbeat comment arrives within `heartbeat_timeout`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] if reading fails, idle timeout expires, or event parsing fails.
    pub async fn next(&mut self) -> Result<Option<SseFrame>, ClientError> {
        loop {
            if let Some(frame) = self.pending.pop_front() {
                if let Some(id) = frame.id {
                    self.last_event_id = Some(id);
                }
                return Ok(Some(frame));
            }

            let frame_result =
                match tokio::time::timeout(self.heartbeat_timeout, self.body.frame()).await {
                    Ok(Some(result)) => result,
                    Ok(None) => return Ok(None),
                    Err(_) => {
                        return Err(ClientError::Protocol(format!(
                            "SSE heartbeat timeout: no data or heartbeat received in {} ms",
                            self.heartbeat_timeout.as_millis()
                        )));
                    }
                };

            let frame = frame_result.map_err(|error| protocol("SSE stream error", &error))?;
            if let Ok(data) = frame.into_data() {
                for event in self.parser.feed(&data) {
                    self.pending.push_back(event);
                }
            }
        }
    }
}

fn protocol(context: &str, error: &dyn std::error::Error) -> ClientError {
    ClientError::Protocol(format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use axum::routing::get;
    use axum::{Json, Router, middleware};
    use loom_api::{ApiVersion, StatusResponse, Timestamp, VersionInfo};
    use loomd::trace::request_span;
    use opentelemetry::Value;
    use opentelemetry::trace::{SpanId, SpanKind};
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
    use tokio::net::UnixListener;
    use tracing::level_filters::LevelFilter;
    use tracing::{Instrument as _, info_span};
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::{Client, ClientError, STATUS_PATH};

    fn status(socket: &Path) -> StatusResponse {
        StatusResponse {
            api_version: ApiVersion::CURRENT,
            daemon: VersionInfo::current(),
            pid: 4242,
            started_at: Timestamp::from_unix_secs(1_791_792_000).unwrap(),
            uptime_secs: 1,
            loom_home: "/home/me/.loom".into(),
            socket_path: socket.to_path_buf(),
            config_path: "/home/me/.loom/config.toml".into(),
            update: None,
            disk: None,
        }
    }

    fn attribute<'a>(span: &'a SpanData, key: &str) -> Option<&'a Value> {
        span.attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| &kv.value)
    }

    fn find<'a>(spans: &'a [SpanData], name: &str, kind: &SpanKind) -> &'a SpanData {
        spans
            .iter()
            .find(|span| span.name == name && span.span_kind == *kind)
            .unwrap_or_else(|| panic!("no {kind:?} span {name} in {spans:#?}"))
    }

    /// The client and an in-process router with loomd's tracing middleware share one in-memory
    /// provider: the server span must be the child of the client span, through the headers.
    #[test]
    fn server_span_is_a_child_of_the_client_span() {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry().with(loom_telemetry::otel_layer(
            &provider,
            "test",
            LevelFilter::INFO,
        ));
        // Under `/tmp`: the macOS default `$TMPDIR` is too long for a socket path.
        let dir = tempfile::Builder::new()
            .prefix("loom-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = dir.path().join("loomd.sock");

        tracing::subscriber::with_default(subscriber, || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let listener = UnixListener::bind(&socket).unwrap();
                let body = status(&socket);
                let router = Router::new()
                    .route(STATUS_PATH, get(move || async move { Json(body) }))
                    .layer(middleware::from_fn(request_span));
                let server = tokio::spawn(async move { axum::serve(listener, router).await });

                let client = Client::new(socket.clone());
                let root = info_span!("command", otel.name = "loom status");
                let answer = client.status().instrument(root.clone()).await.unwrap();
                assert_eq!(answer.pid, 4242);
                let missing = client
                    .get::<serde_json::Value>("/v1/missing")
                    .instrument(root)
                    .await;
                assert!(
                    matches!(missing, Err(ClientError::Protocol(_))),
                    "{missing:?}"
                );
                server.abort();
            });
        });
        provider.force_flush().unwrap();
        let spans = exporter.get_finished_spans().unwrap();

        let root = find(&spans, "loom status", &SpanKind::Internal);
        let client = find(&spans, "GET /v1/status", &SpanKind::Client);
        let server = find(&spans, "GET /v1/status", &SpanKind::Server);
        assert_eq!(root.parent_span_id, SpanId::INVALID);
        assert_eq!(client.parent_span_id, root.span_context.span_id());
        assert_eq!(server.parent_span_id, client.span_context.span_id());
        assert!(server.parent_span_is_remote);
        let trace_id = root.span_context.trace_id();
        assert_eq!(client.span_context.trace_id(), trace_id);
        assert_eq!(server.span_context.trace_id(), trace_id);

        assert_eq!(
            attribute(client, "http.request.method"),
            Some(&Value::from("GET"))
        );
        assert_eq!(
            attribute(client, "url.path"),
            Some(&Value::from(STATUS_PATH))
        );
        assert_eq!(
            attribute(client, "http.response.status_code"),
            Some(&Value::I64(200))
        );
        assert_eq!(
            attribute(server, "http.request.method"),
            Some(&Value::from("GET"))
        );
        assert_eq!(
            attribute(server, "url.path"),
            Some(&Value::from(STATUS_PATH))
        );
        assert_eq!(
            attribute(server, "http.route"),
            Some(&Value::from(STATUS_PATH))
        );
        assert_eq!(
            attribute(server, "http.response.status_code"),
            Some(&Value::I64(200))
        );

        // No route: the server span is named after the method only; the client span records
        // the error.
        let unrouted = find(&spans, "GET", &SpanKind::Server);
        assert_eq!(
            attribute(unrouted, "url.path"),
            Some(&Value::from("/v1/missing"))
        );
        assert_eq!(
            attribute(unrouted, "http.response.status_code"),
            Some(&Value::I64(404))
        );
        let failed = find(&spans, "GET /v1/missing", &SpanKind::Client);
        assert_eq!(unrouted.parent_span_id, failed.span_context.span_id());
        assert!(
            matches!(failed.status, opentelemetry::trace::Status::Error { .. }),
            "{:?}",
            failed.status
        );
    }

    #[test]
    fn subscribe_events_streams_sse_frames_and_passes_last_event_id() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("loomd.sock");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let listener = UnixListener::bind(&socket).unwrap();
            let router = Router::new().route(
                super::EVENTS_PATH,
                get(|headers: axum::http::HeaderMap| async move {
                    let body = if headers
                        .get("Last-Event-ID")
                        .is_some_and(|v| v == "1791792000000-1")
                    {
                        "id: 1791792000000-2\nevent: task.changed\ndata: {\"type\":\"task.changed\",\"id\":\"k3q7zm2a\",\"context\":\"personal\"}\n\n"
                    } else {
                        "id: 1791792000000-1\nevent: repo.changed\ndata: {\"type\":\"repo.changed\",\"slug\":\"personal/loom\"}\n\n: keep-alive\n\n"
                    };
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                        body,
                    )
                }),
            );
            let server = tokio::spawn(async move { axum::serve(listener, router).await });

            let client = Client::new(socket.clone());
            let mut stream = client.subscribe_events(None, None).await.unwrap();
            let first = stream.next().await.unwrap().unwrap();
            assert_eq!(first.id, Some("1791792000000-1".parse().unwrap()));
            assert_eq!(
                first.event,
                loom_api::ChangeEvent::RepoChanged {
                    slug: "personal/loom".to_owned(),
                }
            );
            assert_eq!(stream.last_event_id(), first.id);
            assert!(stream.next().await.unwrap().is_none());

            let mut resumed = client.subscribe_events(None, first.id).await.unwrap();
            let second = resumed.next().await.unwrap().unwrap();
            assert_eq!(second.id, Some("1791792000000-2".parse().unwrap()));
            assert_eq!(
                second.event,
                loom_api::ChangeEvent::TaskChanged {
                    id: "k3q7zm2a".parse().unwrap(),
                    context: "personal".parse().unwrap(),
                }
            );
            assert_eq!(resumed.last_event_id(), second.id);
            server.abort();
        });
    }

    async fn write_all_stream(stream: &tokio::net::UnixStream, data: &[u8]) {
        let mut written = 0;
        while written < data.len() {
            stream.writable().await.unwrap();
            match stream.try_write(&data[written..]) {
                Ok(n) => written += n,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("write error: {e}"),
            }
        }
    }

    #[test]
    fn subscribe_events_heartbeat_timeout_triggers_error() {
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("loomd.sock");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let listener = UnixListener::bind(&socket).unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                stream.readable().await.unwrap();
                let mut buf = [0u8; 1024];
                let _ = stream.try_read(&mut buf);
                write_all_stream(
                    &stream,
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n",
                )
                .await;
                tokio::time::sleep(Duration::from_millis(150)).await;
            });

            let client = Client::new(socket.clone());
            let stream = client.subscribe_events(None, None).await.unwrap();
            let mut stream = stream.with_heartbeat_timeout(Duration::from_millis(50));

            let err = stream.next().await.unwrap_err();
            assert!(
                err.to_string().contains("heartbeat timeout"),
                "expected heartbeat timeout, got: {err}"
            );
            server.abort();
        });
    }

    #[test]
    fn subscribe_events_heartbeats_keep_connection_alive() {
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("loomd.sock");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let listener = UnixListener::bind(&socket).unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                stream.readable().await.unwrap();
                let mut buf = [0u8; 1024];
                let _ = stream.try_read(&mut buf);
                write_all_stream(
                    &stream,
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n",
                )
                .await;
                for _ in 0..3 {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    write_all_stream(&stream, b":heartbeat\n\n").await;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
                write_all_stream(&stream, b"data: {\"type\":\"resync\"}\n\n").await;
            });

            let client = Client::new(socket.clone());
            let stream = client.subscribe_events(None, None).await.unwrap();
            let mut stream = stream.with_heartbeat_timeout(Duration::from_millis(80));

            let event = stream.next().await.unwrap().expect("event received");
            assert_eq!(event.event, loom_api::ChangeEvent::Resync);
            server.abort();
        });
    }
}
