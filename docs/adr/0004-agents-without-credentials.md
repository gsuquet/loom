# ADR-0004: Agents hold no credentials; effects are proposed actions

- Status: accepted
- Date: 2026-10-12

## Context

Loom delegates work to agents and must keep control over anything that leaves the machine: pushes, MRs, comments, ticket replies.

## Decision

Agents never receive forge, tracker or observability credentials. Their typed output contains proposed actions; loom executes them after gates, according to the autonomy policy (ADR-0007). External data reaches agents only through loom's MCP server (ADR-0005).

## Consequences

- Every external effect is executed, logged and attributable by loom.
- Credentials stay in loomd, resolved per context: an agent in one context cannot reach another context's systems.
- Agent environments are scrubbed of credentials; harness and OS-level restrictions keep agents from reading loom's configuration.
