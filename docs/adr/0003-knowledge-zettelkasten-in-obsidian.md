# 3. Knowledge is Zettelkasten-style, in Obsidian

- Status: Accepted
- Date: 2026-11-09

## Context

The knowledge repo is for the human to think with. Zettelkasten (fleeting, literature,
permanent and structure notes; one idea per note in one's own words; links with reasons) is a
proven model for that. Obsidian provides the graph, backlinks, views (Bases) and a CLI that
agents can query, which loom would otherwise have to rebuild. The app is closed source (free);
earlier constraints excluded closed products.

## Decision

- Layout: `inbox/` (fleeting), `sources/` (literature notes), `notes/` (permanent, flat),
  `maps/` (structure notes), `outputs/` (articles, talks, briefs), `attachments/`, `_meta/`.
- Plain markdown, YAML properties, `[[wikilinks]]`; readable file names (kebab-case) rather
  than timestamp IDs; `aliases` for display names; a small tag vocabulary in `_meta/tags.md`.
- Obsidian app and CLI are used, with core plugins and Bases only; no community plugins.
  This is a deliberate exception to the "no closed products" constraint, limited to the human
  UI: the files stay plain markdown, agents fall back to `rg` when the app is not running.
- Sync through git (`loom sync`), not Obsidian Sync.

## Consequences

- loom does not implement note creation, triage UI, views or rendering for knowledge.
- If Obsidian disappears, the repo remains a plain markdown Zettelkasten usable with any editor.
- Agent-drafted notes enter only through `loom accept` after the human rewrote them.
