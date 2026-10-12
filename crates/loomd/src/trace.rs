//! One span per API request, child of the client's span when the request carries a W3C
//! `traceparent` header.

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use loom_telemetry::propagation::set_parent_from_headers;
use tracing::field::Empty;
use tracing::{Instrument as _, info_span};

/// Axum middleware wrapping each request in a server span named `<METHOD> <route>`.
///
/// Attributes: `http.request.method`, `url.path`, `http.route` and
/// `http.response.status_code`. Add it with `Router::layer` so the matched route is known.
pub async fn request_span(request: Request, next: Next) -> Response {
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned());
    let span = info_span!(
        parent: None,
        "http.server",
        otel.name = %span_name(&method, route.as_deref()),
        otel.kind = "server",
        otel.status_code = Empty,
        http.request.method = %method,
        url.path = %path,
        http.route = route.as_deref(),
        http.response.status_code = Empty,
    );
    set_parent_from_headers(&span, request.headers());
    let response = next.run(request).instrument(span.clone()).await;
    let status = response.status();
    span.record("http.response.status_code", i64::from(status.as_u16()));
    // A 4xx is the client's error, not the server's (HTTP semantic conventions).
    if status.is_server_error() {
        span.record("otel.status_code", "ERROR");
    }
    response
}

/// `<METHOD> <route>`, or the method alone when no route matched: an unmatched path would make
/// one span name per URL.
#[must_use]
pub fn span_name(method: &str, route: Option<&str>) -> String {
    route.map_or_else(|| method.to_owned(), |route| format!("{method} {route}"))
}

#[cfg(test)]
mod tests {
    use super::span_name;

    #[test]
    fn span_name_uses_the_route_not_the_path() {
        assert_eq!(span_name("GET", Some("/v1/status")), "GET /v1/status");
        assert_eq!(span_name("GET", None), "GET");
    }
}
