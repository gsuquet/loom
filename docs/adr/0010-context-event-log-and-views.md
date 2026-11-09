# 10. Context store: append-only event log with generated views

- Status: Accepted
- Date: 2026-11-09

## Context

Agents need: a small, deterministic start context; concurrent writes from many sessions without
merge conflicts; dated facts that can be replaced or expire; and cheap queries. Markdown notes
per work unit (the first design) were costly to write, to review and to retrieve, and the
"why" of decisions ended up only in transcripts.

## Decision

- Storage: `log/YYYY/MM/DD-<session>.jsonl`, one JSON event per line, one file per agent
  session; `blobs/<sha256>.<ext>` for plans, reports, artifacts and drafts.
- Event types: `fact pref decision plan outcome question entity link procedure signal`, with
  common fields `id t scope s src at conf ttl sup by`. `schema.json` is versioned and validated
  on write; loom ships migrations.
- Views (`views/hot.md`, `views/repo/<repo>.md`, `views/project/<project>.md`) are compiled by
  loom: replacements and expiry applied, deduplicated, ranked by scope, confidence, use and
  recency, cut to budgets (L0 ≤100 lines, L1 ≤240). Views are caches, never edited.
- Start context (L0 + L1, ≈3k tokens) is injected once per session; later reads use
  `loom ctx query`.
- Expiry: facts default to 90 days unless used; `pref` and `decision` never expire, only get
  replaced; outcomes older than 90 days are rolled up. Nothing is deleted from the log.
- Session transcripts are never stored; they are read transiently at session end for
  extraction ([ADR 0014](0014-continuous-improvement-loop.md)).

## Consequences

- No merge conflicts on writes; history and blame come from git.
- Humans read the views if they want, but the format optimises for agents.
- Retrieval quality depends on view compilation and budgets, which the improvement loop can
  tune with evidence.
