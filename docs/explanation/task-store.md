# How loom stores tasks

> Where a task lives, why its files are the truth and the index only a copy, and how `loomd` writes without ever leaving half a file. Layout and modes: [context stores](../reference/configuration.md#context-stores); formats: [task files](../reference/task-file.md); endpoint: [`POST /v1/tasks/reindex`](../reference/api.md#tasks); decisions: [ADR-0002](../adr/0002-files-source-of-truth.md), [ADR-0003](../adr/0003-one-store-per-context.md), [ADR-0013](../adr/0013-task-store.md).

## Files first

A task is a directory, `<store>/tasks/<id>/`, in the store of its context. It holds two files:

- `task.md`: what the task is now. A TOML frontmatter (title, kind, status, importance, links) and free notes. Yours to read and edit with any tool.
- `events.jsonl`: what happened to it, one event per line. It only grows.

Each context has its own store (`store.path`), so a client's tasks can be backed up, encrypted or deleted with one directory, without touching the others. The directory is named by the task's ID, never by its title: renaming a task moves nothing.

`task.md` stores no `updated` time. The last change of a task is the time of its last event: two machines editing two fields of one task do not conflict on a timestamp.

## The index is a copy

Listing, sorting and searching tasks by reading every file each time would not scale to a few thousand tasks. `loomd` keeps a copy of each task in `index.db`, the SQLite database that already indexes the repositories:

- one row per task: its fields, its notes, the keys of its links (`CORE-1042`, `!88`), and the hash of its `task.md`;
- a full-text table over the title, the notes and the link keys, with the tokenizer of the repository search (case and diacritics folded).

Nothing in the index is lost when it is deleted: `loomd` rebuilds the task rows from the files when it starts, and `loom reindex` does it while it runs. Two rebuilds of the same files give the same rows. A reindex replaces the task rows in one transaction and never touches the repositories, whose index is rebuilt by a scan. A task it cannot read (a permission, a disk error) does not stop it: the task is reported with its path and keeps its previous row, and every other task is indexed.

The repositories and the tasks each have their own writer in `loomd`, on two threads. SQLite lets one write at a time; the other waits for it (up to 5 s) instead of failing, so a task write during a long repository scan is only delayed.

## Writes that survive a crash

`loomd` is the only process that writes the stores, and it writes one task at a time: each task has a lock held for the whole read, change and write.

- `task.md` is never written in place. The new content goes to `.task.md.tmp` beside it, is synced to disk, then renamed over `task.md`, and the directory is synced so that the rename itself survives a power cut. A crash before the rename leaves the previous file whole; the temporary file is replaced by the next write.
- Events are appended and synced. If a crash cuts the last line, the next append starts on a new line: the cut line is kept and skipped when the log is read, the new events stay whole.
- A new task is written whole in a hidden directory, `.<id>.tmp`, then renamed to `<id>`: after a crash, the task exists entirely or not at all.

A change writes `task.md` before its events. A crash in between leaves a file that its log does not explain yet, never an event that the file contradicts.

## When a file is invalid

A `task.md` that cannot be read (broken TOML, a missing key, an `id` that is not its directory, a `context` that is not its store's, bytes that are not UTF-8) is indexed anyway, marked invalid with the reason and its line. The task is read-only until the file is fixed: `loomd` never rewrites a file it cannot read, so nothing you typed is lost. `loom reindex` lists the invalid files.

A task whose kind or status comes from a newer version of loom is valid but read-only too: this version keeps the value and does not change the task.
