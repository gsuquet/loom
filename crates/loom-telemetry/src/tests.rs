//! Spans exported through [`otel_layer`] to an in-memory exporter.

use http::HeaderMap;
use opentelemetry::trace::{SpanKind, Status};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use tracing::level_filters::LevelFilter;
use tracing::{debug_span, info_span};
use tracing_subscriber::layer::SubscriberExt as _;

use crate::otel_layer;
use crate::propagation::{inject_context, set_parent_from_headers};

/// Runs `body` with a subscriber exporting to memory, and returns the finished spans.
fn spans(level: LevelFilter, body: impl FnOnce()) -> Vec<SpanData> {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry().with(otel_layer(&provider, "test", level));
    tracing::subscriber::with_default(subscriber, body);
    provider.force_flush().unwrap();
    exporter.get_finished_spans().unwrap()
}

fn named<'a>(spans: &'a [SpanData], name: &str) -> &'a SpanData {
    spans
        .iter()
        .find(|span| span.name == name)
        .unwrap_or_else(|| panic!("no span {name} in {:?}", names(spans)))
}

fn names(spans: &[SpanData]) -> Vec<&str> {
    spans.iter().map(|span| span.name.as_ref()).collect()
}

#[test]
fn nested_spans_keep_their_parent_and_take_otel_name() {
    let spans = spans(LevelFilter::INFO, || {
        let root = info_span!("command", otel.name = "loom status");
        root.in_scope(|| {
            let _child = info_span!(
                "http.client",
                otel.name = "GET /v1/status",
                otel.kind = "client"
            )
            .entered();
        });
    });
    let root = named(&spans, "loom status");
    let child = named(&spans, "GET /v1/status");
    assert_eq!(child.parent_span_id, root.span_context.span_id());
    assert_eq!(child.span_context.trace_id(), root.span_context.trace_id());
    assert_eq!(child.span_kind, SpanKind::Client);
    assert_eq!(root.parent_span_id, opentelemetry::trace::SpanId::INVALID);
}

#[test]
fn error_status_is_recorded() {
    let spans = spans(LevelFilter::INFO, || {
        let span = info_span!(
            "command",
            otel.name = "loom status",
            otel.status_code = tracing::field::Empty,
            otel.status_description = tracing::field::Empty
        );
        span.record("otel.status_code", "ERROR");
        span.record("otel.status_description", "loomd is not running");
    });
    assert_eq!(
        named(&spans, "loom status").status,
        Status::error("loomd is not running")
    );
}

#[test]
fn debug_spans_are_exported_with_verbose_only() {
    let body = || {
        let _info = info_span!("kept").entered();
        let _debug = debug_span!("detail").entered();
    };
    assert_eq!(names(&spans(LevelFilter::INFO, body)), ["kept"]);
    // `-q` does not hide spans from the trace.
    assert_eq!(names(&spans(LevelFilter::ERROR, body)), ["kept"]);
    let mut verbose = names(&spans(LevelFilter::DEBUG, body))
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    verbose.sort();
    assert_eq!(verbose, ["detail", "kept"]);
}

#[test]
fn opentelemetry_diagnostics_are_not_exported() {
    let spans = spans(LevelFilter::TRACE, || {
        let _own = info_span!(target: "opentelemetry_sdk", "export").entered();
    });
    assert!(spans.is_empty(), "{:?}", names(&spans));
}

#[test]
fn trace_context_crosses_headers() {
    let spans = spans(LevelFilter::INFO, || {
        let mut headers = HeaderMap::new();
        let client = info_span!("http.client", otel.name = "client");
        inject_context(&client, &mut headers);
        assert!(headers.contains_key("traceparent"), "{headers:?}");
        drop(client);

        // A root span, as in the daemon: no current span.
        let server = info_span!(parent: None, "http.server", otel.name = "server");
        set_parent_from_headers(&server, &headers);
        drop(server);
    });
    let client = named(&spans, "client");
    let server = named(&spans, "server");
    assert_eq!(server.parent_span_id, client.span_context.span_id());
    assert_eq!(
        server.span_context.trace_id(),
        client.span_context.trace_id()
    );
    assert!(server.parent_span_is_remote);
}

#[test]
fn no_header_leaves_the_server_span_a_root() {
    let spans = spans(LevelFilter::INFO, || {
        let server = info_span!(parent: None, "http.server", otel.name = "server");
        set_parent_from_headers(&server, &HeaderMap::new());
        drop(server);
    });
    assert_eq!(
        named(&spans, "server").parent_span_id,
        opentelemetry::trace::SpanId::INVALID
    );
}

#[test]
fn nothing_is_injected_without_an_otel_layer() {
    let subscriber = tracing_subscriber::registry();
    tracing::subscriber::with_default(subscriber, || {
        let mut headers = HeaderMap::new();
        inject_context(&info_span!("client"), &mut headers);
        assert!(headers.is_empty(), "{headers:?}");
    });
}
