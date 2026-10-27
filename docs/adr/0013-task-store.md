# ADR-0013: Task store, kinds, priority, focus and backup

- Status: accepted
- Date: 2026-10-13

## Context

v0.3 brings tasks: the store, the domain and the TUI to start the day with. The design docs left decisions open (one focus task or several, Presentation vs Content, Admin granularity), did not fix the priority rule or the frontmatter format, and put multi-machine sync in v0.3.

Constraints found while planning:

- Today, loom runs on one machine. A second one (Linux) is planned, not available.
- `config.toml` is versioned in the dotfiles: it is identical on every machine.
- The git profile of a forge may set `commit.gpgsign` with a GPG key. A daemon cannot answer a pinentry prompt.
- `serde_yaml` is archived; `toml_edit` is already a dependency and round-trips a document exactly.
- Two machines can run different loom versions for a few hours (auto-upgrade every 6 h).

## Decision

1. **Kinds** (closed enum): Story, Bug, Chore, Research, Investigation, Review, Support, Content, Admin, Interview, Learning.
   - Content replaces Presentation, with an optional `format`: `talk | article | training | workshop`. Done when delivered or published.
   - Interview is done when the feedback is written. Admin stays one kind (timesheets, expenses, staffing).
   - Learning is done when the course or certification is completed.
2. **Projects are parent tasks**, not an entity. Children see the parent's `files/` (design docs, ADRs). The TUI shows the tree in task detail and can group Today by parent.
3. **Priority is lexicographic**, no weights: pinned, then overdue or due today (earliest first), then importance (p0 first), then due date (earliest first, none last), then age (oldest first). Today lists `ready` and `active` tasks; `waiting` tasks with a future `until` are hidden; `inbox` has its own view. `--why` names the rule that ordered two neighbours.
4. **Focus**: one focus task per context, machine-local (in `index.db`, never synced). `loom go <id>` makes it the focus of its context, sets it `active`, puts the previous focus of that context back to `ready`, and changes directory to the task's repo (`repo` link, a slug of the v0.2 index) through `loom shell-init`.
5. **Files are the source of truth** (ADR-0002), SQLite is the index:
   - `task.md` has a **TOML frontmatter** between `+++` lines, edited with `toml_edit`: comments, key order and unknown keys survive a rewrite.
   - `events.jsonl` lines carry a schema version `v`, an actor and a `machine_id`.
   - IDs are 8 random Crockford base32 characters, shown as the shortest unique prefix.
   - loomd is the only writer (atomic write and rename). Hand edits are detected (file watcher, plus a reconciliation at start) and recorded as an `edited` event; an invalid file is flagged, kept as is, and reported by the doctor.
6. **Forward compatibility**: a version that reads an unknown field keeps it on rewrite; a task of an unknown kind or status is shown read-only. Nothing is dropped.
7. **Undo**: `loom undo` and `u` in the TUI revert the last local change of a task by appending a compensating event. History is never rewritten.
8. **Backup, not multi-machine, in v0.3**:
   - Each context store is a git repository. loomd commits each change (git CLI, ADR-0008), unsigned (`commit.gpgsign=false` for store repositories only): private data, no forge review.
   - With `store.backup.remote` set, loomd pushes and fast-forward pulls on a timer and on `loom sync`. A divergence stops the sync of that context and is reported by the doctor; it is never merged automatically.
   - `~/.loom/machine.toml` (local, unversioned, created at first start, hostname by default) names the machine. `[contexts.<name>] machines = [...]` lists the machines that hold a context; absent means all.
   - Poller per context (`poll_on`), leases and `task.md` conflicts turned into inbox tasks wait for a second machine.
9. **Recurring tasks**: `[[contexts.<name>.schedules]]` in `config.toml` with an RRULE (RFC 5545), kind, importance and a policy `skip | stack | replace`. Dedup key `(schedule_id, occurrence_date)`; only the latest missed occurrence is created.
10. **Archive**: a task moves to `archive/<yyyy>/<id>/` 7 days after `done` or `cancelled` (`[tasks] archive_after`). Archived tasks stay readable and searchable, read-only.

## Clarifications

- 2026-10-13 (#71): `toml_edit` was not yet a dependency, contrary to the Context section; #71 adds it (`parse` and `display` only), sharing its parser crates with `toml`. `task.md` stores no `updated` key: the time of the last change is the `at` of the last event, so an edit on one machine does not conflict with an edit of another field elsewhere.
- 2026-10-14 (#78): git commands on context stores run with `commit.gpgsign=false`, `tag.gpgsign=false` and `core.hooksPath=/dev/null`. Unlike the user's repositories (ADR-0008), a store is loom-owned data: the user's signing and hooks would only block backups from a daemon.
- 2026-10-27 (#97): decision 5 says that a hand edit is recorded as an `edited` event, which holds fields. A change of the notes alone left no trace. It now makes a `notes_changed` event of the actor `file`: the hash of the notes before and after, no text (the log stays free of prose, and a note that holds a secret is not copied into the ledger). Notes saved in `$EDITOR` (`loom edit`, the TUI) are hand edits too and are recorded the same way, so their actor is `file`. The event changes no field: a replay and `loom undo` skip it, and an older reader skips the line as an unknown type.

## Consequences

- The design example of `task.md` changes from YAML to TOML. Obsidian and GitHub show the frontmatter as text.
- Store history is unsigned. Its integrity rests on the private remote, not on signatures.
- One machine can hold all contexts or a subset; a second machine needs the deferred multi-machine work before both edit the same context.
- Merge and split in triage, the embedded text editor and the Runs, Approvals and Review views are not in v0.3.
- Adding a kind later is cheap; renaming or removing one needs a file migration.
