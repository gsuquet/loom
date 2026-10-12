# ADR-0007: Three-class autonomy policy for proposed actions

- Status: accepted
- Date: 2026-10-12

## Context

A factory running agents unattended cannot wait for my approval on every action, but actions visible to clients must stay under my control.

## Decision

Proposed actions are classified: internal and reversible (push to `loom/*`, draft MR, child tasks), visible to the team (MR ready, MR comment, Jira transition), visible to a client or requester (ticket or Slack reply, public Jira comment). Internal actions are approved automatically once a template version has earned it in a context (gate pass rate over recent runs). Team-visible actions are configurable per context. Client-visible actions always require me.

## Consequences

- Autonomy is granted per template version and context, and revoked when metrics drop.
- Templates must be versioned and measured from their first run (v0.7).
- Auto-delegation (`requested_by: rule:<id>`) does not change approval rules.
