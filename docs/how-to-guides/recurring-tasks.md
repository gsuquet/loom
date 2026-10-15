# Create recurring tasks

> Configure `loomd` to create a task for each occurrence of a schedule. Examples: a timesheet on the last working day of the month, a weekly review each Friday.

Before you start, make sure of these conditions:

- `loom` and `loomd` are installed.
- The context is in `config.toml` ([configure contexts](configure-contexts.md)).

The [schedules reference](../reference/configuration.md#schedules) gives all the keys.

## Add a schedule

1. Write the rule as an RFC 5545 `RRULE`. `loomd` supports `FREQ=DAILY|WEEKLY|MONTHLY`, `INTERVAL`, `BYDAY`, `BYMONTHDAY` and `BYSETPOS`:

   | Each | `rrule` |
   | --- | --- |
   | Working day | `FREQ=DAILY;BYDAY=MO,TU,WE,TH,FR` |
   | Friday | `FREQ=WEEKLY;BYDAY=FR` |
   | Second Thursday | `FREQ=WEEKLY;INTERVAL=2;BYDAY=TH`, with `start` |
   | First Monday of the month | `FREQ=MONTHLY;BYDAY=1MO` |
   | Last day of the month | `FREQ=MONTHLY;BYMONTHDAY=-1` |
   | Last working day of the month | `FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1` |

2. Add a `[[contexts.<name>.schedules]]` table to `config.toml`, below the context of the tasks:

   ```toml
   [contexts.company-a]

   [[contexts.company-a.schedules]]
   id = "timesheet"
   title = "Fill in the timesheet"
   kind = "admin"
   rrule = "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1"
   policy = "replace"
   due = "same-day"
   ```

   - `id` identifies the schedule in its tasks. Do not change it after the schedule creates tasks.
   - `policy` gives the result when the task of the previous occurrence is open. `skip` (the default) creates no task until you close the previous task. `stack` keeps the two tasks. `replace` cancels the previous task.
   - If the rule has an `INTERVAL` of more than 1, add `start = 2026-10-01`. The count of periods starts at this date.

3. If more than one machine holds the context, add the machines to `machines`. Only the first machine creates the tasks:

   ```toml
   [contexts.company-a]
   machines = ["laptop", "tower"]
   ```

   If you do not add `machines`, `loomd` writes a warning at start. Two machines that share the store create each task two times.

4. Restart `loomd` to load the schedule. `loomd` examines the configuration first. If the rule has a part that `loomd` does not support, the error gives the name of the part.

   ```bash
   loom service restart
   ```

## Examine the tasks

On the day of an occurrence, the task is in the priority list of the context. Its status is `ready` and its due date is that day:

```bash
loom ls -c company-a
```

Each task has the origin link `schedule` / `<id>/<occurrence date>` (`timesheet/2026-10-30`). Thus a restart does not create the same occurrence again.

If `loomd` was stopped for more than one occurrence, it creates only the latest occurrence at its next start.

With `policy = "skip"`, a task of the schedule with an invalid `task.md` counts as open. The schedule creates no task until you correct the file. `loom reindex` shows the invalid files.

## Stop a schedule

1. Remove the table of the schedule from `config.toml`.
2. Restart `loomd`.

The tasks that the schedule created do not change.
