//! Client of the loomd API: HTTP/1.1 over the unix socket, one connection per request.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use http_body_util::{BodyExt as _, Full};
use hyper::body::Bytes;
use hyper::client::conn::http1;
use hyper::{Method, Request, header};
use hyper_util::rt::TokioIo;
use loom_api::{ApiError, StatusResponse};
use loom_telemetry::propagation::inject_context;
use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::net::UnixStream;
use tracing::field;
use tracing::{Instrument as _, Span, info_span};

/// Longest a request may take, connection included.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Path of the status endpoint.
const STATUS_PATH: &str = "/v1/status";
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
    pub const fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            timeout: REQUEST_TIMEOUT,
        }
    }

    /// The same client, each request taking at most `timeout`, connection included.
    pub fn with_timeout(&self, timeout: Duration) -> Self {
        Self {
            socket: self.socket.clone(),
            timeout,
        }
    }

    /// Path of the socket.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// `GET /v1/status`.
    pub async fn status(&self) -> Result<StatusResponse, ClientError> {
        self.get(STATUS_PATH).await
    }

    /// `GET path`, with the JSON body of a successful response decoded as `T`. Traced as a
    /// client span whose context is sent to loomd.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ClientError> {
        self.call(Method::GET, path, None).await
    }

    /// `POST path` with `body` as JSON (an empty body when `None`).
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

    /// `DELETE path`.
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
}
