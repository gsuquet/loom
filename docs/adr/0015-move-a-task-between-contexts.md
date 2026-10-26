# ADR-0015: Move a task between context stores

- Status: proposed
- Date: 2026-10-26

## Context

A task belongs to one context, and each context has its own store (ADR-0003), usually its own git repository and backup. A task created in the wrong context, or a task that changes client, must move. The move touches two stores and two git histories, and it cannot be one atomic step: there is no operation that renames a directory between two stores that can be on two volumes.

The task must keep its ID and its history (ADR-0013: IDs are random, the log is the history). The reconciliation already treats the same ID in two stores as a duplicate: both copies are read-only, and loomd never chooses between them.

## Decision

1. **Copy, then remove, with a rollback.** The move copies the task directory (every file, not only `task.md` and `events.jsonl`) into the target store under a hidden name, writes the new `task.md` and the new events in the copy, then renames it to `tasks/<id>`. After that, it removes the source directory (a rename to a hidden name, then a delete). It never removes the source before the copy is in place.
2. **A failure after the copy is rolled back.** If the removal of the source fails, loomd removes the copy and the task stays in the source store. The error says that the task was not moved. If the rollback also fails, both stores hold the task, and the error says which directory to remove. A crash between the copy and the removal has the same result.
3. **Two copies are never merged or chosen automatically.** The duplicate ID rule of the reconciliation applies: the task is read-only, the doctor and `loom reindex` list it, and one removal by hand makes it whole again.
4. **The move is an event.** It is an `edited` event with a `context` field change (`from`, `to`), appended in the target store after the copied log. A hand edit that was not recorded yet is recorded first, as for any command.
5. **A move is not undone.** `loom undo` refuses when the last change is a move, and the error names the context to move back to. A move back is a new move.
6. **Relations stay valid.** A parent is a task of the context of its children (domain model). Thus a task with a parent or with children is not moved: the error asks the user to detach it first. Moving several tasks together is out of scope.
7. **Commits.** The target store gets one commit of the task (`edited(<id>): context`). The source store gets one commit of the removal (`moved(<id>): moved to <context>`). The two commits are not atomic; a failed commit is retried as any other (the store is pending).
8. **The focus does not follow.** The focus is per context and machine-local. A focused task that moves is not the focus of its old context any more.

## Consequences

- No data is lost on a failure: at every moment at least one complete copy exists.
- A hard crash can leave two copies. This is rare, visible and fixed with one removal.
- An older loom reads the `context` field change as an invalid event line and skips it (forward compatibility, ADR-0013, decision 6).
- A future central instance can implement the same two port operations (`receive`, `remove`) in its own way.
