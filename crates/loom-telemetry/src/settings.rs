//! Whether to export traces, and where: pure decisions over the configuration and the
//! environment. The environment is injected as a lookup function.

use std::time::Duration;

/// `true` (any case) disables the OpenTelemetry SDK, whatever the configuration.
pub const OTEL_SDK_DISABLED: &str = "OTEL_SDK_DISABLED";
/// Full URL of the traces endpoint, used as given.
pub const OTEL_EXPORTER_OTLP_TRACES_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT";
/// Base URL of every signal; `/v1/traces` is appended for traces.
pub const OTEL_EXPORTER_OTLP_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";
/// Timeout of one export to the traces endpoint, in milliseconds.
pub const OTEL_EXPORTER_OTLP_TRACES_TIMEOUT: &str = "OTEL_EXPORTER_OTLP_TRACES_TIMEOUT";
/// Timeout of one export, in milliseconds, when the traces-specific one is unset.
pub const OTEL_EXPORTER_OTLP_TIMEOUT: &str = "OTEL_EXPORTER_OTLP_TIMEOUT";
/// Service name; overrides `service.name` in `OTEL_RESOURCE_ATTRIBUTES`.
pub const OTEL_SERVICE_NAME: &str = "OTEL_SERVICE_NAME";
/// Resource attributes, `key=value` pairs separated by commas.
pub const OTEL_RESOURCE_ATTRIBUTES: &str = "OTEL_RESOURCE_ATTRIBUTES";

/// Traces endpoint when neither the configuration nor the environment gives one.
pub const DEFAULT_TRACES_ENDPOINT: &str = "http://localhost:4318/v1/traces";
/// Path appended to `OTEL_EXPORTER_OTLP_ENDPOINT` for traces.
const TRACES_PATH: &str = "v1/traces";
/// Export timeout when no variable sets one (OTLP exporter specification).
pub const DEFAULT_EXPORT_TIMEOUT: Duration = Duration::from_secs(10);

/// Looks up an environment variable; `None` when unset.
pub type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Outcome of [`resolve`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Telemetry {
    /// No exporter, no HTTP client, no thread: nothing leaves the process.
    Disabled(DisabledBy),
    /// Traces are exported over OTLP/HTTP (protobuf) to `endpoint`.
    Enabled {
        /// Full URL of the traces endpoint.
        endpoint: String,
    },
}

/// Why telemetry is off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisabledBy {
    /// `[telemetry] enabled` is `false` or absent.
    Config,
    /// `OTEL_SDK_DISABLED=true`.
    SdkDisabledEnv,
}

/// Decides whether to export traces and where.
///
/// On only when `enabled` and `OTEL_SDK_DISABLED` is not `true`. Endpoint, first found:
/// `config_endpoint`, `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` as given,
/// `OTEL_EXPORTER_OTLP_ENDPOINT` followed by `/v1/traces`, [`DEFAULT_TRACES_ENDPOINT`].
/// Empty variables count as unset.
#[must_use]
pub fn resolve(enabled: bool, config_endpoint: Option<&str>, env: EnvLookup<'_>) -> Telemetry {
    if !enabled {
        return Telemetry::Disabled(DisabledBy::Config);
    }
    if non_empty(env, OTEL_SDK_DISABLED).is_some_and(|value| value.eq_ignore_ascii_case("true")) {
        return Telemetry::Disabled(DisabledBy::SdkDisabledEnv);
    }
    let endpoint = config_endpoint
        .map(str::to_owned)
        .or_else(|| non_empty(env, OTEL_EXPORTER_OTLP_TRACES_ENDPOINT))
        .or_else(|| non_empty(env, OTEL_EXPORTER_OTLP_ENDPOINT).map(|base| traces_url(&base)))
        .unwrap_or_else(|| DEFAULT_TRACES_ENDPOINT.to_owned());
    Telemetry::Enabled { endpoint }
}

/// `base` followed by `/v1/traces`, without doubling a trailing slash.
fn traces_url(base: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{TRACES_PATH}")
    } else {
        format!("{base}/{TRACES_PATH}")
    }
}

/// Whether the binary's own service name applies: the user did not set one through
/// `OTEL_SERVICE_NAME` or `service.name` in `OTEL_RESOURCE_ATTRIBUTES`.
#[must_use]
pub fn default_service_name_applies(env: EnvLookup<'_>) -> bool {
    if non_empty(env, OTEL_SERVICE_NAME).is_some() {
        return false;
    }
    !non_empty(env, OTEL_RESOURCE_ATTRIBUTES).is_some_and(|attributes| {
        attributes
            .split(',')
            .filter_map(|pair| pair.split_once('='))
            .any(|(key, _)| key.trim() == "service.name")
    })
}

/// Timeout of one export request.
///
/// `OTEL_EXPORTER_OTLP_TRACES_TIMEOUT`, else `OTEL_EXPORTER_OTLP_TIMEOUT` (milliseconds), else
/// [`DEFAULT_EXPORT_TIMEOUT`]. A value that is not a whole number of milliseconds is ignored,
/// as the OTLP exporter does.
#[must_use]
pub fn export_timeout(env: EnvLookup<'_>) -> Duration {
    [
        OTEL_EXPORTER_OTLP_TRACES_TIMEOUT,
        OTEL_EXPORTER_OTLP_TIMEOUT,
    ]
    .into_iter()
    .find_map(|name| env(name).and_then(|value| value.trim().parse::<u64>().ok()))
    .map_or(DEFAULT_EXPORT_TIMEOUT, Duration::from_millis)
}

/// The trimmed value of `name`, `None` when unset or blank.
fn non_empty(env: EnvLookup<'_>, name: &str) -> Option<String> {
    env(name)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_EXPORT_TIMEOUT, DEFAULT_TRACES_ENDPOINT, DisabledBy, Telemetry,
        default_service_name_applies, export_timeout, resolve,
    };
    use std::collections::HashMap;
    use std::time::Duration;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    fn enabled(endpoint: &str) -> Telemetry {
        Telemetry::Enabled {
            endpoint: endpoint.to_owned(),
        }
    }

    const CONFIG: &str = "http://config:4318/v1/traces";
    const TRACES: (&str, &str) = (
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        "http://traces:4318/custom",
    );
    const BASE: (&str, &str) = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://base:4318");

    /// `enabled`, `otlp_endpoint`, environment, expected outcome.
    type Case = (
        bool,
        Option<&'static str>,
        Vec<(&'static str, &'static str)>,
        Telemetry,
    );

    #[test]
    fn precedence_table() {
        let off = Telemetry::Disabled(DisabledBy::Config);
        let sdk_off = Telemetry::Disabled(DisabledBy::SdkDisabledEnv);
        let cases: Vec<Case> = vec![
            // Off unless the configuration enables it, whatever the environment.
            (false, None, vec![], off.clone()),
            (false, Some(CONFIG), vec![TRACES, BASE], off),
            // OTEL_SDK_DISABLED=true wins over the configuration, in any case.
            (
                true,
                Some(CONFIG),
                vec![("OTEL_SDK_DISABLED", "true")],
                sdk_off.clone(),
            ),
            (true, None, vec![("OTEL_SDK_DISABLED", "TRUE")], sdk_off),
            // Any other value of OTEL_SDK_DISABLED leaves it on.
            (
                true,
                None,
                vec![("OTEL_SDK_DISABLED", "false")],
                enabled(DEFAULT_TRACES_ENDPOINT),
            ),
            (
                true,
                None,
                vec![("OTEL_SDK_DISABLED", "1")],
                enabled(DEFAULT_TRACES_ENDPOINT),
            ),
            (
                true,
                None,
                vec![("OTEL_SDK_DISABLED", "")],
                enabled(DEFAULT_TRACES_ENDPOINT),
            ),
            // Endpoint: configuration, then the traces variable, then the base variable.
            (true, Some(CONFIG), vec![TRACES, BASE], enabled(CONFIG)),
            (
                true,
                None,
                vec![TRACES, BASE],
                enabled("http://traces:4318/custom"),
            ),
            (
                true,
                None,
                vec![BASE],
                enabled("http://base:4318/v1/traces"),
            ),
            (
                true,
                None,
                vec![("OTEL_EXPORTER_OTLP_ENDPOINT", "http://base:4318/")],
                enabled("http://base:4318/v1/traces"),
            ),
            (
                true,
                None,
                vec![("OTEL_EXPORTER_OTLP_ENDPOINT", "https://otel.example/prefix")],
                enabled("https://otel.example/prefix/v1/traces"),
            ),
            (true, None, vec![], enabled(DEFAULT_TRACES_ENDPOINT)),
            // Empty variables count as unset.
            (
                true,
                None,
                vec![("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", " "), BASE],
                enabled("http://base:4318/v1/traces"),
            ),
            (
                true,
                None,
                vec![("OTEL_EXPORTER_OTLP_ENDPOINT", "")],
                enabled(DEFAULT_TRACES_ENDPOINT),
            ),
        ];
        for (is_enabled, config, vars, expected) in cases {
            let lookup = env(&vars);
            assert_eq!(
                resolve(is_enabled, config, &lookup),
                expected,
                "enabled={is_enabled} config={config:?} env={vars:?}"
            );
        }
    }

    #[test]
    fn default_endpoint_is_the_local_otlp_http_port() {
        assert_eq!(DEFAULT_TRACES_ENDPOINT, "http://localhost:4318/v1/traces");
    }

    #[test]
    fn service_name_set_by_the_user_wins() {
        let cases = [
            (vec![], true),
            (vec![("OTEL_SERVICE_NAME", "mine")], false),
            (vec![("OTEL_SERVICE_NAME", "")], true),
            (
                vec![("OTEL_RESOURCE_ATTRIBUTES", "a=1, service.name=mine")],
                false,
            ),
            (
                vec![("OTEL_RESOURCE_ATTRIBUTES", "service.namespace=x,a=1")],
                true,
            ),
        ];
        for (vars, expected) in cases {
            let lookup = env(&vars);
            assert_eq!(default_service_name_applies(&lookup), expected, "{vars:?}");
        }
    }

    #[test]
    fn export_timeout_follows_the_otlp_variables() {
        let cases = [
            (vec![], DEFAULT_EXPORT_TIMEOUT),
            (
                vec![("OTEL_EXPORTER_OTLP_TIMEOUT", "2500")],
                Duration::from_millis(2500),
            ),
            (
                vec![
                    ("OTEL_EXPORTER_OTLP_TRACES_TIMEOUT", "700"),
                    ("OTEL_EXPORTER_OTLP_TIMEOUT", "2500"),
                ],
                Duration::from_millis(700),
            ),
            (
                vec![
                    ("OTEL_EXPORTER_OTLP_TRACES_TIMEOUT", "soon"),
                    ("OTEL_EXPORTER_OTLP_TIMEOUT", "2500"),
                ],
                Duration::from_millis(2500),
            ),
            (
                vec![("OTEL_EXPORTER_OTLP_TIMEOUT", "-1")],
                DEFAULT_EXPORT_TIMEOUT,
            ),
        ];
        for (vars, expected) in cases {
            let lookup = env(&vars);
            assert_eq!(export_timeout(&lookup), expected, "{vars:?}");
        }
    }
}
