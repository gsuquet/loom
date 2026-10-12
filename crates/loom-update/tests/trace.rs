//! `decide` is traced as the span `update.decide`.

use loom_update::{Channel, UpdateDecision, UpdateMode, UpdatePolicy, decide};
use opentelemetry::Value;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use semver::Version;
use tracing_subscriber::layer::SubscriberExt as _;

#[test]
fn decide_is_a_span_with_its_inputs() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let decision = tracing::subscriber::with_default(subscriber, || {
        decide(
            &Version::new(0, 1, 0),
            Channel::Stable,
            &[Version::new(0, 2, 0)],
            &UpdatePolicy {
                mode: UpdateMode::Notify,
                pinned: None,
            },
            &[],
        )
    });
    assert_eq!(
        decision,
        UpdateDecision::Available {
            version: Version::new(0, 2, 0)
        }
    );

    provider.force_flush().unwrap();
    let spans = exporter.get_finished_spans().unwrap();
    let span = spans
        .iter()
        .find(|span| span.name == "update.decide")
        .unwrap_or_else(|| panic!("no update.decide span in {spans:#?}"));
    let attribute = |key: &str| {
        span.attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.clone())
    };
    assert_eq!(attribute("current"), Some(Value::from("0.1.0")));
    assert_eq!(attribute("channel"), Some(Value::from("Stable")));
    assert_eq!(attribute("mode"), Some(Value::from("Notify")));
}
