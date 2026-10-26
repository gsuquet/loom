# Manage tasks

> Capture a task from any shell. Then list the tasks to work on, change a task, and close it.

Before you start, make sure that:

- `loom` and `loomd` are installed.
- Your contexts are defined in `config.toml` ([configure contexts](configure-contexts.md)).

## Capture a task

In any directory, run `loom add` with the title of the task:

```bash
loom add Renew the passport
```

```text
k3q7 personal Renew the passport
```

The task goes to the inbox of the context of the current directory:

- In a repository, this is the context of the repository.
- Outside a repository, this is the context of a `match.paths` prefix, then `default_context`.

To select the context yourself, give `-c`. To select it for a full shell, set `LOOM_CONTEXT`:

```bash
loom add -c work Review the API draft
export LOOM_CONTEXT=work
```

If you already know the details, add them:

```bash
loom add --kind bug --importance p1 --due tomorrow --ready Login timeout on checkout
loom add --parent k3q7 --due +2w Book the appointment
```

`--ready` skips the inbox. `--due` accepts a date (`2026-11-02`), `today`, `tomorrow`, or a number of days or weeks from today (`+3d`, `+2w`).

## List what to work on

```bash
loom ls
```

```text
#  ID    CONTEXT  STATUS  P   KIND  DUE         TITLE
1  h8w2  work     ready   p0  chore              Reproduce on iOS
2  k3q8  work     ready   p1  bug   2026-10-15  Login timeout on checkout
```

This is the priority list of the current context. It shows the ready and active tasks. It also shows the tasks with the status `waiting` whose date has come. To see why a task has its position in the list, run this command:

```bash
loom ls --why
```

Other views:

- All contexts: `loom ls --all-contexts`.
- The inbox, or any status: `loom ls --status inbox`.

## Look at a task

Give the ID of the task, or the prefix that `loom ls` shows:

```bash
loom show k3q8
```

The output shows the fields first, then the notes. For scripts, each command accepts `--json`.

## Change a task

```bash
loom set k3q8 importance=p0 due=+3d
loom set k3q8 parent=h8w2 pinned=yes
loom set k3q8 due=none
loom set k3q8 status=active
```

`status` accepts every status except `waiting`. To move a task to `waiting`, use `loom snooze`. `loomd` checks the move as for `loom done`: a task that has this status already, or a closed task that does not reopen to `ready`, is refused.

When `loom set` has several keys, each change is its own event and `loom set` applies them in this order: the fields, the status, the pin, the context. If `loomd` refuses one, `loom set` stops there. The changes that came before it stay, and `loom undo` reverts them one at a time.

## Move a task to another context

```bash
loom set k3q8 context=company-b
```

The task keeps its ID and its history. Its files move to the store of the other context, and the log ends with an `edited` event that has a `context` change. `loom set` prints the task in its new context.

A task with a parent or with children does not move, because a parent is in the context of its children. To move a child, clear its parent in the same command:

```bash
loom set k3q8 parent=none context=company-b
```

An archived task, and a task that is read-only, do not move. The target context needs a store on this machine.

`loom undo` does not undo a move. To go back, move the task again.

If a move fails, `loom` prints why. In almost every case the task stays in its first context. If the message says that the task is in two stores, remove the task directory from the store that you do not want, then run `loom reindex`.

To change the notes, or several fields at the same time, open the file.

**Caution:** While the file is open, do not change the task from a different terminal, for example to close it. `loom` does not lock the file. If the task changes, your save replaces that change and `loom` shows a warning. `loom undo` reverts your edit.

```bash
loom edit k3q8
```

`loom edit` uses `VISUAL`, then `EDITOR`. If the saved file is not valid, `loom` gives the reason and keeps the file as you saved it. To correct it, run `loom edit` again.

In the TUI, press `e` on the task page to do the same. The TUI gives the terminal to the editor and takes it back when the editor ends.

## Wait on someone

```bash
loom snooze k3q8 +1w --on Alice
```

The task leaves the priority list until that date.

## Close a task

```bash
loom done k3q8
loom cancel h8w2
```

To revert a mistake, run `loom undo k3q8`. It reverts the last change of the task, also if this change closed the task.

`loomd` archives a closed task 7 days after its close time (`[tasks] archive_after`). It moves the task to the archive of its context store. An archived task is read-only.

## Find an archived task

List the archived tasks of the context:

```bash
loom ls --archived
```

Other ways to find an archived task:

- To see only one status in all the contexts, add `--status` and `--all-contexts`:

  ```bash
  loom ls --archived --status cancelled --all-contexts
  ```

- To find a task by its words, use `loom search`. The archived tasks come after all the other results:

  ```bash
  loom search certificates
  ```

- To see the task, use `loom show`. It prints `read-only:  archived in archive/<yyyy>`:

  ```bash
  loom show k3q8
  ```

## Open an archived task again

`loom done`, `set`, `undo`, `edit` and the other commands refuse an archived task. Change the file first, then move the directory: in this order, the archive pass cannot move the task back.

1. Open `task.md` in the archive directory with your editor, and set `status = "ready"`. `loomd` records the edit.

   ```bash
   $EDITOR ~/.loom/contexts/work/archive/2026/k3q8m2xa/task.md
   ```

2. Move the task directory back to `tasks/` in its store. The store is `<loom home>/contexts/<context>`, if `store.path` does not give a different path.

   ```bash
   mv ~/.loom/contexts/work/archive/2026/k3q8m2xa ~/.loom/contexts/work/tasks/
   ```

The task is open again, and the commands accept it.

## Reference

- [CLI conventions: task commands](../reference/cli/conventions.md#task-commands)
- [`loom` CLI reference](../reference/cli/loom.md)
