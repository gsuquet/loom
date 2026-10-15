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
```

To change the notes, or several fields at the same time, open the file.

**Caution:** While the file is open, do not change the task from a different terminal, for example to close it. `loom` does not lock the file. If the task changes, your save replaces that change and `loom` shows a warning. `loom undo` reverts your edit.

```bash
loom edit k3q8
```

`loom edit` uses `VISUAL`, then `EDITOR`. If the saved file is not valid, `loom` gives the reason and keeps the file as you saved it. To correct it, run `loom edit` again.

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

## Reference

- [CLI conventions: task commands](../reference/cli/conventions.md#task-commands)
- [`loom` CLI reference](../reference/cli/loom.md)
