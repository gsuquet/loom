# ADR-0006: Private task file, shared issue mirrors with 3-way merge

- Status: accepted
- Date: 2026-10-12

## Context

Tickets (Jira, GitLab, GitHub) must be editable locally and pushed upstream, while my own notes about a task must never leak to a client tracker.

## Decision

`task.md` is private and never pushed. Each linked issue has a mirror (`issues/<provider>-<key>/issue.md`) co-owned by me and the daemon, merged with a 3-way merge against the last synced state (`.base/`). Push is explicit by default, sends only changed fields, and is blocked on conflict. Creating an issue is the first push of a mirror without a remote key.

## Consequences

- No accidental leak of private notes.
- Remote changes and local edits merge without silent overwrite; conflicts become inbox items.
- Untouched descriptions are never re-sent, so lossy Markdown conversions (Jira ADF) cannot damage them.
- Task status and issue status are independent.
