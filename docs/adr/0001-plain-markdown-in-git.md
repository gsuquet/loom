# 1. Plain markdown, YAML frontmatter and wikilinks in git as the only storage

- Status: Accepted
- Date: 2026-11-09

## Context

Notes outlive tools. Previous setups mixed editor-specific syntax, plugin metadata and
ad-hoc caches, which made the KB hard to read without the tool that produced it. The KB
must remain usable with `rg`, any editor and git alone, on every machine and in agents.

## Decision

The KB is a git repository of plain markdown files with YAML frontmatter. Links are
`[[wikilinks]]` only; no `![[embeds]]` and no plugin syntax. Git is the only storage and
history; there is no database of record. Any index (e.g. full-text search) is a disposable
cache rebuilt from the files.

## Consequences

- Every note is greppable, diffable and reviewable; agents read and write it with ordinary
  file tools.
- Sync, history and backup come from git; conflicts surface as git conflicts.
- Features needing structured queries must derive them from frontmatter at runtime or from a
  rebuildable cache, which bounds what loom can offer cheaply.
- Binary attachments live in `bronze/files/` and bloat history; they are kept small and few.
