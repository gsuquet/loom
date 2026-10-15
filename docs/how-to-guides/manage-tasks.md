# Manage tasks

> Capture a task from any shell, list what to work on, and look at one task.

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

## Reference

- [CLI conventions: task commands](../reference/cli/conventions.md#task-commands)
- [`loom` CLI reference](../reference/cli/loom.md)
