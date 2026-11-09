# 2. One knowledge repo; one context repo per confidentiality boundary

- Status: Accepted
- Date: 2026-11-09

## Context

Some contexts hold data that must stay isolated: it must be deletable at the end of an
engagement and must not reach machines or people outside it. Knowledge (what the human
learned) is generic and portable; context (projects, decisions, work records, meetings) is
situational and often confidential.

## Decision

- One **knowledge** repo per human, private, holding no isolated-context data.
- One **context** repo per confidentiality boundary (`context-<ctx>`), private, configured per
  context in `contexts.toml` (`store`). Agents push directly to its `main`; history and the
  weekly review replace per-change review.
- Events of a context are only ever written to that context's repo (hooks, extraction,
  consolidation all resolve the context first).
- Where the repos are hosted is configuration, not design.

## Consequences

- Ending an engagement means deleting one repo; a shared machine can carry the knowledge repo
  and only the contexts it is allowed to.
- Moving something learned in an isolated context into knowledge requires the human to rewrite
  it generically (`loom accept` checks for leaks).
- More repos to sync; `loom sync` iterates over the configured stores.
