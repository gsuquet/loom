//! Diagnostics of `loom` and `loomd`: logs on stderr and, when enabled, traces exported over
//! OTLP/HTTP.
//!
//! [`init`] installs the global `tracing` subscriber of a binary. Decisions (log level, colors,
//! whether and where to export) are pure functions in [`logging`] and [`settings`]; the rest
//! builds the subscriber, the exporter and its HTTP client.
//!
//! otel is for diagnosing loom itself, not a record of the work (that is `events.jsonl`).

pub mod logging;
pub mod propagation;
pub mod settings;
mod transport;

use std::cmp;
use std::io;
use std::time::Duration;

use loom_api::VersionInfo;
use opentelemetry::KeyValue;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{Protocol, WithExportConfig as _, WithHttpConfig as _};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing::level_filters::LevelFilter;
use tracing::{Subscriber, debug, warn};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::settings::{EnvLookup, Telemetry, default_service_name_applies, export_timeout};
use crate::transport::UreqClient;

/// Longest `loom` waits, at exit, for its spans to be exported: a dead or slow collector delays
/// a command by at most this.
pub const LOOM_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
/// Longest `loomd` waits, at exit, for its spans to be exported.
pub const LOOMD_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// Resource attribute holding the release channel of the build.
pub const CHANNEL_ATTRIBUTE: &str = "loom.channel";
/// Prefix of the targets of the OpenTelemetry crates' own diagnostics. Never exported as
/// spans, so that a failing export cannot feed itself.
const OTEL_TARGET_PREFIX: &str = "opentelemetry";

/// How a binary sets up its diagnostics.
#[derive(Debug, Clone)]
pub struct Options<'a> {
    /// `service.name` unless the user sets one (`OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`).
    pub service_name: &'static str,
    /// Build information: `service.version` and `loom.channel`.
    pub version: &'a VersionInfo,
    /// Most verbose level logged on stderr.
    pub level: LevelFilter,
    /// Whether stderr is colored.
    pub color: bool,
    /// Whether log lines start with a timestamp and the event's target (`loomd`).
    pub timestamps: bool,
    /// Whether and where to export traces.
    pub telemetry: &'a Telemetry,
    /// Longest wait for pending spans at exit.
    pub shutdown_timeout: Duration,
}

/// Installs the global subscriber: logs on stderr and, when `options.telemetry` is enabled,
/// a batch exporter of spans. When it is disabled, no exporter, HTTP client or thread is
/// created.
///
/// Problems are reported, never fatal: a subscriber that cannot be installed is reported on
/// stderr, an exporter that cannot be built is logged as a warning and traces are not exported.
pub fn init(options: &Options<'_>, env: EnvLookup<'_>) -> TelemetryGuard {
    let (provider, build_error) = match options.telemetry {
        Telemetry::Disabled(_) => (None, None),
        Telemetry::Enabled { endpoint } => match tracer_provider(options, endpoint, env) {
            Ok(provider) => (Some(provider), None),
            Err(error) => (None, Some(error)),
        },
    };

    let level = options.level;
    // Events only: spans exist for the trace, and log lines keep the same layout either way.
    let events = filter_fn(move |metadata| metadata.is_event() && *metadata.level() <= level);
    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(io::stderr)
        .with_ansi(options.color);
    let stderr = if options.timestamps {
        stderr.with_filter(events).boxed()
    } else {
        stderr
            .without_time()
            .with_target(false)
            .with_filter(events)
            .boxed()
    };
    let traces = provider
        .as_ref()
        .map(|provider| otel_layer(provider, options.service_name, level));

    let result = tracing_subscriber::registry()
        .with(stderr)
        .with(traces)
        .try_init();
    if let Err(error) = result {
        eprintln!("Error: cannot set up logging: {error}");
    }
    if let Some(error) = build_error {
        warn!("traces are not exported: cannot set up the OTLP exporter: {error}");
    }
    debug!(telemetry = ?options.telemetry, exporting = provider.is_some(), "telemetry");
    TelemetryGuard {
        provider,
        timeout: options.shutdown_timeout,
    }
}

/// `tracing` layer turning spans into OpenTelemetry spans of `provider`.
///
/// The spans belong to the instrumentation scope `scope`. Spans and events at `info` or above
/// are exported, more with `-v`. The OpenTelemetry crates' own diagnostics are not.
#[must_use]
pub fn otel_layer<S>(
    provider: &SdkTracerProvider,
    scope: &'static str,
    level: LevelFilter,
) -> impl Layer<S> + use<S>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    let level = cmp::max(level, LevelFilter::INFO);
    tracing_opentelemetry::layer()
        .with_tracer(provider.tracer(scope))
        .with_filter(filter_fn(move |metadata| {
            *metadata.level() <= level && !metadata.target().starts_with(OTEL_TARGET_PREFIX)
        }))
}

/// Exporter, batch processor and resource for `endpoint`.
fn tracer_provider(
    options: &Options<'_>,
    endpoint: &str,
    env: EnvLookup<'_>,
) -> Result<SdkTracerProvider, opentelemetry_otlp::ExporterBuildError> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        // The only protocol built in: `OTEL_EXPORTER_OTLP_PROTOCOL` is ignored.
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(endpoint)
        .with_http_client(UreqClient::new(export_timeout(env)))
        .build()?;
    // The default detectors read `OTEL_SERVICE_NAME` and `OTEL_RESOURCE_ATTRIBUTES`.
    let mut resource = Resource::builder();
    if default_service_name_applies(env) {
        resource = resource.with_service_name(options.service_name);
    }
    let resource = resource
        .with_attributes([
            KeyValue::new("service.version", options.version.version.clone()),
            KeyValue::new(CHANNEL_ATTRIBUTE, options.version.channel.as_str()),
        ])
        .build();
    Ok(SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build())
}

/// Flushes pending spans and stops the exporter when shut down or dropped, waiting at most
/// the shutdown timeout. Does nothing when traces are not exported.
#[derive(Debug)]
#[must_use = "dropping the guard stops the export of traces"]
pub struct TelemetryGuard {
    provider: Option<SdkTracerProvider>,
    timeout: Duration,
}

impl TelemetryGuard {
    /// Whether traces are exported.
    #[must_use]
    pub const fn is_exporting(&self) -> bool {
        self.provider.is_some()
    }

    /// Exports pending spans and stops the exporter, waiting at most the shutdown timeout.
    /// A failure is logged as a warning.
    pub fn shutdown(mut self) {
        self.finish();
    }

    fn finish(&mut self) {
        if let Some(provider) = self.provider.take()
            && let Err(error) = provider.shutdown_with_timeout(self.timeout)
        {
            warn!("cannot export traces: {error}");
        }
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests;
