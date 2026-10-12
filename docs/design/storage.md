# Storage

> On-disk layout, file ownership, identifiers, backups and multi-machine operation.

Files are the source of truth, SQLite is a rebuildable index (`loom reindex`).

```
~/.loom/
  config.toml
  index.db                          # rebuildable, machine-local (repo paths live here)
  knowledge/                        # portable notes, survive a context purge
  contexts/<ctx>/
    tasks/<id>/
      task.md                       # mine: frontmatter + free notes
      events.jsonl                  # append-only
      remote/<provider>-<key>.json  # daemon-owned snapshots (MRs, alerts, read-only sources)
      issues/<provider>-<key>/      # editable mirror of a tracker issue (see tracker-sync.md)
      mrs/<provider>-<key>/         # MR mirror: description, threads.jsonl, diffs/<sha>.patch, drafts/review.json
      files/                        # diffs per SHA, transcripts, slides
      runs/<run-id>/                # context pack, stream, gate results, outputs
    archive/<yyyy>/<id>/
    notes/                          # context-scoped notes
    journal/<yyyy>/<mm>.jsonl       # events not tied to a task
```

- One root per context: separate encryption, backup, and purge with one `rm`.
- One owner per file: `task.md` is mine, `remote/` is the daemon's, `events.jsonl` only grows. Issue mirrors are the exception, co-owned and protected by a 3-way merge.
- `task.md` is private and never pushed anywhere. What goes upstream lives only in issue mirrors.
- IDs: 8 random Crockford base32 chars, shown as shortest unique prefix. External keys (`CORE-1042`, `!88`) resolve as aliases. Directory named by ID, so renaming never moves it.
- Backup: one git repo per client context (purging from a shared repo requires a history rewrite). Shared repo acceptable for personal and Ippon. Backup host must be allowed by the client, otherwise encrypt (`age`, `git-crypt`).

```markdown
---
id: k3q7zm2a
title: Login timeout on mobile checkout
kind: bug
context: company-a
status: active
importance: p1
due: 2026-10-15
parent: null
links:
  - { role: origin, provider: jira, key: CORE-1042, url: "https://…" }
  - { role: related, provider: gitlab, key: "!88", url: "https://…" }
relations:
  spawned_from: [h8w2nc4d]
  blocks: []
created: 2026-10-12T09:14:00Z
---

Free notes, kind template sections.
```

## Multiple machines

- `StorePort`: local files + git sync now, HTTP adapter to a central instance (container / VM) later.
- One poller machine per context (`poll_on = "<machine>"`) until the central instance.
- Runs take a lease to avoid double execution. Sync conflicts become inbox tasks.
- `merge=union` on `*.jsonl` in `.gitattributes`; `task.md` is the only real conflict point.
- `machine_id` on every event.
