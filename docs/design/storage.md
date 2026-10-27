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
      remote/<provider>-<key>.json  # daemon-owned snapshots (alerts, read-only sources)
      issues/<provider>-<key>/      # editable mirror of a tracker issue (see tracker-sync.md)
      mrs/<provider>-<key>/         # MR mirror: mr.json, description.md, versions.json, threads.json, diffs/<sha>.patch, review.json (see reviews.md)
      files/                        # transcripts, slides, other artifacts
      runs/<run-id>/                # context pack, stream, gate results, outputs
    archive/<yyyy>/<id>/
    notes/                          # context-scoped notes
    journal/<yyyy>/<mm>.jsonl       # events not tied to a task
```

- One root per context: separate encryption, backup, and purge with one `rm`.
- One owner per file: `task.md` is mine, `remote/` is the daemon's, `events.jsonl` only grows. Issue mirrors are the exception, co-owned and protected by a 3-way merge. In an MR mirror, `review.json` is mine, written through loomd; the other files are the daemon's.
- `task.md` is private and never pushed to a tracker: it is backed up with its store (see [Backup](#backup-v03)). What goes upstream lives only in issue mirrors.
- IDs: 8 random Crockford base32 chars, shown as shortest unique prefix. External keys (`CORE-1042`, `!88`) resolve as aliases. Directory named by ID, so renaming never moves it.
- Backup: one git repo per client context (purging from a shared repo requires a history rewrite). A shared repo for personal and employer contexts needs one branch per context, a later option (see [Backup](#backup-v03)). Backup host must be allowed by the client, otherwise encrypt (`age`, `git-crypt`).

```markdown
+++
id = "k3q7zm2a"
title = "Login timeout on mobile checkout"
kind = "bug"
context = "company-a"
status = "active"
importance = "p1"
due = 2026-10-15
created = 2026-10-12T09:14:00Z

[[links]]
role = "origin"
provider = "jira"
key = "CORE-1042"
url = "https://…"

[[links]]
role = "repo"
repo = "company-a/checkout"

[relations]
parent = "h8w2nc4d"
spawned_from = []
blocks = []
+++

Free notes, kind template sections.
```

- `task.md` has no `updated` key: the last change is the `at` of the last event in `events.jsonl`.
- Frontmatter is TOML between `+++` lines, rewritten with `toml_edit`: comments, key order and unknown keys survive ([ADR-0013](../adr/0013-task-store.md)). A task of an unknown kind or status, written by a newer version, is shown read-only.
- `events.jsonl` lines carry a schema version `v`, the actor and the `machine_id`.

## Moving a task between contexts

- The task directory is copied into the target store, then removed from the source store ([ADR-0015](../adr/0015-move-a-task-between-contexts.md)). The task keeps its ID, its files and its log; an `edited` event with a `context` field change ends the log.
- If the removal fails, the copy is removed and the task stays in the source store. Only a crash, or a failed rollback, leaves two copies: they are a duplicate ID, read-only until one is removed by hand.
- The source and the target store each get one commit.

## Backup (v0.3)

- Each context store is a git repository. loomd commits each change, unsigned (store repositories only: private data, no forge review).
- `store.backup.remote` set: push and fast-forward pull on a timer and on `loom sync`. A divergence stops the sync of that context and is reported by `loom doctor`, never merged automatically.
- In v0.3 each context needs its own backup remote: the same `store.backup.remote` in two contexts is rejected when the configuration is loaded, since both stores would push to the same branch. One branch per context in a shared repository is a later option.
- `~/.loom/machine.toml` (local, unversioned) names the machine; `[contexts.<name>] machines = [...]` lists the machines holding a context (absent: all). `config.toml` stays identical on every machine.

## Multiple machines (after v0.3)

Needed once a second machine edits the same context.

- `StorePort`: local files + git sync now, HTTP adapter to a central instance (container / VM) later.
- One poller machine per context (`poll_on = "<machine>"`) until the central instance.
- Runs take a lease to avoid double execution. Sync conflicts become inbox tasks.
- `merge=union` on `*.jsonl` in `.gitattributes`; `task.md` is the only real conflict point.
- `machine_id` on every event.
