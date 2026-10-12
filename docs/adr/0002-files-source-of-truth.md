# ADR-0002: Files as source of truth, SQLite as rebuildable index

- Status: accepted
- Date: 2026-10-12

## Context

Everything loom records must be readable and greppable by me, scripts and agents, survive a rewrite of the tool, and hold attachments (diffs, transcripts, slides). Search and ranking need an index.

## Decision

Task, event, note and mirror files are the source of truth. SQLite (`index.db`) is a machine-local index rebuilt by `loom reindex`. All writes go through loomd.

## Consequences

- Any tool can read the store; loom can be rewritten without migrating a database.
- The index can be lost without data loss; it also holds machine-local data (repo paths).
- Single writer (loomd) avoids concurrent writes on one machine. Across machines, see ADR-0003 and the storage doc.
- Hand edits must be detected (file watching) and reindexed.
