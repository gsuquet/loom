# 1. Two stores: human knowledge and agent memory

- Status: Accepted
- Date: 2026-11-09

## Context

A first design put human notes and agent records in one knowledge base with
bronze/silver/gold layers. A day of real use showed the cost: agents produced dozens of work
records and notes in hours, every one went through review, and "silver" notes written by
agents were no longer the human's knowledge. Humans and agents need different things: the
human needs a small, curated set of ideas in their own words; agents need fast, bounded,
machine-readable working memory that many sessions write concurrently.

## Decision

loom manages two stores with different owners, formats and rules:

- **knowledge**: the human's notes, Zettelkasten-style, curated, read in Obsidian
  ([ADR 0003](0003-knowledge-zettelkasten-in-obsidian.md));
- **memory**: the agents' working memory, one Obsidian vault of notes per confidentiality boundary
  ([ADR 0010](0010-memory-vaults-one-note-per-item.md)).

Agents read knowledge but never write it; they write drafts into their memory vault, which the human
accepts (`loom accept`) after rewriting them. Memory may reference knowledge; knowledge never
references memory.

## Consequences

- Review load is bounded: the human reviews drafts and proposals, not every agent record.
- Knowledge stays the human's; agent-written material is explicitly provisional until accepted.
- Memory can be optimised for agents (format, budgets, concurrency) without caring about
  human readability.
- Two stores to search; `loom mem query` and the Obsidian CLI/`rg` cover each.
