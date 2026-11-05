# Architecture

> Processes, interfaces, language, crate layout and how loom talks to git.

## Processes

| Process | Role |
| --- | --- |
| `loomd` | Daemon, single writer of the store. Polls sources, syncs, schedules and supervises agent runs, applies upgrades. Runs as a launchd user agent (macOS) or systemd user unit (Linux) unless disabled. |
| `loom` | CLI, for me, scripts and agents. A client of `loomd`; starts it if it is not running. With no arguments it prints the help; once the desktop app exists, it opens the app. |
| Desktop app | The one window I work in. A client of `loomd`. |
| `loom mcp` | MCP server (stdio) for agents, a client of `loomd`. |

## Interfaces

- **loomd API**: HTTP + JSON with SSE streams, on a unix socket locally, TCP + auth for the future central instance. CLI, desktop app and MCP server are all clients of this one API.
  - HTTP/1.1 on the unix socket: HTTP/2 brings nothing locally (no handshake or connection cost) and would need prior-knowledge h2c without TLS; every client, `curl --unix-socket` included, speaks HTTP/1.1.
- **CLI**: every command supports `--json`, with a versioned output schema. A CLI-only gap is acceptable while the app catches up.
- **Desktop app**: see [Desktop app](#desktop-app) and [User workflow](user-workflow.md).
- **MCP server**: the only MCP server agents see. See [Agent orchestration](agents.md).
- **Files**: readable by anyone and any tool. Writes go through loomd. Hand edits of `task.md` are detected and indexed.

## Desktop app

The TUI is removed, and the CLI is the interface until the app exists. Business logic stays in `loomd` and `loom-core`, and never moves into the app.

- It reaches `loomd` over the unix socket, with no TCP port and no new authentication.
- MR content, transcripts and articles are untrusted. Rendering them must not run their scripts or reach `loomd`.
- The look is native macOS, see [UI style](ui-style.md#desktop-app-target).
- A Linux version is likely later, and the toolkit choice weighs it. The app holds presentation only: every rule stays in `loomd` and the core crates, behind the API, so a second front-end reuses them.

The toolkit is the one open decision here. The candidates are SwiftUI, Rust with AppKit through `objc2`, and Tauri. Docs that depend on the toolkit are written once it is chosen.

## Language: Rust

See [ADR-0001](../adr/0001-rust.md). In short: the domain is made of closed sets (kinds, statuses, events, proposed actions, autonomy classes, runtime capabilities) that map to Rust enums with exhaustive matching, and one set of types generates the CLI JSON output, MCP tool schemas and agent contracts (`serde` + `schemars`).

Known costs:

- No official Anthropic Rust SDK: the direct-API runtime uses a thin typed client (`reqwest` + `serde`).
- Jira, Freshservice, Datadog, GitLab and GitHub: thin typed clients for the endpoints loom uses, on the blocking `ureq` transport of the forge clients, rather than broad crates.
- MCP: official `rmcp`. Bedrock: AWS SDK for Rust.

## Crate layout

Crates are added when a version needs them, never ahead of time.

| Crate | Kind | Introduced | Content |
| --- | --- | --- | --- |
| `loom` | bin | v0.1 | CLI (clap) |
| `loomd` | bin | v0.1 | Daemon: API server, scheduler, updater |
| `loom-api` | lib | v0.1 | Types shared by the API, the CLI `--json` output and the MCP tools. No I/O. Builds on `loom-core` and re-exports its primitives (`Timestamp`, `ContextName`) |
| `loom-config` | lib | v0.1 | Loom home resolution, files under it, `config.toml` parsing. No I/O. Shared by `loom` and `loomd` |
| `loom-update` | lib | v0.1 | Release discovery, update decision (pure), download, verification, install, rollback, auto-upgrade decisions (pure), update state, pre-flight and health check (ADR-0011). Shared by `loom` and `loomd` |
| `loom-telemetry` | lib | v0.1 | `tracing` subscriber of both binaries: logs on stderr, OTLP/HTTP trace export when enabled (decisions pure), W3C trace context propagation. Shared by `loom` and `loomd` |
| `loom-repo` | lib | v0.2 | Repo identity, context matching, scan diff, update decisions (pure); discovery scan (`scan` module: directory listing and git files read with `gix-config`) and state (`state` module: read with `gix`, bounded by a time budget), the only modules with I/O, no git process |
| `loom-core` | lib | v0.3 | Task domain (pure): kinds, statuses and transitions, IDs and prefix resolution, commands, events, undo, replay of a log and the changes of a hand edit, priority order, file formats. No I/O, clock, randomness source or async runtime: time and randomness are injected. Owns the primitives the API shares: `Timestamp`, `ContextName`; dates are `jiff` civil dates |
| `loom-store` | lib | v0.2 | SQLite index (`index.db`): repos and search in v0.2; `StorePort`, its local files adapter, the tasks index and the hand-edit decision in v0.3 (git backup next) |
| `loom-forge` | lib | v0.4 | MR review (ADR-0016): the provider-neutral model (MR, version, thread, anchor, drafts, verdict), parsing of the GitHub and GitLab payloads, MR keys, context of an MR (pure, with the `match.hosts` matcher of `loom-repo`); the forge clients in one `client` module (`ureq`, TLS of `loom-update`), its only I/O; later re-anchoring and the publish plan |
| `loom-connector` | lib | Connectors | Ports and types of [Connector ports](connector-ports.md); Datadog, Jira and Freshservice adapters: parsing, queries, conversions (pure) and one `client` module each, their only I/O |

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
