# Manage tasks

> Capture a task from any shell, list what to work on, change it, and close it.

Before you start: `loom` and `loomd` are installed and your contexts are defined in `config.toml` ([configure contexts](configure-contexts.md)).

## Capture a task

From any directory:

```bash
loom add Renew the passport
```

```text
k3q7 personal Renew the passport
```

The task goes to the inbox of the context of the current directory: inside a repository, the context of the repository; otherwise the context of a `match.paths` prefix, then `default_context`. To choose it yourself, pass `-c`, or set `LOOM_CONTEXT` for a whole shell:

```bash
loom add -c work Review the API draft
export LOOM_CONTEXT=work
```

Add details when you already know them:

```bash
loom add --kind bug --importance p1 --due tomorrow --ready Login timeout on checkout
loom add --parent k3q7 --due +2w Book the appointment
```

`--ready` skips the inbox. `--due` takes a date (`2026-11-02`), `today`, `tomorrow`, or a number of days or weeks from today (`+3d`, `+2w`).

## List what to work on

```bash
loom ls
```

```text
#  ID    CONTEXT  STATUS  P   KIND  DUE         TITLE
1  h8w2  work     ready   p0  chore              Reproduce on iOS
2  k3q8  work     ready   p1  bug   2026-10-15  Login timeout on checkout
```

This is the priority list of the current context: ready and active tasks, and the waiting ones whose date has come. To see why a task is where it is:

```bash
loom ls --why
```

Other views:

- every context: `loom ls --all-contexts`;
- the inbox, or any status: `loom ls --status inbox`.

## Look at a task

Pass its ID, or the prefix `loom ls` shows:

```bash
loom show k3q8
```

The fields come first, then the notes. For scripts, every command takes `--json`.

## Change a task

```bash
loom set k3q8 importance=p0 due=+3d
loom set k3q8 parent=h8w2 pinned=yes
loom set k3q8 due=none
```

To change the notes, or several fields at once, open the file:

```bash
loom edit k3q8
```

`loom edit` uses `VISUAL`, then `EDITOR`. If the saved file is invalid, `loom` says why and keeps it as saved: run `loom edit` again to fix it.

The file is not locked while it is open. If the task changes meanwhile (you close it from another terminal, for example), your save replaces that change and `loom` warns you; `loom undo` reverts your edit.

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

To revert a mistake, `loom undo k3q8` reverts the last change of the task, a closing included.

A closed task leaves `loom ls --status done` 7 days after its closing (`[tasks] archive_after`): `loomd` moves it to the archive of its context store, where it is read-only.

## Find an archived task

```bash
loom ls --archived
loom ls --archived --status cancelled --all-contexts
loom search certificates
```

`loom show <id>` works on an archived task as on any other; it prints `read-only:  archived in archive/<yyyy>`. `loom search` lists archived tasks after every other result.

An archived task cannot be changed: `loom done`, `set`, `undo`, `edit` and the others refuse it. To work on it again, move its directory back to `tasks/` in its store (`<loom home>/contexts/<context>` unless `store.path` says otherwise), then reopen it with `loom edit k3q8`, setting `status = "ready"`:

```bash
mv ~/.loom/contexts/work/archive/2026/k3q8m2xa ~/.loom/contexts/work/tasks/
loom edit k3q8
```

Reopen it right away: a task still closed goes back to the archive at the next hourly pass.

## Reference

- [CLI conventions: task commands](../reference/cli/conventions.md#task-commands)
- [`loom` CLI reference](../reference/cli/loom.md)
