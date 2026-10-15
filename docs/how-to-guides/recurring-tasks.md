# Create recurring tasks

> Have loomd create a task for each occurrence of a schedule: a timesheet on the last working day of the month, a weekly review every Friday.

Before you start: `loom` and `loomd` are installed and the context is defined in `config.toml` ([configure contexts](configure-contexts.md)). Every key is described in the [schedules reference](../reference/configuration.md#schedules).

## Add a schedule

1. Write the rule as an RFC 5545 `RRULE`. loomd supports `FREQ=DAILY|WEEKLY|MONTHLY`, `INTERVAL`, `BYDAY`, `BYMONTHDAY` and `BYSETPOS`:

   | Every | `rrule` |
   | --- | --- |
   | Working day | `FREQ=DAILY;BYDAY=MO,TU,WE,TH,FR` |
   | Friday | `FREQ=WEEKLY;BYDAY=FR` |
   | Other Thursday | `FREQ=WEEKLY;INTERVAL=2;BYDAY=TH`, with `start` |
   | First Monday of the month | `FREQ=MONTHLY;BYDAY=1MO` |
   | Last day of the month | `FREQ=MONTHLY;BYMONTHDAY=-1` |
   | Last working day of the month | `FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1` |

2. Add a `[[contexts.<name>.schedules]]` table to `config.toml`, under the context the tasks belong to:

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

   - `id` names the schedule in its tasks; keep it once the schedule has created tasks.
   - `policy` decides what happens when the task of the previous occurrence is still open: `skip` (default) creates nothing until it is closed, `stack` keeps both, `replace` cancels the old one.
   - For a rule with `INTERVAL` above 1, add `start = 2026-10-01`: the periods are counted from it.

3. Restart loomd to load the schedule. The configuration is checked first; a rule loomd does not support is refused with the part it names.

   ```bash
   loom service restart
   ```

## Check the tasks

On the day of an occurrence, the task shows up in the priority list of the context, `ready` and due that day:

```bash
loom ls -c company-a
```

Each task carries the origin link `schedule` / `<id>/<occurrence date>` (`timesheet/2026-10-30`): a restart never creates the same occurrence twice.

If loomd was stopped over several occurrences, it creates only the latest one at its next start.

## Stop a schedule

Remove its table from `config.toml` and restart loomd. The tasks it created stay as they are.
