# See loom's traces in a local backend

Export the traces of `loom` and `loomd` to a trace backend on your machine. The traces show what a command did and the duration of each step. For the keys, variables and spans, see the [`[telemetry]` reference](../reference/configuration.md#telemetry).

You need one of these:

- A loom checkout with [mise](https://mise.jdx.dev) configured (`mise install`).
- A backend that accepts OTLP over HTTP on port 4318.

## Start the backend

In the loom checkout, run this command:

```bash
mise run otel-dev
```

The task downloads [Jaeger](https://www.jaegertracing.io/) 2 the first time that you use it. It runs Jaeger in the foreground. Jaeger receives OTLP/HTTP on `localhost:4318` and keeps the traces in memory. It serves its UI on <http://localhost:16686>. Let Jaeger run in its own terminal. Ctrl-C stops Jaeger and deletes the traces.

## Enable telemetry

1. Add these lines to `~/.loom/config.toml` (or `$LOOM_HOME/config.toml`):

   ```toml
   [telemetry]
   enabled = true
   ```

   By default, the traces go to `http://localhost:4318/v1/traces`. For a different backend, set `otlp_endpoint`, or `OTEL_EXPORTER_OTLP_ENDPOINT` in the environment.

2. If `loomd` runs, stop it:

   ```bash
   kill "$(cat ~/.loom/run/loomd.lock)"
   ```

   `loom` reads the file at each command. `loomd` reads it only when it starts. The next `loom status` starts `loomd` with the new configuration.

## Produce and find a trace

1. Run a command:

   ```bash
   loom status
   ```

2. In the Jaeger UI, choose the service `loom` and the operation `loom status`. Then click **Find Traces**.
3. Open the latest trace. Under the `loom status` span, each `GET /v1/status` span of `loom` is a request to `loomd`. The request that got an answer has a child `GET /v1/status` span from the service `loomd`.

If `loom status` started `loomd`, the trace also shows the requests that failed during the start of `loomd`. `loomd` sends its spans every 5 s. If its span is not there, wait a few seconds. Then refresh the page.

## Disable telemetry

1. Set `enabled = false`, or remove the `[telemetry]` table.
2. Stop `loomd` as above. Then `loomd` restarts without telemetry.

To disable telemetry for one command and keep the file as it is, set `OTEL_SDK_DISABLED`:

```bash
OTEL_SDK_DISABLED=true loom status
```

If that command starts `loomd`, `loomd` also gets the variable and exports nothing.

## Troubleshoot

- **`WARN traces are not exported: invalid configuration ...`**: correct the file at the given line and column. The command itself ran.
- **`ERROR name="BatchSpanProcessor.ExportError"` or `cannot export traces`**: the backend is not reachable at the endpoint. `loom` waits a maximum of 2 s for the backend. Then it exits normally. If `loom status` started `loomd`, these messages are in `~/.loom/log/loomd.log`.
