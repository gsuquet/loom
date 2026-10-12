//! Client of the loomd API: HTTP/1.1 over the unix socket, one connection per request.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use http_body_util::{BodyExt as _, Empty};
use hyper::body::Bytes;
use hyper::client::conn::http1;
use hyper::{Request, header};
use hyper_util::rt::TokioIo;
use loom_api::{ApiError, StatusResponse};
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::net::UnixStream;

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

    /// Path of the socket.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// `GET /v1/status`.
    pub async fn status(&self) -> Result<StatusResponse, ClientError> {
        self.get(STATUS_PATH).await
    }

    /// `GET path`, with the JSON body of a successful response decoded as `T`.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ClientError> {
        let (status, body) = tokio::time::timeout(self.timeout, self.exchange(path))
            .await
            .map_err(|_| {
                ClientError::Protocol(format!(
                    "no answer to GET {path} within {} s",
                    self.timeout.as_secs()
                ))
            })??;
        if status.is_success() {
            return serde_json::from_slice(&body).map_err(|error| {
                ClientError::Protocol(format!("invalid body for GET {path}: {error}"))
            });
        }
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
        Err(error)
    }

    /// Connects, sends `GET path` and reads the whole response.
    async fn exchange(&self, path: &str) -> Result<(hyper::StatusCode, Bytes), ClientError> {
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

        let request = Request::get(path)
            .header(header::HOST, HOST)
            .body(Empty::<Bytes>::new())
            .map_err(|error| protocol("invalid request", &error))?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|error| protocol("request failed", &error))?;
        let status = response.status();
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
