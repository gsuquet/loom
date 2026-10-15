# How loom stores tasks

> This page tells where a task lives, and why its files are the truth and the index is only a copy. It also tells how `loomd` never leaves half a file, and how it records the edits that you make by hand. Layout and modes: [context stores](../reference/configuration.md#context-stores). Formats: [task files](../reference/task-file.md). Endpoint: [`POST /v1/tasks/reindex`](../reference/api.md#tasks). Decisions: [ADR-0002](../adr/0002-files-source-of-truth.md), [ADR-0003](../adr/0003-one-store-per-context.md), [ADR-0013](../adr/0013-task-store.md).

## Files first

A task is a directory, `<store>/tasks/<id>/`, in the store of its context. It holds two files:

- `task.md`: what the task is now. A TOML frontmatter (title, kind, status, importance, links) and free notes. Yours to read and edit with any tool.
- `events.jsonl`: what happened to it, one event per line. It only grows.

Each context has its own store (`store.path`). Thus, you can encrypt or delete the tasks of a client, or make a backup of them, with one directory. The other tasks do not change. The name of the directory is the ID of the task, never its title. If you rename a task, no file moves.

`task.md` stores no `updated` time. The last change of a task is the time of its last event. Thus, if two machines edit two fields of one task, they do not conflict on a timestamp.

## The index is a copy

If `loomd` read every file each time to list, sort and search tasks, it would be too slow for a few thousand tasks. `loomd` keeps a copy of each task in `index.db`, the SQLite database that already indexes the repositories:

- one row per task: its fields, its notes, the keys of its links (`CORE-1042`, `!88`), and the hash of its `task.md`.
- a full-text table over the title, the notes and the link keys, with the tokenizer of the repository search (case and diacritics folded).

If you delete the index, nothing is lost. `loomd` rebuilds the task rows from the files when it starts, and `loom reindex` rebuilds them while `loomd` runs. Two rebuilds of the same files give the same rows. A reindex replaces the task rows in one transaction and never touches the repositories: a scan rebuilds their index. If the reindex cannot read a task (a permission, a disk error), it does not stop. It reports the task with its path, the task keeps its previous row, and the reindex indexes every other task.

The repositories and the tasks each have their own writer in `loomd`, on two threads. SQLite lets one writer write at a time. The other writer waits for it (up to 5 s) and does not fail. Thus, a repository scan that takes a long time only delays a task write.

## Writes that survive a crash

`loomd` is the only process that writes the stores. It writes one task at a time: each task has a lock, which `loomd` holds for the full read, change and write.

- `loomd` never writes `task.md` in place. It writes the new content to `.task.md.tmp` beside it, syncs it to disk, then renames it over `task.md`. Then it syncs the directory, so that the rename itself survives a power cut. A crash before the rename leaves the previous file whole. The next write replaces the temporary file.
- `loomd` appends the events and syncs them. If a crash cuts the last line, the next append starts on a new line. The cut line stays in the log, and a read of the log skips it. The new events stay whole.
- `loomd` writes a new task whole in a hidden directory, `.<id>.tmp`, then renames it to `<id>`. After a crash, the task exists fully or not at all.

A change writes `task.md` before its events. A crash between the two leaves a file that its log does not explain yet, never an event that the file contradicts.

## Hand edits

`task.md` is made for edits by hand: to fix a title, an editor is faster than any command. The history must still tell what changed, so `loomd` separates its own writes from your writes.

- Each index row keeps the SHA-256 of the `task.md` that `loomd` read or wrote last. A file with that hash is the one that `loomd` knows: nothing to do. This is how `loomd` ignores the watcher events of its own writes.
- `loomd` compares a file with another hash with its log. It replays `events.jsonl` (the created task, then every change in order). Then it compares the result with the frontmatter, field by field. Each field that is different goes into one `edited` event of the actor `file`, which `loomd` appends to the log. `loomd` does not write `task.md` itself, so your layout and comments stay.
- Only the notes changed: the task is indexed, the notes are searchable, no event. The log records fields, not prose.
- The log is not reliable: the task is indexed, no event. Examples: no creation event, a line from a newer loom, changes that do not follow from each other. A guess can put a wrong change in the history.
- A line that `loomd` does not replay does not make the log unreliable, unless a newer loom wrote it. A line cut by a crash, a line that is not an event and a line of an unknown type stay in the log as they are. `loomd` replays the other lines and records the edit as usual. A line from a newer loom can hold a change that this version cannot read, so `loomd` cannot tell that change from your edit. It records nothing for the task until you upgrade loom, and it logs a warning each time it indexes the task.

`loom show` prints the lines that are not replayed as a warning on standard error. Thus a damaged log is not silent.

The comparison uses the log, not the index, so the result is correct in every order of events. The log, not the index, tells what the task was. If you edit `task.md` while `loomd` is stopped, or before you delete `index.db`, `loomd` still records the edit at its next start. When nothing changed, the hash only lets `loomd` skip the replay.

`loomd` finds changes in two ways:

- a file watcher on `tasks/` and `archive/` of every store (FSEvents on macOS, inotify on Linux). Thus, `loomd` indexes and records an edit almost as soon as the editor saves it.
- a reconciliation of every task when `loomd` starts, for what changed while it was stopped. There is also a reconciliation each time the watcher reports that it lost events.

`loom reindex` reconciles every task before it rebuilds the rows, so it can append `edited` events. Without this step, a rebuild can store the hash of an edit whose watcher event is still in the queue. That edit then looks like a write of `loomd`.

A write by `loomd` changes `task.md`, then appends its events. If `loomd` crashes between the two, the next reconciliation finds a file that its log does not explain. It records the difference as a hand edit. The change stays, with the actor `file` instead of your actor.

## Archived tasks

A task that is done or cancelled is history that you can still need. But it does not belong with the tasks that you use each day. Thus, an archived task is in `<store>/archive/<yyyy>/<id>/`, where `<yyyy>` is the year of its close time. It is the same directory with the same two files, at a different location. Directories per year stay small, and you can copy or delete an old year easily.

`loomd` moves a task there `[tasks] archive_after` (7 days by default) after its close time. It does this when it starts, then every hour. The close time is the time of the last event that set the current status of the task. Thus a note that you add after `done` does not delay the move. If you open a task again and close it again, the delay starts at the second close time.

If no event in the log closed the task (for example, a lost `events.jsonl`), `loomd` never archives the task automatically. A date that `loomd` guesses could hide a task too early. The reconciliation on start runs before the pass. Thus, if you close a task by hand while `loomd` is stopped, `loomd` records the edit before the pass looks for the close time.

The year is the UTC year of the close time, because `loom-core` has no time zone. A task closed near midnight on 31 December can go into the directory of one year or the other.

Each move is one commit of the store repository, `archived(<id>): moved to archive/<yyyy>`. The commit contains the removal from `tasks/` and the addition to the archive. If the archive already contains a directory with the same ID (for example, a copy that you restored by hand), `loomd` refuses the move and logs it. `loomd` never deletes a task.

`loomd` indexes an archived task like the other tasks, with its archive year. `loom show` finds it. `loom search` finds it after all the other results. `loom reindex` builds its row again from the files. The task list and the priority list do not show it; `archived=true` lists the archived tasks.

An archived task is read-only. `loomd` refuses each action, edit and undo with `read_only` and the reason `archived`, and a command adds nothing to its log. The location makes a task archived, not its status. If a hand edit sets an archived task to `ready`, `loomd` records the edit, but the task stays read-only and out of the priority list. To open the task again, move its directory back to `tasks/`. No command does this.

A move is one `rename` of the task directory. Before the rename, `loomd` creates the year directory and syncs it to the disk. On one file system, a rename is atomic. Thus, after a crash, the task is in `tasks/` or in the archive, in one location only.

`loomd` writes the index row after the rename. If a crash occurs between the two, the reconciliation at the next start finds the same file at a new location. It then indexes the task as archived. The hash alone shows no change. The same rule applies to a directory that you move by hand, in the two directions.

The file watcher watches `archive/` and `tasks/`. `loomd` records and commits a hand edit of an archived file like any other edit, at its archive path. The commit of an archived task also removes from git what stays of it in `tasks/`. Thus, a move is one commit.

## When a file is invalid

Sometimes `loomd` cannot read a `task.md`. These are examples:

- broken TOML.
- a key that is not there.
- an `id` that is not its directory.
- a `context` that is not the context of its store.
- bytes that are not UTF-8.

`loomd` indexes the task anyway, and marks it invalid with the reason and its line. The task is read-only until you fix the file. `loomd` never changes a file that it cannot read, so nothing that you typed is lost, and it records no event for it. `loom reindex` lists the invalid files. When you fix the file, the reconciliation records the fields that are different from the last state of the log, as for any hand edit.

A task whose kind or status comes from a newer version of loom is valid, but it is also read-only. This version keeps the value and does not change the task.

## When two stores hold one ID

The index has one row for each ID, and a task ID is unique in every context. A task directory that you copy by hand from one context store to another breaks this rule. `loomd` cannot tell which copy a command is for, and a command that writes the wrong file would hide the other.

`loomd` does not choose. It indexes the ID once, as invalid, with the error `duplicate ID across contexts` and the contexts that hold it. The row has the context and the file of the first store, in the order of the contexts. The task is read-only: each action, edit and undo is refused with `read_only`, and `loomd` writes neither copy. `loom reindex` lists the ID in `invalid`, and `loomd` logs a warning when it finds it.

`loomd` looks at the directories of the other stores to find a duplicate, and reads no file for it. A store that it cannot look at (a permission, a disk error) is skipped with a warning, so it never blocks the tasks of the other contexts. Until that store can be read, `loomd` does not see its copy as a duplicate. A reindex finds a duplicate from the listing of the stores, also when a copy cannot be read.

To fix this, remove one task directory (or move it out of the stores). `loomd` then indexes the copy that is left as a valid task, and records the difference between the file and its log as a hand edit.
