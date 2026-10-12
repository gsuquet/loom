# See loom's traces in a local backend

Export the traces of `loom` and `loomd` to a trace backend on your machine, to see what a command did and how long each step took. Keys, variables and spans: [`[telemetry]` reference](../reference/configuration.md#telemetry).

Requirements: a loom checkout with [mise](https://mise.jdx.dev) set up (`mise install`), or any backend that accepts OTLP over HTTP on port 4318.

## Start the backend

From the loom checkout:

```bash
mise run otel-dev
```

The task downloads [Jaeger](https://www.jaegertracing.io/) 2 on first use and runs it in the foreground: it receives OTLP/HTTP on `localhost:4318`, keeps traces in memory, and serves its UI on <http://localhost:16686>. Leave it running in its own terminal; Ctrl-C stops it and discards the traces.

## Enable telemetry

Add this to `~/.loom/config.toml` (or `$LOOM_HOME/config.toml`):

```toml
[telemetry]
enabled = true
```

Traces go to `http://localhost:4318/v1/traces` by default. For another backend, set `otlp_endpoint`, or `OTEL_EXPORTER_OTLP_ENDPOINT` in the environment.

`loom` reads the file at each command; `loomd` reads it only when it starts. Stop a running `loomd` so that the next `loom status` starts it with the new configuration:

```bash
kill "$(cat ~/.loom/run/loomd.lock)"
```

## Produce and find a trace

```bash
loom status
```

In the Jaeger UI:

1. Choose the service `loom` and the operation `loom status`, then **Find Traces**.
2. Open the latest trace. Under the `loom status` span, each `GET /v1/status` span of `loom` is a request to `loomd`; the one that got an answer has a child `GET /v1/status` span from the service `loomd`.

When `loom status` started `loomd`, the trace also shows the requests that failed while `loomd` was starting. `loomd` sends its spans every 5 s: if its span is missing, wait a few seconds and refresh.

## Disable telemetry

Set `enabled = false` (or remove the `[telemetry]` table), then stop `loomd` as above so that it restarts without telemetry.

To turn it off for one command without editing the file:

```bash
OTEL_SDK_DISABLED=true loom status
```

A `loomd` started by that command inherits the variable and exports nothing either.

## Troubleshoot

- **`WARN traces are not exported: invalid configuration ...`**: fix the file at the line and column given; the command itself ran.
- **`ERROR name="BatchSpanProcessor.ExportError"` or `cannot export traces`**: the backend is not reachable at the endpoint. `loom` waits at most 2 s for it and still exits normally. For a `loomd` started by `loom status`, these messages are in `~/.loom/log/loomd.log`.
