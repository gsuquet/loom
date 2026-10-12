# Architecture

> Processes, interfaces, language, crate layout and how loom talks to git.

## Processes

| Process | Role |
| --- | --- |
| `loomd` | Daemon, single writer of the store. Polls sources, syncs, schedules and supervises agent runs, applies upgrades. Runs as a launchd user agent (macOS) or systemd user unit (Linux) unless disabled. |
| `loom` | CLI and TUI. A client of `loomd`; starts it if it is not running. |
| `loom mcp` | MCP server (stdio) for agents, a client of `loomd`. |

## Interfaces

- **loomd API**: HTTP + JSON with SSE streams, on a unix socket locally, TCP + auth for the future central instance. CLI, TUI, MCP server and web UI are all clients of this one API.
  - HTTP/1.1 on the unix socket: HTTP/2 brings nothing locally (no handshake or connection cost) and would need prior-knowledge h2c without TLS; every client, `curl --unix-socket` included, speaks HTTP/1.1.
  - HTTP/2 negotiated through TLS (ALPN) on the central instance: browsers allow 6 HTTP/1.1 connections per host and each SSE stream holds one, so the web UI needs HTTP/2 there.
- **CLI**: every command supports `--json`, with a versioned output schema. Every TUI action has a CLI equivalent.
- **TUI** (ratatui): the cockpit. See [User workflow](user-workflow.md).
- **Web UI**: served by loomd once the central instance exists; works locally too. A desktop app would be the same web UI in a shell, only if needed.
- **MCP server**: the only MCP server agents see. See [Agent orchestration](agents.md).
- **Files**: readable by anyone and any tool. Writes go through loomd. Hand edits of `task.md` are detected and indexed.

## Language: Rust

See [ADR-0001](../adr/0001-rust.md). In short: the domain is made of closed sets (kinds, statuses, events, proposed actions, autonomy classes, runtime capabilities) that map to Rust enums with exhaustive matching, and one set of types generates the CLI JSON output, MCP tool schemas and agent contracts (`serde` + `schemars`).

Known costs:

- No official Anthropic Rust SDK: the direct-API runtime uses a thin typed client (`reqwest` + `serde`).
- Jira, GitLab and GitHub: thin typed clients for the endpoints loom uses, rather than broad community crates.
- Datadog: official `datadog-api-client-rust`. MCP: official `rmcp`. Bedrock: AWS SDK for Rust.

## Crate layout

Crates are added when a version needs them, never ahead of time.

| Crate | Kind | Introduced | Content |
| --- | --- | --- | --- |
| `loom` | bin | v0.1 | CLI (clap), later the TUI |
| `loomd` | bin | v0.1 | Daemon: API server, scheduler, updater |
| `loom-api` | lib | v0.1 | Types shared by the API, the CLI `--json` output and the MCP tools. No I/O. |
| `loom-config` | lib | v0.1 | Loom home resolution, files under it, `config.toml` parsing. No I/O. Shared by `loom` and `loomd` |
| `loom-update` | lib | v0.1 | Release discovery, update decision (pure), download, verification, install, rollback. Shared by `loom` and `loomd` |
| `loom-telemetry` | lib | v0.1 | `tracing` subscriber of both binaries: logs on stderr, OTLP/HTTP trace export when enabled (decisions pure), W3C trace context propagation. Shared by `loom` and `loomd` |
| `loom-repo` | lib | v0.2 | Repo identity, scanning, state, update policy |
| `loom-core` | lib | v0.3 | Domain: tasks, events, notes, priority, contexts. No I/O. |
| `loom-store` | lib | v0.3 | `StorePort` and the local files + git adapter, SQLite index |

Rules:

- Domain and API crates have no I/O and no async runtime. Decision logic is written as pure functions, tested without filesystem or network.
- Binaries own I/O, configuration and wiring.

## Git access

See [ADR-0008](../adr/0008-git-cli-for-writes.md).

- Writes (fetch, `merge --ff-only`, worktree, checkout of MR refs) go through the `git` CLI, so they honour the user's git config, credential helpers and hooks.
- Read-only inspection at scale (repo index, state) uses `gix`.

## Observability

`tracing` everywhere, OTLP export configurable and off by default. otel is for diagnosing loom itself; the work ledger is `events.jsonl` (see [Capture and learnings](capture-and-learnings.md)).

- `loom-telemetry` builds the subscriber of both binaries. Disabled means no exporter, HTTP client or thread.
- Export: OTLP over HTTP/protobuf, batch span processor, over the same blocking HTTP client and TLS configuration as the updater (`ureq` + rustls with ring and the OS trust store, `loom_update::tls`); no gRPC.
- One trace per CLI command: `loom` sends the W3C trace context with each API request, and `loomd`'s request span is its child.
- `loom` waits at most 2 s at exit for its spans to be exported.
