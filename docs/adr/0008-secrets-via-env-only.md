# 8. Secrets via environment variables only

- Status: Accepted
- Date: 2026-11-09

## Context

Integrations (ticketing, calendars, capture) need credentials. Each context uses a
different password manager CLI. Storing tokens in loom config or caches would duplicate
secrets and leak them into dotfiles or backups.

## Decision

loom reads credentials only from environment variables. The caller fetches them (mise
per-directory env, shell wrapper or the password-manager CLI named by the context's
`secrets` key). loom never writes a secret to disk, never logs one and never stores tokens
in `contexts.toml`. `secrets` is informational for tooling around loom.

## Consequences

- No credential storage code to secure or audit in loom.
- A missing variable is a clear error naming the variable and context.
- Running a command outside the configured environment requires exporting the variable
  explicitly.
