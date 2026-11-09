# 10. Memory vaults: one note per item, written deterministically, queried by tags

- Status: Accepted (revised the same day: an append-only JSONL event log was considered first)
- Date: 2026-11-09

## Context

Agents need a small, deterministic start context, concurrent writes without merge conflicts, claims that can be
replaced or expire, and cheap queries. A JSONL event log met those needs but was opaque to the human, needed a
separate blob store for plans and drafts, and reimplemented what Obsidian offers (properties, tags, backlinks,
Bases views, a CLI). The advantages that mattered (append-only items, one query layer with replacement and expiry
semantics, compiled views, no dependency on an app) do not depend on the file format.

## Decision

- One Obsidian vault per confidentiality boundary, `memory-<ctx>`, one Markdown note per item, organised in folders by
  kind: living notes (`task spec project repo review research investigation support question draft`), claims
  (`fact decision pref procedure`, never edited, replaced through a `supersedes` list), records (`entity signal`).
- Notes are written deterministically: created from per-kind templates by a script (later `loom mem new`), properties
  changed only by a script (`loom mem set`), bodies of living notes edited freely. A checker enforces file names,
  a property allowlist, status vocabularies, tags and links. Per-kind schemas start minimal.
- Properties hold single-valued fields; tags carry every query facet (`type/<kind>`, `project/`, `repo/`, `area/`,
  `tool/`, `domain/`, `lang/`, `kind/`), nested and prefix-matched.
- Views (`views/hot.md`, repo and project views) are compiled for agents within budgets and injected once per session;
  Bases dashboards serve the human. Queries apply semantics (superseded claims and expired facts hidden).
- The Obsidian app and CLI are optional accelerators; all agent paths work on the files alone.
- Session transcripts are never stored ([ADR 0014](0014-continuous-improvement-loop.md)).

## Consequences

- The human can browse, review and steer agent memory in Obsidian (task board, review queue, health dashboard).
- New items are new files, so concurrent agents rarely conflict; living notes are usually owned by one task at a time.
- More files and more bytes than a log; irrelevant for agents, which read compiled views and query results.
- Naming avoids "context" (the confidentiality boundary) and "agents" (deployed agents): the vaults are `memory-<ctx>`.
