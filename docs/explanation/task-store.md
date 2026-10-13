# How loom stores tasks

> Where a task lives, why its files are the truth and the index only a copy, how `loomd` writes without ever leaving half a file, and how it records the edits you make by hand. Layout and modes: [context stores](../reference/configuration.md#context-stores); formats: [task files](../reference/task-file.md); endpoint: [`POST /v1/tasks/reindex`](../reference/api.md#tasks); decisions: [ADR-0002](../adr/0002-files-source-of-truth.md), [ADR-0003](../adr/0003-one-store-per-context.md), [ADR-0013](../adr/0013-task-store.md).

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

## Hand edits

`task.md` is meant to be edited by hand: fixing a title in an editor is faster than any command. The history must still say what changed, so `loomd` tells its own writes from yours.

- Each index row keeps the SHA-256 of the `task.md` that `loomd` last read or wrote. A file with that hash is the one `loomd` knows: nothing to do. This is how `loomd` ignores the watcher events of its own writes.
- A file with another hash is compared with its log: `loomd` replays `events.jsonl` (the created task, then every change in order) and compares the result with the frontmatter, field by field. Each field that differs goes into one `edited` event of the actor `file`, appended to the log; `task.md` itself is not written, so your formatting and comments stay.
- Only the notes changed: the task is indexed, the notes are searchable, no event. The log records fields, not prose.
- The log cannot be trusted (no creation event, a line written by a newer loom, changes that do not follow from each other): the task is indexed, no event. Guessing would put a wrong change in the history.

Comparing with the log rather than with the index keeps this right in every order of events: an edit made while `loomd` was stopped, or before `index.db` was deleted, is still recorded at the next start, because the log, not the index, says what the task was. The hash only saves the replay when nothing changed.

`loomd` learns about changes two ways:

- a file watcher on `tasks/` of every store (FSEvents on macOS, inotify on Linux), so that an edit is indexed and recorded about as soon as the editor saves it;
- a reconciliation of every task when `loomd` starts, for what changed while it was stopped, and whenever the watcher reports it lost events.

`loom reindex` reconciles every task before it rebuilds the rows, so it may append `edited` events: a rebuild would otherwise store the hash of an edit whose watcher event is still queued, and that edit would then look like `loomd`'s own.

A write by `loomd` changes `task.md` and appends its events in that order. If it crashes in between, the next reconciliation sees a file its log does not explain and records the difference as a hand edit: the change is kept, with the actor `file` instead of yours.

## When a file is invalid

A `task.md` that cannot be read (broken TOML, a missing key, an `id` that is not its directory, a `context` that is not its store's, bytes that are not UTF-8) is indexed anyway, marked invalid with the reason and its line. The task is read-only until the file is fixed: `loomd` never rewrites a file it cannot read, so nothing you typed is lost, and it records no event for it. `loom reindex` lists the invalid files. Once the file is fixed, the reconciliation records the fields that differ from the last state of the log, as for any hand edit.

A task whose kind or status comes from a newer version of loom is valid but read-only too: this version keeps the value and does not change the task.
