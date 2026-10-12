//! HTTP transport of the OTLP exporter: a blocking `ureq` agent with the TLS configuration of
//! `loom-update` ([`loom_update::tls::tls_config`]).
//!
//! The SDK's batch span processor exports from its own OS thread and blocks on each export,
//! so a blocking client needs no async runtime.

use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http::{Request, Response};
use loom_update::tls::tls_config;
use opentelemetry_http::{HttpClient, HttpError};

/// Largest response body read from the collector. OTLP responses are a few bytes.
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// OTLP/HTTP client over `ureq`. Created only when telemetry is enabled.
#[derive(Debug)]
pub struct UreqClient {
    agent: ureq::Agent,
}

impl UreqClient {
    /// A client whose requests, response body included, fail after `timeout`.
    pub fn new(timeout: Duration) -> Self {
        let config = ureq::Agent::config_builder()
            // Same TLS as the updater: OS trust store, ring provider.
            .tls_config(tls_config())
            .timeout_global(Some(timeout))
            // The exporter reads the status itself, to retry or report it.
            .http_status_as_error(false)
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

#[async_trait]
impl HttpClient for UreqClient {
    async fn send_bytes(&self, request: Request<Bytes>) -> Result<Response<Bytes>, HttpError> {
        let (parts, body) = request.into_parts();
        let response = self.agent.run(Request::from_parts(parts, body.as_ref()))?;
        let (parts, mut body) = response.into_parts();
        let bytes = body.with_config().limit(MAX_RESPONSE_BYTES).read_to_vec()?;
        Ok(Response::from_parts(parts, Bytes::from(bytes)))
    }
}
