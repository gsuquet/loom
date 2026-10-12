# ADR-0005: MCP transport with loom-owned, shaped tools

- Status: accepted
- Date: 2026-10-12

## Context

Agents need external data (Datadog logs, metrics, monitors, dashboards; later Sentry, GitLab, Jira). Options were: MCP, a CLI used through Bash, code execution, or proxying upstream MCP servers. Token cost is driven mainly by result shape, not transport.

## Decision

Loom's MCP server is the only MCP server an agent sees. It exposes a small set of loom-owned tools built on direct API clients. Results are aggregated first, projected (`fields`, `limit`), tabular as TSV, and spilled to a file in the run directory above a per-call budget (the agent gets a summary and the path, and reads slices with Read/Grep). Proxying upstream MCP servers stays possible for long-tail tools, under the same shaping rules.

## Consequences

- No shell access needed for data lookups; credentials stay in loomd.
- Loom controls result size end to end; tokens per call are recorded and compared.
- Loom maintains its own tools instead of mirroring an upstream tool surface.
- Datadog credentials: fine-grained PAT with read scopes only, per context.
