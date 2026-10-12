//! W3C trace context (`traceparent`, `tracestate`) between `loom` and `loomd`.
//!
//! The propagator is used directly rather than through the global one, so nothing is
//! registered globally. Without an OpenTelemetry layer, spans have no trace context: nothing
//! is injected, and an extracted context is ignored.

use http::HeaderMap;
use opentelemetry::propagation::TextMapPropagator as _;
use opentelemetry::trace::TraceContextExt as _;
use opentelemetry_http::{HeaderExtractor, HeaderInjector};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

/// Writes the trace context of `span` into `headers`, so the server's span becomes its child.
pub fn inject_context(span: &Span, headers: &mut HeaderMap) {
    let context = span.context();
    if context.span().span_context().is_valid() {
        TraceContextPropagator::new().inject_context(&context, &mut HeaderInjector(headers));
    }
}

/// Makes the trace context found in `headers`, if any, the parent of `span`. Call it before
/// entering `span`.
pub fn set_parent_from_headers(span: &Span, headers: &HeaderMap) {
    let context = TraceContextPropagator::new().extract(&HeaderExtractor(headers));
    if context.span().span_context().is_valid() {
        // Fails only when `span` was already started or has no OpenTelemetry data: it then
        // keeps its current parent.
        let _ = span.set_parent(context);
    }
}
