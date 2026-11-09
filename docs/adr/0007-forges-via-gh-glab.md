# 7. Forge access by shelling out to `gh` and `glab`

- Status: Accepted
- Date: 2026-11-09

## Context

loom talks to GitHub and several GitLab instances. Embedding API clients means token
management per host, pagination, retries and a large dependency surface. `gh` and `glab`
are already installed and authenticated per host.

## Decision

`internal/forge` wraps `gh` (host `github.com`) and `glab` (any other host) via
`os/exec`, parsing their JSON output. The forge kind is derived from the remote host. A
missing CLI is a clear runtime error naming the tool. Tests use recorded fixtures behind
an interface; no network.

## Consequences

- No tokens in loom; authentication is whatever the user configured for `gh`/`glab`.
- loom depends on the CLIs' JSON output staying stable; fixtures catch regressions.
- Some API features not exposed by the CLIs are out of reach (acceptable for the scope).
