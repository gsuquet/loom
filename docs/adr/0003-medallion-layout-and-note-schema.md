# 3. Medallion (bronze/silver/gold) layout and note schema

- Status: Accepted
- Date: 2026-11-09

## Context

Captures, agent logs, meeting notes, refined knowledge and publishable output have very
different lifecycles. Mixing them makes linting, triage and publication guesswork.

## Decision

- `bronze/`: raw, append-only inputs (inbox, clips, papers, meetings, daily, agents, files).
- `silver/`: flat atomic notes typed by frontmatter `type`.
- `gold/`: publishable outputs (`articles talks adrs briefs`), each in `<slug>/<slug>.md`,
  rendered to a gitignored `dist/`.
- `_meta/`: templates, views, schema.

Frontmatter: `type`, `created`, `context`, `tags` everywhere; `source` and `status`
(`new triaged done`) in bronze; `sources` (wikilinks) in silver and gold; `author`
(`agent/<name>`) on agent-written notes. Type vocabularies:

- bronze: `capture clip paper meeting daily weekly agent-plan agent-report agent-changes`
- silver: `concept howto decision person project reference`
- gold: `article talk adr brief`

`loom lint` enforces the schema and warns on edits to `status: done` bronze notes.

## Consequences

- Triage is a well-defined flow: bronze inbox → silver note or done.
- Adding a type is a schema change (this ADR is superseded or amended), not a free-form tag.
- Lint rules stay simple because location and `type` must agree.
