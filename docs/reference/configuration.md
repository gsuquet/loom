# Configuration reference

> Loom home directory, configuration file, runtime files, state and log of `loomd`, as shipped.

## Loom home directory

`loomd` keeps its files in one directory, the loom home. It is resolved in this order:

| Order | Source | Condition |
| --- | --- | --- |
| 1 | `--loom-home <DIR>` | Given. An empty value is an error. |
| 2 | `LOOM_HOME` environment variable | Set and non-empty. |
| 3 | `~/.loom` | The user's home directory is known (`HOME`). |

When no source gives a directory, `loomd` exits with code 1.

`loom` resolves the loom home the same way, without step 1: it has no `--loom-home` flag.

A relative path is made absolute against the current directory. Symbolic links and `..` are kept as given, not resolved.

If the loom home does not exist, `loomd` creates it (mode `0700`). `loom upgrade` and `loom rollback` create it the same way when they first write to it.

## `config.toml`

Path: `<loom home>/config.toml`. Format: [TOML](https://toml.io/en/v1.1.0).

| Case | Behavior |
| --- | --- |
| No file | Defaults. |
| File empty or comments only | Defaults. |
| Unknown key or table, at any level | Rejected. |
| Value of the wrong type | Rejected. |
| TOML syntax error | Rejected. |

A rejected file stops `loomd` with exit code 1 before it creates anything, and this message on stderr:

```text
Error: invalid configuration <path>:<line>:<column>: <message>
```

`line` and `column` start at 1. `column` counts characters. For an unknown key, `message` names it:

```text
Error: invalid configuration /home/me/.loom/config.toml:3:1: unknown field `foo`, expected one of `contexts`, `daemon`, `default_context`, `forges`, `repos`, `tasks`, `telemetry`, `update`
```

`loom` reads `config.toml` at each command for `[telemetry]`. If `loom` cannot read or parse the file, the command does not stop for that reason. `loom` exports no traces, prints this warning on stderr (`-q` hides it) and runs the command. Commands that need the file for something else, such as `loom service install`, still fail on it. `loom upgrade` reads `[update] channel` the same way. If it cannot read or parse the file, the key stays unset and `loom upgrade` prints a warning, so that you can still repair an install.

```text
 WARN traces are not exported: invalid configuration <path>:<line>:<column>: <message>
```

### Keys

| Key | Type | Default | Effect |
| --- | --- | --- | --- |
| `default_context` | string: context name | none | Context of the repositories no other rule matches. See [contexts and repositories](#contexts-and-repositories). |
| `[contexts.<name>] match.paths`, `match.hosts`, `update`, `store.path`, `store.backup.remote`, `machines` | | | See [contexts and repositories](#contexts-and-repositories). |
| `[tasks] archive_after` | string: duration | `"7d"` | The time after its close time (when it became `done` or `cancelled`) at which a task moves from `tasks/<id>/` to `archive/<yyyy>/<id>/`. `<yyyy>` is the UTC year of the close time. Duration syntax of `[update] check_interval`. At least `"1d"`. See [archive](#archive). |
| `[[contexts.<name>.schedules]]` | | | See [schedules](#schedules). |
| `[tasks] sync_interval` | string: duration | `"15m"` | Time between two syncs of each context store with its `store.backup.remote`. Duration syntax of `[update] check_interval`. At least `"1m"`. See [store backup](#store-backup). |
| `[repos] roots`, `max_depth`, `exclude`, `fetch_interval`, `fetch_concurrency` | | | See [contexts and repositories](#contexts-and-repositories). |
| `[repos."<path>"] context`, `update` | | | See [contexts and repositories](#contexts-and-repositories). |
| `[forges."<host>"] kind` | | | See [contexts and repositories](#contexts-and-repositories). |
| `[forges."<host>"] token_cmd`, `token_env`, `poll_interval`, `review_requests`; `[contexts.<name>.forges."<host>"] token_cmd`, `token_env` | | | See [forges](#forges). |
| `[daemon] service` | boolean | `true` | `false`: `loom service install` refuses with exit code 1 and a message that names this key and the file. `loom service uninstall` and `loom service status` still work. Does not stop or remove an installed service. |
| `[telemetry] enabled` | boolean | `false` | Export traces. See [`[telemetry]`](#telemetry). |
| `[telemetry] otlp_endpoint` | string | none | OTLP/HTTP traces endpoint. See [`[telemetry]`](#telemetry). |
| `[update] policy` | string: `"auto"`, `"notify"`, `"off"` | `"auto"` for a `stable` or `dev` build, `"notify"` for a `local` build | What `loomd` does when a newer release exists. `auto`: installs it (see [auto-upgrade](../explanation/auto-upgrade.md)). `notify`: records it in `state/update.json`, shown by `loom status`. `off`: never checks, no network access. |
| `[update] channel` | string: `"stable"`, `"dev"` | The channel last given to `loom upgrade --channel` (or `install.sh --channel`), otherwise the build channel (`loom version`); none for a `local` build | Releases `loomd` and `loom upgrade` without `--channel` check. `stable`: the latest stable release. `dev`: dev prereleases and stable releases, the highest wins (as `loom upgrade --channel dev`). See [channel resolution](#channel-resolution). |
| `[update] check_interval` | string: duration | `"6h"` | Time between two checks, before a random ±10 %. One or more `<integer><unit>` parts, units `d`, `h`, `m`, `s`, no spaces: `"6h"`, `"1h30m"`, `"90m"`. At least `"10m"`: a shorter value is rejected like an unknown key. |

```toml
[daemon]
service = false

[update]
policy = "notify"
channel = "stable"
check_interval = "12h"
```

A `check_interval` below the minimum:

```text
Error: invalid configuration /home/me/.loom/config.toml:8:18: check_interval `5m` is shorter than the minimum 10m
```

### Contexts and repositories

`loomd` reads and validates `default_context`, `[contexts]`, `[repos]` and `[forges]` with the rest of `config.toml`. An invalid value stops `loomd`, and `loom` reports it like any other error in the file. `loomd` uses these keys for the [repository index](#indexdb). It reads them once, when it starts, so a change takes effect after a restart ([`loom service restart`](cli/loom.md#loom-service-restart)). The scan on start then assigns again the contexts (and the slugs) of every repository.

`~` is the user's home directory (`HOME`). When `HOME` is unknown, `loomd` exits with code 1.

`update`, `fetch_interval` and `fetch_concurrency` control the automatic updates of `loomd` ([updates](api.md#updates), [how updates work](../explanation/repo-updates.md)). An update fetches the primary remote of the repository. Then it fast-forwards the repository, but only if the fast-forward cannot change local work. The update policy of a repository is its `[repos."<path>"] update`, otherwise the `update` of its context, otherwise `ff`. A repository without a context (`unassigned`) and without `[repos."<path>"] update` gets `fetch`: fetched, never fast-forwarded. An explicit `[repos."<path>"] update` always applies, `ff` included.

```toml
default_context = "personal"

[contexts.personal]
match.paths = ["~/perso"]
match.hosts = ["github.com/gsuquet"]

[contexts.company-a]
match.paths = ["~/work/company-a"]
match.hosts = ["gitlab.com/company-a", "gitlab.company-a.example"]
update = "fetch"

[repos]
roots = ["~/perso", "~/work"]
max_depth = 4
exclude = ["archive"]
fetch_interval = "1h"
fetch_concurrency = 4

[repos."~/perso/client-fork"]
context = "company-a"
update = "off"

[forges."gitlab.company-a.example"]
kind = "gitlab"
```

| Key | Type | Default | Constraints |
| --- | --- | --- | --- |
| `default_context` | string | none | Names a context defined by a `[contexts.<name>]` table. |
| `[contexts.<name>]` | table | none | `<name>`: 1 to 64 lowercase ASCII letters, digits and `-`. The first character is a letter, the last character is not `-` (the rule of `-c`). `unassigned` is reserved. A table without keys is valid. |
| `[contexts.<name>] color` | string: ANSI color | none | Color of the context badge in the [TUI](tui.md#context-colors). `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`, `bright_black`, `bright_red`, `bright_green`, `bright_yellow`, `bright_blue`, `bright_magenta`, `bright_cyan`, `bright_white`. |
| `[contexts.<name>] match.paths` | array of strings: paths | none | At least one entry when given. Each entry is absolute, `~` or starts with `~/`; `..` is not allowed. |
| `[contexts.<name>] match.hosts` | array of strings | none | At least one entry when given. Each entry is `host` or `host/path-prefix`: a lowercase host name (ASCII letters, digits, `-`, `.`) without scheme, user or port, then path segments that are not empty and contain no space. A `/` at the end is ignored. |
| `[contexts.<name>] update` | string: `"ff"`, `"fetch"`, `"off"` | none (`ff`) | Update policy of the context's repositories. `ff`: fetch, then fast-forward the branch `HEAD` is on and the default branch when it is safe. `fetch`: fetch only, no branch moves. `off`: no git command. |
| `[contexts.<name>] store.path` | string: path | `<loom home>/contexts/<name>` | Root of the context's task store. Same path rules as `match.paths`; `~` is the user's home, not the loom home. Two contexts cannot have the same `store.path`. |
| `[contexts.<name>] store.backup.remote` | string: git URL or path | none | Repository the context store is pushed to and pulled from (see [store backup](#store-backup)). Not empty, no whitespace or control characters, first character not `-`. Two contexts cannot have the same remote: their stores would diverge at once. |
| `[contexts.<name>] machines` | array of strings: machine names | none (every machine) | The machines that hold the context's store, by their [`machine.toml`](#machinetoml) `id`. At least one entry when given; each entry follows the rules of `id`. On a machine it does not name, `loomd` does not load the store: no `tasks/` is created, its tasks are not indexed. The repositories of the context are matched as usual. |
| `[repos] roots` | array of strings: paths | `["~/perso", "~/work"]` | Same path rules as `match.paths`. May be empty. |
| `[repos] max_depth` | integer | `4` | 0 to 4294967295. Deepest directory level examined below each root. The root is level 0. |
| `[repos] exclude` | array of strings: glob patterns | `[]` | Each pattern is not empty and is a valid glob (`*`, `?`, `**`, `[…]`, `{a,b}`). A pattern without `/` matches a directory name at any depth (`archive`, `*-old`). A pattern with `/` matches the whole directory path relative to its root, with `/` separators (`clients/old`, `**/fixtures`). A directory that matches is not walked. Hidden directories and `node_modules`, `vendor`, `target`, `dist`, `build`, `.venv`, `venv`, `__pycache__` are always excluded. |
| `[repos] fetch_interval` | string: duration | `"1h"` | Duration syntax of `[update] check_interval`. At least `"1m"`. Time between two updates of a repository, ±10 % at random, and between two refreshes of every repository state. A failed fetch waits 2, 4, then 8 times as long. |
| `[repos] fetch_concurrency` | integer | `4` | 1 to 4294967295. Number of updates that run at the same time. A value above 64 counts as 64. |
| `[repos."<path>"]` | table | none | `<path>`: same path rules as `match.paths`. Two keys that name the same path (`"~/a"` and `"~/a/"`) are rejected. |
| `[repos."<path>"] context` | string | none | Names a defined context. |
| `[repos."<path>"] update` | string: `"ff"`, `"fetch"`, `"off"` | none | Update policy of this repository, over its context's, and over the `fetch` default of an `unassigned` repository. |
| `[forges."<host>"]` | table | none | `<host>`: a lowercase host name (ASCII letters, digits, `-`, `.`), without port or path. |
| `[forges."<host>"] kind` | string: `"github"`, `"gitlab"` | required | Forge of the host. The other keys of the table are in [forges](#forges). |

The check compares the paths as written, after it removes `.` components and repeated or final `/`. `~` stays unexpanded for this check.

Two contexts cannot have the same `match.paths` entry, the same `match.hosts` entry or the same `store.path`. For `match.hosts`, the comparison of path segments ignores ASCII case. The error is at the second entry and names the location of the first:

```text
Error: invalid configuration /home/me/.loom/config.toml:5:16: match.paths `~/work/` of context `company-b` is also `~/work` of context `company-a` (line 2, column 16): an entry can belong to one context only
```

Errors in an array element (`roots`, `exclude`, `match.paths`, `match.hosts`) are located at the start of the array.

### Forges

A forge credential lets `loomd` call the API of a forge host for a context. `loomd` reads the credentials when it starts. `loom repo doctor` checks each credential (the [`forge <context> <host>`](cli/conventions.md#loom-repo-doctor) checks). [Connect a forge](../how-to-guides/connect-a-forge.md) shows how to create the token.

```toml
[forges."github.com"]
kind = "github"
token_cmd = "gh auth token --hostname github.com"

[forges."gitlab.company-a.example"]
kind = "gitlab"
token_env = "GITLAB_TOKEN"
poll_interval = "10m"

[contexts.company-a.forges."github.com"]
token_cmd = "op read op://company-a/github/token"
```

| Key | Type | Default | Constraints |
| --- | --- | --- | --- |
| `[forges."<host>"] token_cmd` | string: shell command | none | Not empty. `loomd` runs it with `sh -c`, with no input, and stops it after 10 s. Its standard output, trimmed, is the token. Not with `token_env`. |
| `[forges."<host>"] token_env` | string: variable name | none | ASCII letters, digits and `_`, not starting with a digit. The variable of the environment of `loomd` that holds the token. Not with `token_cmd`. |
| `[forges."<host>"] poll_interval` | string: duration | `"5m"` | Duration syntax of `[update] check_interval`. At least `"1m"`. Interval of the [forge metadata](api.md#forge-metadata) poll of the host, per credential. |
| `[forges."<host>"] review_requests` | string: `"teams"`, `"direct"` | `"teams"` | GitHub review requests that count: to me and my teams (`teams`), or to me only (`direct`). `loomd` validates it; no version of `loomd` uses it yet. |
| `[contexts.<name>.forges."<host>"]` | table | none | `<host>`: the rules of `[forges."<host>"]`. The host is `github.com`, `gitlab.com`, or has a `[forges."<host>"]` table: a host with no known forge is an error. |
| `[contexts.<name>.forges."<host>"] token_cmd`, `token_env` | string | none | The rules of `[forges."<host>"]`. Exactly one of them. |

The credential of a context for a host is its `[contexts.<name>.forges."<host>"]` table, otherwise the `token_cmd` or `token_env` of `[forges."<host>"]`. Use a context table for another account on the same host. A context without a credential for a host does not use that host. A `[forges."<host>"]` table without `token_cmd` and `token_env` gives the kind of the host only.

`loomd` keeps a token in memory and never writes it. It does not log the token, put it in a trace, or give it to git or to an agent. git uses your own credentials. After a `401`, `loomd` gets the token again. `loomd` sends the token only to the API of its host: `https://api.github.com` for `github.com`, `https://<host>/api/v3` for another GitHub host (GitHub Enterprise Server), `https://<host>/api/v4` for GitLab. The token does not follow a redirect to another host.

As a service, `loomd` has a minimal `PATH` and does not see the variables of your shell. Write the absolute path of the program in `token_cmd` (`command -v gh` gives it), and prefer `token_cmd` to `token_env`.

Both keys in one table:

```text
Error: invalid configuration /home/me/.loom/config.toml:1:1: token_cmd and token_env are both set: give only one source for the token
```

### Schedules

A `[[contexts.<name>.schedules]]` table defines a recurring task. For each occurrence of the schedule, `loomd` creates one task in the context. `loomd` reads the schedules when it starts. A change to a schedule applies after a restart.

One machine runs the schedules of a context: the first machine in its [`machines`](#contexts-and-repositories). At start, the other machines that hold the context write a log line that names this machine. They create no task for the schedules. The tasks come to them through the [store backup](#store-backup).

- If the first machine is off, no machine creates the tasks. When that machine starts again, it creates only the latest occurrence that it did not create.
- To move the schedules to a different machine, put that machine first in `machines`. Then restart `loomd` on the two machines.
- If a context has no `machines`, each machine holds the context and runs its schedules. Each `loomd` writes this warning at start, because two machines that share the store create each occurrence two times:

```text
WARN schedules run on every machine holding the context: set `machines` so that only its first machine creates their tasks context=company-a
```

```toml
[contexts.company-a]

[[contexts.company-a.schedules]]
id = "timesheet"
title = "Fill in the timesheet"
kind = "admin"
importance = "p1"
rrule = "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1"
policy = "replace"
due = "same-day"

[[contexts.company-a.schedules]]
id = "one-on-one"
title = "Prepare the 1:1"
kind = "chore"
rrule = "FREQ=WEEKLY;INTERVAL=2;BYDAY=TH"
start = 2026-10-01
```

| Key | Type | Default | Constraints |
| --- | --- | --- | --- |
| `id` | string | required | 1 to 64 lowercase ASCII letters, digits and `-`. The first character is not `-`. Each schedule of a context has a different `id`. The `id` is part of the dedup key of each task. If you change it, the schedule starts again with no task. |
| `title` | string | required | One line that is not blank. The title of each task. |
| `kind` | string | required | `story`, `bug`, `chore`, `research`, `investigation`, `review`, `support`, `content`, `admin`, `interview` or `learning`. |
| `importance` | string: `"p0"` to `"p3"` | the default of the kind | The importance of each task. |
| `rrule` | string: recurrence rule | required | A rule in the subset of the RFC 5545 `RRULE` that the table below gives. |
| `start` | date: `2026-10-01` or `"2026-10-01"` | none | The schedule has no occurrence before this date. `start` is necessary in these conditions: `INTERVAL` is more than 1 (the count of periods starts at the period of `start`); `FREQ=WEEKLY` has no `BYDAY` (the weekday of `start` applies); `FREQ=MONTHLY` has no `BYDAY` and no `BYMONTHDAY` (the day of the month of `start` applies). |
| `policy` | string: `"skip"`, `"stack"`, `"replace"` | `"skip"` | The result when an occurrence is due and a task of an earlier occurrence is open (`inbox`, `ready`, `active` or `waiting`, and not archived). `skip`: `loomd` creates no task. When no earlier task is open, `loomd` creates the latest occurrence. `stack`: `loomd` creates the task and keeps the open tasks. `replace`: `loomd` creates the task, then cancels the open tasks. |
| `due` | string: `"same-day"` | none | `same-day`: the due date of each task is the date of its occurrence. If `due` is not there, the tasks have no due date. |

In `rrule`, a `;` separates the parts. Names and values can be in uppercase or lowercase. Each part can occur one time only.

| Part | Values | Effect |
| --- | --- | --- |
| `FREQ` | `DAILY`, `WEEKLY`, `MONTHLY` | Required. The period: one day, one week (Monday to Sunday), one month. |
| `INTERVAL` | positive integer | Default 1. One period in `INTERVAL` periods. The count starts at the period of `start`. |
| `BYDAY` | a list of `MO`, `TU`, `WE`, `TH`, `FR`, `SA`, `SU`, with commas. With `FREQ=MONTHLY`, each day can have a rank from `1` to `5` or from `-1` to `-5` (`1MO`: the first Monday, `-1FR`: the last Friday) | `DAILY`: only these weekdays. `WEEKLY`: these days of the week. `MONTHLY`: these days of the month. A day without a rank is each such weekday of the month. |
| `BYMONTHDAY` | a list of `1` to `31` or `-1` (the last day) to `-31`, with commas. `FREQ=MONTHLY` only | These days of the month. If a month does not have the day, that month has no occurrence for it. With `BYDAY`, only the days that the two parts give. |
| `BYSETPOS` | a list of `1` to `366` or `-1` (the last) to `-366`, with commas. `FREQ=WEEKLY` or `MONTHLY`, with `BYDAY` or `BYMONTHDAY` | Keeps the days at these positions in the days of the period, in date order. |

`loomd` refuses all other parts (`COUNT`, `UNTIL`, `BYHOUR`, `BYMINUTE`, `BYSECOND`, `BYMONTH`, `BYWEEKNO`, `BYYEARDAY`, `WKST`, `X-…`) and all other frequencies. The error gives the name of the part:

```text
Error: invalid configuration /home/me/.loom/config.toml:8:9: rrule `FREQ=DAILY;BYHOUR=9`: `BYHOUR` is not supported: use FREQ=DAILY|WEEKLY|MONTHLY, INTERVAL, BYDAY, BYMONTHDAY and BYSETPOS
```

The error for a missing `start` shows the location of the `[[contexts.<name>.schedules]]` header. The other errors show the location of their value. For an `id` that occurs two times, the error shows the second `id` and gives the location of the first.

An occurrence is a date, not a time. `loomd` examines each schedule when it starts, then one time each minute. It compares the schedule with the local date (the time zone of the system). Thus `loomd` creates a task on the day of its occurrence, at the first examination after local midnight. A change of the UTC offset does not move or repeat an occurrence. Each examination creates a maximum of one task for each schedule:

- The occurrence is the latest occurrence on or before today. It is after the latest occurrence that has a task, and not before `start`. If `loomd` was stopped for a week, it creates one task, not one task for each occurrence that it did not create.
- If a schedule has no task, `loomd` creates its first task on its first occurrence from today. A new schedule never creates a task for a date in the past.
- The task status is `ready`. The task has the title, kind, importance and due date of the schedule. It has the origin link `provider = "schedule"`, `key = "<id>/<occurrence date>"` (`timesheet/2026-10-30`). This link is the dedup key: a restart or a second examination does not create the same occurrence again.
- If the `task.md` of a task is invalid, `loomd` reads the latest `"<id>/<date>"` in quotes in its text. The task counts for that occurrence, as an open task that `loomd` never cancels.
- If a task has no link (for example, you removed it by hand) or is not in the index, the task does not count. `loomd` can then create its occurrence again.
- The events of the task, and the cancellations of `replace`, have the actor `{"type":"daemon"}`. `loomd` commits each change to the store, as for the changes of the API. Each change sends `task.changed` to the clients of [`GET /v1/events`](api.md#change-events).
- If `loomd` cannot create a task, it tries again at each examination. It writes a warning only when the error is different from the previous error.

### Channel resolution

The effective channel of `loomd` and of `loom upgrade` without `--channel` is the first of:

| Order | Source | `loom status` shows |
| --- | --- | --- |
| 1 | `[update] channel` in `config.toml`. | `config` |
| 2 | `channel` of `state/update.json`: the channel last given to `loom upgrade --channel`. `install.sh --channel` runs `loom upgrade --channel <channel> --check`, which records it. | `chosen` |
| 3 | The build channel (`loom version`): `stable` or `dev`. A `local` build has none: nothing is checked. | `build` |

`loomd` reads `config.toml` at start and `state/update.json` before each check, so a channel chosen while it runs applies from its next check. No command writes `config.toml`.

### Update checks

| `policy` | Effective `channel` | Network |
| --- | --- | --- |
| `off` | any | None. |
| `auto`, `notify` | none (`local` build, no `channel` configured or chosen) | None. |
| `auto`, `notify` | `stable` or `dev` | One check per interval, through the GitHub REST API (`LOOM_RELEASES_URL` replaces `https://api.github.com`). |

Schedule:

- The first check runs `min(5 min, check_interval)` ± 10 % after `loomd` starts, never at once.
- Each next check runs `check_interval` ± 10 % after the last check (`last_check` of `state/update.json`), but never before that first delay.
- After a failed check, `loomd` tries again at the next interval.

When its environment sets `LOOM_GITHUB_TOKEN` or `GITHUB_TOKEN`, `loomd` authenticates with it. `loomd` never runs `gh`.

## User service

`loom service install` installs `loomd` as a user service. `loom service` reads `config.toml` only for `[daemon] service`.

| Platform | Manager | File | Logs |
| --- | --- | --- | --- |
| macOS | launchd user agent, label `io.github.gsuquet.loomd`, domain `gui/<uid>` | `~/Library/LaunchAgents/io.github.gsuquet.loomd.plist` | `<loom home>/log/loomd.log` (stdout and stderr) |
| Linux | systemd user unit `loomd.service` | `$XDG_CONFIG_HOME/systemd/user/loomd.service` when `XDG_CONFIG_HOME` is an absolute path, otherwise `~/.config/systemd/user/loomd.service` | systemd journal: `journalctl --user -u loomd.service` |

The service runs the `loomd` that `loom service install` finds. This is the `loomd` next to the `loom` executable (symbolic links to `loom` resolved), otherwise the first `loomd` file in a `PATH` directory. `loom service install` writes its path canonicalized (absolute, symbolic links resolved). It gives the resolved loom home as `LOOM_HOME`.

Property list content (macOS): `ProgramArguments` = [`loomd` path], `EnvironmentVariables` = `LOOM_HOME`, `RunAtLoad` true, `KeepAlive` true, `ProcessType` `Background`, `StandardOutPath` and `StandardErrorPath` = `<loom home>/log/loomd.log`. Values are XML-escaped.

Unit content (Linux). `KillMode=process` makes systemd stop only `loomd` when it exits, not the watchdog it starts after an auto-upgrade. `RestartSec=1s` restarts `loomd` every second after it fails. The default start limit of systemd (5 starts in 10 s) still stops a crash loop, and the watchdog clears it after a rollback. To update a unit written before these two lines existed, run `loom service install` again.

```ini
[Unit]
Description=Loom daemon

[Service]
ExecStart="/usr/local/bin/loomd"
Environment="LOOM_HOME=/home/me/.loom"
Restart=always
RestartSec=1s
KillMode=process

[Install]
WantedBy=default.target
```

Values are double-quoted. `\` and `"` are backslash-escaped, `%` is written `%%`, and `$` in `ExecStart` is written `$$`.

A `loomd` path or loom home that is not valid UTF-8 or contains a control character is refused with exit code 1. When the content of the file differs from the generated content, `install` writes the file again: it overwrites hand edits.

The auto-upgrade watchdog treats `loomd` as supervised when these three conditions are true:

- The service file exists.
- The service file sets `LOOM_HOME` to the loom home of the watchdog.
- The service is loaded. On macOS, `launchctl print gui/<uid>/io.github.gsuquet.loomd` succeeds. On Linux, `systemctl --user is-enabled loomd.service` succeeds.

The watchdog then never starts `loomd` itself. After a rollback, it runs `launchctl kickstart -k gui/<uid>/io.github.gsuquet.loomd` (macOS) or `systemctl --user reset-failed loomd.service` then `systemctl --user restart loomd.service` (Linux). If one condition is false, the watchdog starts `loomd` detached. See [how loomd upgrades itself](../explanation/auto-upgrade.md).

## `[telemetry]`

Export of traces over OTLP, to diagnose loom itself. `loom` reads it at each command, and `loomd` reads it at start.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `enabled` | boolean | `false` | Export traces. |
| `otlp_endpoint` | string | none | Full URL of the OTLP/HTTP traces endpoint, such as `http://localhost:4318/v1/traces`. Must start with `http://` or `https://` and have a host. Any other value is rejected with the location of the value. |

```toml
[telemetry]
enabled = true
otlp_endpoint = "http://localhost:4318/v1/traces"
```

Traces are exported only when `enabled = true` and `OTEL_SDK_DISABLED` is not `true` (any case). Otherwise no exporter, HTTP client or thread is created, and no connection is opened.

The traces endpoint is the first of:

| Order | Source | Used as |
| --- | --- | --- |
| 1 | `otlp_endpoint` | Given. |
| 2 | `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | Given. |
| 3 | `OTEL_EXPORTER_OTLP_ENDPOINT` | Followed by `/v1/traces`. |
| 4 | Default | `http://localhost:4318/v1/traces` |

Empty variables count as unset. Other environment variables: see [environment variables](cli/conventions.md#environment-variables).

Export:

- Protocol: OTLP over HTTP with protobuf bodies (`http/protobuf`). `OTEL_EXPORTER_OTLP_PROTOCOL` is ignored. gRPC is not supported.
- Spans are batched: `loomd` sends a batch every 5 s (`OTEL_BSP_SCHEDULE_DELAY`), and at exit waits at most 5 s for the last one. `loom` sends its spans when the command ends and waits at most 2 s. Thus a collector that is slow or that `loom` cannot reach delays a command by at most 2 s.
- An export that fails is logged on stderr (for `loomd` started by `loom status`, in `log/loomd.log`) and the spans are dropped. It never changes the exit code.
- Resource:
  - `service.name` is `loom` or `loomd`, unless `OTEL_SERVICE_NAME` or `service.name` in `OTEL_RESOURCE_ATTRIBUTES` sets it.
  - `service.version` is the build version.
  - `loom.channel` is the build channel (`stable`, `dev`, `local`).

Spans:

| Process | Span | Kind | Attributes |
| --- | --- | --- | --- |
| `loom` | One root span per command, named after the command path: `loom version`, `loom status`, `loom service install`. | internal | `loom.context` when `-c` is given. Error status and message when the command fails. |
| `loom` | `GET <path>`: each request to `loomd`, child of the command span. | client | `http.request.method`, `url.path`, `http.response.status_code`. Error status when the request fails. |
| `loomd` | `<METHOD> <route>` (`GET /v1/status`), or `<METHOD>` alone when no route matches: each API request. | server | `http.request.method`, `url.path`, `http.route`, `http.response.status_code`. Error status on a 5xx. |
| `loomd` | `update.check`: root span of each update check. | internal | `update.channel`, `update.current`, `update.version` (version found), `update.outcome`: `up_to_date`, `available`, `skipped_failed`, `upgrade_in_progress`, `swapped`, `preflight_failed`, `failed`. Error status on `preflight_failed` and `failed`. |
| `loom`, `loomd` | `update.decide`: the update decision, child of `update.check` (or of the `loom upgrade` command span). | internal | `current`, `channel`, `mode`, `pinned`, `releases`, `failed_versions`. |
| `loom`, `loomd` | `update.download`: download, verification and extraction of a release, child of `update.check` (or of the `loom upgrade` command span). | internal | `update.version`, `update.target`. Error status when it fails. |
| `loom`, `loomd` | `update.verify`: one per verification, child of `update.download`. | internal | `update.verify.what`: `signature`, `checksum`, `version`. Error status when it fails. |
| `loomd` | `update.preflight`: the pre-flight check, child of `update.check`. | internal | `update.version`, `update.outcome`: `passed`, `failed`. |
| `loomd` | `update.swap`: replacement of the binaries and start of the watchdog, child of `update.check`. | internal | `update.from`, `update.to`, `update.outcome`: `swapped`, `failed`. |
| `loom` | `update.watchdog`: the watchdog, child of the `loom __upgrade-watchdog` command span. | internal | `update.from`, `update.to`, `update.outcome`: `upgraded`, `rolled_back`, `rollback_failed`. Error status unless `upgraded`. |

`loom` sends the [W3C trace context](https://www.w3.org/TR/trace-context/) (`traceparent`, `tracestate` headers) with each request. The request span of `loomd` is a child of the client span, in the same trace. Spans at `info` level are exported. `-v` and `-vv` add `debug` and `trace` spans. `-q` does not reduce what is exported.

## `index.db`

Index of the git repositories, in `<loom home>/index.db`: SQLite, machine-local, written by `loomd` only and read through the [API](api.md#repositories).

| Path | Mode | Content |
| --- | --- | --- |
| `index.db` | `0600` | SQLite database in WAL mode. If the file does not exist, `loomd` creates it when it starts. At each start, `loomd` sets the file to `0600`. |
| `index.db-wal`, `index.db-shm` | `0600` | Write-ahead log and its shared-memory index, created by SQLite while `loomd` runs. |

When it starts, `loomd` upgrades the schema of `index.db` in one transaction (the version is `PRAGMA user_version`). `loomd` never downgrades a database that a newer loom wrote. It exits with code 1:

```text
Error: cannot start the repository index
  caused by: the index /home/me/.loom/index.db has schema version 2, this loom supports up to 1: upgrade loom, or stop loomd and delete the file to rebuild it
```

`loomd` then scans the repositories (see [`POST /v1/repos/scan`](api.md#repositories)) and serves the API during that scan. After the scan, it reads the state of every repository in the background, then again every `[repos] fetch_interval` (see [state refresh](api.md#state-refresh)). It also updates the repositories on their schedule (see [updates](api.md#updates)). The index keeps the result of the last update of each repository across restarts.

`loom repo scan --rebuild` rebuilds the index from a scan of the roots while `loomd` runs (see [`loom repo scan`](cli/conventions.md#loom-repo-scan)). It can also be rebuilt by hand: stop `loomd`, delete `index.db`, `index.db-wal` and `index.db-shm`, start `loomd`. After a rebuild:

- The registered repositories outside the scan are lost.
- The slugs are assigned again.
- The states are read again.
- The results of the last updates are lost: every repository is then updated at once.

The index also holds the tasks of the [context stores](#context-stores), read from their files. `loomd` reconciles them when it starts and when their files change. While `loomd` runs, `loom reindex` ([`POST /v1/tasks/reindex`](api.md#tasks)) rebuilds them from the files and does not change the repositories. If `index.db` is deleted, the next start gives back the same task rows.

## `machine.toml`

Path: `<loom home>/machine.toml`, mode `0600`. Local to the machine and not meant to be versioned, unlike `config.toml`, which can be the same on every machine.

```toml
id = "laptop"
```

| Key | Type | Constraints | Effect |
| --- | --- | --- | --- |
| `id` | string | Required. 1 to 64 bytes, no whitespace or control characters. | Name of the machine: matched against `[contexts.<name>] machines`, and written as the `machine_id` of every [event](task-file.md#common-fields) `loomd` records. |

If the file does not exist, `loomd` creates it at start with the host name and logs `machine named after the host`. `loomd` removes whitespace and control characters from the host name and cuts it to 64 bytes. If nothing is left, the name is `unknown`. The file is read once, when `loomd` starts. These errors stop `loomd` with code 1: an unknown key, no `id`, an invalid `id`, or a TOML syntax error. For example:

```text
Error: invalid configuration /home/me/.loom/machine.toml:1:1: unknown field `name`, expected `id`
```

## Context stores

Each context of `config.toml` held by this machine (see [`machines`](#contexts-and-repositories)) has a task store: `[contexts.<name>] store.path`, by default `<loom home>/contexts/<name>`. `loomd` creates `<store>/tasks/` and `<store>/archive/` (mode `0700`) when it starts. If `loomd` cannot create a store, it stops with code 1.

| Path | Mode | Content |
| --- | --- | --- |
| `tasks/<id>/` | `0700` | One directory per task, named by its ID (8 lowercase Crockford base32 characters). Other names in `tasks/` are ignored. |
| `tasks/<id>/task.md` | `0600` | The task: TOML frontmatter between `+++` lines, then free notes ([format](task-file.md)). Replaced atomically: written to `.task.md.tmp` beside it, synced, renamed over it, and the directory synced. |
| `tasks/<id>/events.jsonl` | `0600` | The task's events, one JSON object per line, appended and synced, never rewritten. If a crash cuts the last line, `loomd` adds a line ending to it before the next append. |
| `tasks/.<id>.tmp/` | `0700` | A task during its creation: written whole, then renamed to `tasks/<id>/`. A leftover of a crash is ignored, and replaced when the same ID is created again. |
| `archive/<yyyy>/<id>/` | `0700` | An archived task, with the same two files. `<yyyy>` is the UTC year of its close time. Read-only ([task files](task-file.md)). Other names in `archive/` and in its years are ignored. |

`loomd` watches `tasks/` and `archive/` of every store (FSEvents on macOS, inotify on Linux) and reconciles each task whose files change, and every task when it starts:

| Change | Result |
| --- | --- |
| `task.md` as `loomd` last read or wrote it (same SHA-256) | Nothing. |
| A field of the frontmatter differs from the task its `events.jsonl` describes | `loomd` appends an `edited` event with the actor `{"type":"file"}` and each changed field (`title`, `kind`, `status`, `importance`, `due`, `pinned`, `links`, `relations`). The task is indexed. `task.md` is not written. |
| Only the notes changed, or `events.jsonl` has no `created` event, a line of a newer version, or changes that do not follow from each other | The task is indexed, no event. |
| `task.md` is invalid | The task is indexed read-only, with the error and its line. No event. The file is never written again. |
| The task directory moved between `tasks/` and `archive/<yyyy>/`, with the same `task.md` | The task is indexed at its new location. No event. |
| An archive year directory `archive/<yyyy>/` is created or removed | `loomd` reconciles all the tasks, one time for a series of these changes. On Linux, the watcher gets no event for the tasks in a new directory. |
| `tasks/` or `archive/` is removed, then created again | The watcher does not watch the new directory. `loomd` sees its changes at the next start, or with `loom reindex`. |
| The task directory or its `task.md` is gone | The task leaves the index. |
| A file cannot be read (permissions, I/O), or `tasks/` cannot be listed | Logged with its path. The task keeps its index entry. The other tasks are reconciled. |

When the watcher cannot start, `loomd` logs a warning and reads the hand edits at its next start. When it reports lost events, every task is reconciled.

### Archive

`loomd` moves the closed tasks to the archive when it starts, after the reconciliation on start. Then it does this every hour. A `done` or `cancelled` task of `tasks/` moves to `archive/<yyyy>/<id>/` when its close time is at least `[tasks] archive_after` old. Thus `loomd` archives a task between `archive_after` and `archive_after` plus one hour after its close time.

No value disables the archive. To keep all the tasks in `tasks/`, set a very long duration, for example `"36500d"`.

| Rule | Value |
| --- | --- |
| Close time | The time of the last event in `events.jsonl` that set the current status of the task (`status_changed`, `edited` by hand, `undone`, or `created` with a closed status). A note or an edit after it does not delay the archive. |
| No event that closed the task (a lost log) | Never archived automatically. |
| `<yyyy>` | The year of the close time, in UTC. |
| Move | One `rename` of the task directory under its lock, then the index. After a crash, the task is in one location, and `loomd` indexes it there at the next start. |
| `archive/<yyyy>/<id>/` exists | No move. `loomd` logs a warning, and the task stays in `tasks/`. `loomd` deletes nothing. |
| A move that fails | `loomd` logs a warning. The task stays in `tasks/`, and the next pass tries again. |
| More than 50 tasks to move | A pass moves 50 tasks. The next pass starts when the requests that wait have run. Thus a large backlog does not block the API for a long time. |
| Debug build with `LOOM_TEST_TODAY=YYYY-MM-DD` | The pass runs as at `00:00:00Z` on that date (tests). |

After `loomd` starts, reads (`GET /v1/tasks`, `loom ls`, `loom show`) wait for the reconciliation and for the first pass (its first 50 moves).

### Store repository

Each store is a git repository. `loomd` creates it at its first commit: when the store first changes, or at start when `tasks/` or `archive/` holds an entry. `loomd` runs `git init --initial-branch=main` in the store root and writes these files if they do not exist:

| Path | Content |
| --- | --- |
| `.gitattributes` | `*.jsonl merge=union` |
| `.gitignore` | `.*.tmp` (the temporary files of atomic writes and creations) |

`loomd` commits each change through the `git` CLI immediately after it writes the change:

| Change | Staged | Commit message |
| --- | --- | --- |
| A task created, edited, changed by an action (status, pin, note) or undone through the [API](api.md#tasks) | `tasks/<id>/` | `<event type>(<id>): <summary>` of its first event, such as `created(k3q7zm2a): Ship it`, `status_changed(k3q7zm2a): ready -> active`, `note_logged(k3q7zm2a): Asked Alice`, `undone(k3q7zm2a): event e0000003`; `(+<n> events)` when it recorded more. A request that records no event commits nothing. |
| A hand edit recorded as an `edited` event | `tasks/<id>/`. For an archived task: `archive/<yyyy>/<id>/` and the removal of `tasks/<id>/` | `edited(<id>): <fields>`, such as `edited(k3q7zm2a): title, due`; `(+1 events)` when the same save also changed the notes |
| A hand edit of the notes only | The same paths as an `edited` event | `notes_changed(<id>): notes` |
| A hand edit that records no event (a new or invalid file, a move to the archive, notes with no known previous value) | The same paths as an `edited` event | `edited(<id>): changed by hand` |
| A closed task moved to the archive by `loomd` | `archive/<yyyy>/<id>/` and the removal of `tasks/<id>/` | `archived(<id>): moved to archive/<yyyy>` |
| A task directory removed | the whole store | `removed(<id>): task directory removed` |
| The end of a reconciliation of every task (start, lost watcher events, `loom reindex`) | the whole store | `sync(<context>): uncommitted changes` |

A message is `<type>(<id>): <summary>`, the summary on one line, cut to 60 characters with `…`. If nothing is staged, there is no commit.

Every git command of a store runs in its root with `-c commit.gpgsign=false -c tag.gpgsign=false -c core.hooksPath=/dev/null`. Thus store commits are never signed and run no hook, whatever the configuration says. This applies to the `.git/hooks` of the store and to a `core.hooksPath` of the user's configuration. The rest of the user's configuration applies: identity, `init.templateDir`.

The commands run without a terminal (`GIT_TERMINAL_PROMPT=0`, no standard input). They never look for a repository above the store root (`GIT_CEILING_DIRECTORIES`). After 60 seconds, `loomd` stops them with their process group.

`loomd` logs a commit that fails (no identity, a lock held by another git process) as a warning. The change stays written, and the next commit of that store stages the whole store.

### Store backup

A store with `[contexts.<name>] store.backup.remote` is synced with that repository when `loomd` starts, then every `[tasks] sync_interval`, and on [`loom sync`](cli/conventions.md#loom-sync). A sync of a store:

1. Creates its repository if it has none, without a commit, so that the remote can fill an empty store. Then commits every change not committed yet.
2. Points the remote `backup` of the store repository at `store.backup.remote` (`git remote add` or `set-url`).
3. Fetches it: `git fetch --prune --no-tags backup`.
4. Compares `HEAD` with `backup/main`, then:

| Store | Remote | Action |
| --- | --- | --- |
| no new commit | no new commit | Nothing (`up to date`). |
| new commits | no new commit | `git push backup HEAD:refs/heads/main`, never forced. |
| no new commit | new commits | `git merge --ff-only --no-autostash <commit>`, only when `git status --porcelain` is empty. |
| new commits | new commits | Nothing: the sync of that store stops (`diverged`), and `loom repo doctor` fails on it. |

A sync never merges, rebases, resets or forces. If the remote refuses a push, the commits stay local. `loomd` logs a failed step, and [`GET /v1/tasks/backup`](api.md#tasks) and `loom repo doctor` report it. The other stores are synced.

The fetch and the push use the environment of the [repository updates](#contexts-and-repositories):

- `GIT_TERMINAL_PROMPT=0`.
- SSH `BatchMode`, unless `core.sshCommand` or `GIT_SSH_COMMAND` is set.

`loomd` stops the fetch and the push after 120 s, with their process group. It also stops them when it shuts down: the process group receives `SIGTERM`, and `SIGKILL` 5 s later. The sync thread ends before `loomd` exits, no git process of a sync stays after `loomd`, and no other store is synced after the shutdown began. See [exit codes](cli/conventions.md#exit-codes).

The fetch and the push run on their own thread. The commit and the fast-forward run between the writes of the task store. A fast-forward changes task files: they are reconciled and indexed like hand edits that record no event.

## `run/`

Runtime files of `loomd`, in `<loom home>/run/`.

| Path | Mode | Content |
| --- | --- | --- |
| `run/` | `0700` | Created if it does not exist. At start, `loomd` sets the directory to `0700`. |
| `run/loomd.lock` | `0600` | Single-instance lock. Holds the PID of the `loomd` that runs, followed by a newline. Kept after `loomd` stops. |
| `run/loomd.sock` | `0600` | Unix socket of the [API](api.md). Removed when `loomd` stops gracefully. |

### Single instance

At start, `loomd` takes an exclusive lock on `run/loomd.lock` (`flock`), held until the process exits. If another process holds it, `loomd` exits with code 1:

```text
Error: another loomd is running (PID 4242): /home/me/.loom/run/loomd.lock is locked
```

The `(PID …)` part is omitted when the lock file holds no PID.

### Stale socket

When `loomd` holds the lock, a file at `run/loomd.sock` comes from a daemon that did not stop gracefully (killed, crashed). `loomd` removes this file before it listens, and logs `removed stale socket` at info level.

### Socket path length

A unix socket path is limited to 107 bytes on Linux and 103 bytes on macOS. When `<loom home>/run/loomd.sock` is longer, `loomd` exits with code 1 and a message that names the path. Use a shorter loom home. `loom status` checks the same limit before it connects, and fails the same way.

## `log/`

Log of a `loomd` started by `loom status`, in `<loom home>/log/`.

| Path | Mode | Content |
| --- | --- | --- |
| `log/` | `0700` | Created by `loom status` before it starts `loomd`. If the directory exists, it is set to `0700`. |
| `log/loomd.log` | `0600` | stderr of every `loomd` that `loom status` started, appended across starts, never rotated or truncated. |

A `loomd` started another way (by hand, by a service manager) logs to its own stderr, not to this file.

The auto-upgrade watchdog appends its stderr to `log/loomd.log` too, and the `loomd` it starts writes there like one started by `loom status`.

## `versions/`

Previous versions of `loom` and `loomd`, kept by `loom upgrade` and `loom rollback`.

| Path | Mode | Content |
| --- | --- | --- |
| `versions/` | `0700` | Created at the first upgrade or rollback. |
| `versions/<version>/loom` | as installed | Copy of the `loom` replaced. |
| `versions/<version>/loomd` | as installed | Copy of the `loomd` replaced; absent when none was installed. |

- Before the binaries are replaced, the current version is copied into `versions/<version>/`. If a save of that version exists, the copy replaces it.
- After the binaries are replaced, two previous versions are kept: the one just saved, plus the highest others by semver precedence, two in all.
- The directory of the newly installed version can exist after `loom rollback`, or after an upgrade back to a version saved earlier. It duplicates the current binaries and is deleted. Exception: if a build of the same version (`--from`) replaced the binaries, this directory is the one just saved, and it is kept.
- Other directories are deleted.
- `loom rollback` restores the highest version other than the current one, among the directories that hold both binaries.

The auto-upgrade of `loomd` saves and prunes `versions/` the same way. The watchdog restores `versions/<previous version>/` and does not save the failed version. Then it deletes `versions/<previous version>/`, because it duplicates the current binaries again. It keeps the two highest other versions.

## `staging/`

Downloads that `loom upgrade` and the auto-upgrade of `loomd` verify and unpack, in `staging/upgrade-<random>/` (mode `0700`). Each directory is removed when the command or the upgrade ends, successful or not. A process killed by a signal can leave one directory. It is safe to delete it.

The pre-flight check of the auto-upgrade runs the downloaded `loomd` with a temporary loom home in `/tmp/loom-preflight-<random>/`. If `/tmp` does not exist, this directory is in the system temporary directory. It is removed when the check ends.

## `state/`

State kept across restarts of `loomd`, in `<loom home>/state/`.

| Path | Mode | Content |
| --- | --- | --- |
| `state/` | `0700` | Created at the first write. At each write, the directory is set to `0700`. |
| `state/update.json` | `0600` | Auto-upgrade state, JSON, schema [`update_state.json`](schemas/update_state.json). |
| `state/update.lock` | `0600` | Empty. Locked (`flock`) while `update.json` is read, changed and written. |

`update.json` is written to a temporary file in `state/`, synced, then renamed over the previous one. If the file does not exist, the state is empty. An invalid file is logged and replaced by an empty state at the next write. Fields this version does not know (written by another version) are accepted and kept when the file is rewritten.

| Field | Type | Content |
| --- | --- | --- |
| `channel` | object or absent | Channel chosen explicitly: `name` (`stable` or `dev`) and `chosen_at` (`Timestamp`). Written by `loom upgrade --channel` when it succeeds, `--check` included, `--dry-run` excluded. `--version` and `--from` leave it unchanged. Used when `[update] channel` is not set (see [channel resolution](#channel-resolution)). |
| `last_check` | `Timestamp` or absent | Instant of the last check, successful or not. |
| `available` | version or absent | Newer version found by the last check under `notify`. |
| `pending` | object or absent | Upgrade in progress: `from`, `to` (versions), `started_at` (`Timestamp`). Set when `loomd` starts the download, cleared by every outcome. Older than one hour: stale, cleared at the next check and recorded as `interrupted`. |
| `failed_versions` | list of versions | Versions that failed the pre-flight check or the watchdog. Never applied automatically again. |
| `last_result` | object or absent | Outcome of the last check or upgrade, tagged by `kind`, with `at` (`Timestamp`). |

`last_result` kinds:

| `kind` | Other fields | Meaning |
| --- | --- | --- |
| `up_to_date` | | No newer version. |
| `available` | `version` | Newer version recorded (`notify`). |
| `skipped_failed` | `version` | Every newer version is in `failed_versions`. |
| `upgraded` | `from`, `to` | The new `loomd` answered the watchdog. |
| `preflight_failed` | `version`, `message` | The downloaded `loomd` failed the pre-flight check. Nothing installed. |
| `rolled_back` | `from`, `to`, `message` | The new `loomd` did not answer the watchdog. `from` restored, `to` added to `failed_versions`. |
| `interrupted` | `from`, `to` | A stale `pending` was cleared. |
| `failed` | `message` | The check or the upgrade failed before it replaced anything (network, verification, install directory not writable, I/O). Retried at the next interval. |

Delete `state/update.json` to forget the failed versions and the chosen channel.
