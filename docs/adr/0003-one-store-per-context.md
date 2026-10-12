# ADR-0003: One store root per context

- Status: accepted
- Date: 2026-10-12

## Context

I work for several clients. Their data must be separable for encryption, backup location, and purge at the end of an engagement. Cross-context search remains available to me.

## Decision

Each context has its own root (`~/.loom/contexts/<ctx>/`) and its own backup target. Portable knowledge lives outside contexts (`~/.loom/knowledge/`).

## Consequences

- Purging a client is one `rm` plus a reindex, if its backup repo is separate (purging from a shared git repo requires a history rewrite).
- Backup hosts must be allowed by each client, otherwise encrypted.
- Cross-context queries go through the index, which spans all contexts.
