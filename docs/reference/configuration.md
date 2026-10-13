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

`loomd` creates the loom home if it does not exist (mode `0700`); `loom upgrade` and `loom rollback` create it the same way when they first write to it.

## `config.toml`

Path: `<loom home>/config.toml`. Format: [TOML](https://toml.io/en/v1.1.0).

| Case | Behavior |
| --- | --- |
| File missing | Defaults. |
| File empty or comments only | Defaults. |
| Unknown key or table, at any level | Rejected. |
| Value of the wrong type | Rejected. |
| TOML syntax error | Rejected. |

A rejected file stops `loomd` with exit code 1 before it creates anything, and this message on stderr:

```text
Error: invalid configuration <path>:<line>:<column>: <message>
```

`line` and `column` start at 1; `column` counts characters. For an unknown key, `message` names it:

```text
Error: invalid configuration /home/me/.loom/config.toml:3:1: unknown field `foo`, expected one of `contexts`, `daemon`, `default_context`, `forges`, `repos`, `telemetry`, `update`
```

`loom` reads `config.toml` at each command for `[telemetry]`. A file it cannot read or parse does not stop a command for that reason: `loom` exports no traces, prints this warning on stderr (hidden by `-q`) and runs the command. Commands that need the file for something else, such as `loom service install`, still fail on it. `loom upgrade` reads `[update] channel` the same way: a file it cannot read or parse leaves the key unset, with a warning, so that an install can still be repaired.

```text
 WARN traces are not exported: invalid configuration <path>:<line>:<column>: <message>
```

### Keys

| Key | Type | Default | Effect |
| --- | --- | --- | --- |
| `default_context` | string: context name | none | Context of the repositories no other rule matches. See [contexts and repositories](#contexts-and-repositories). |
| `[contexts.<name>] match.paths`, `match.hosts`, `update`, `store.path` | | | See [contexts and repositories](#contexts-and-repositories). |
| `[repos] roots`, `max_depth`, `exclude`, `fetch_interval`, `fetch_concurrency` | | | See [contexts and repositories](#contexts-and-repositories). |
| `[repos."<path>"] context`, `update` | | | See [contexts and repositories](#contexts-and-repositories). |
| `[forges."<host>"] kind` | | | See [contexts and repositories](#contexts-and-repositories). |
| `[daemon] service` | boolean | `true` | `false`: `loom service install` refuses with exit code 1 and a message naming this key and the file. `loom service uninstall` and `loom service status` still work. Does not stop or remove an installed service. |
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

`default_context`, `[contexts]`, `[repos]` and `[forges]` are read and validated with the rest of `config.toml`: an invalid value stops `loomd` and is reported by `loom` like any other error in the file. `loomd` uses them for the [repository index](#indexdb): it reads them once, when it starts, so a change takes effect after a restart; the scan on start then reassigns the contexts (and the slugs) of every repository. `~` is the user's home directory (`HOME`); `loomd` exits with code 1 when it is unknown.

`update`, `fetch_interval` and `fetch_concurrency` drive the automatic updates of `loomd`: a fetch of each repository's primary remote, then fast-forwards only when no local work can be affected ([updates](api.md#updates), [how updates work](../explanation/repo-updates.md)). The update policy of a repository is its `[repos."<path>"] update`, otherwise the `update` of its context, otherwise `ff`. A repository without a context (`unassigned`) and without `[repos."<path>"] update` gets `fetch`: fetched, never fast-forwarded. An explicit `[repos."<path>"] update` always applies, `ff` included.

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
| `[contexts.<name>]` | table | none | `<name>`: 1 to 64 lowercase ASCII letters, digits and `-`, starting with a letter, not ending with `-` (the rule of `-c`). `unassigned` is reserved. A table without keys is valid. |
| `[contexts.<name>] match.paths` | array of strings: paths | none | At least one entry when given. Each entry is absolute, `~` or starts with `~/`; `..` is not allowed. |
| `[contexts.<name>] match.hosts` | array of strings | none | At least one entry when given. Each entry is `host` or `host/path-prefix`: a lowercase host name (ASCII letters, digits, `-`, `.`) without scheme, user or port, then path segments that are not empty and contain no space. A trailing `/` is ignored. |
| `[contexts.<name>] update` | string: `"ff"`, `"fetch"`, `"off"` | none (`ff`) | Update policy of the context's repositories. `ff`: fetch, then fast-forward the branch `HEAD` is on and the default branch when it is safe. `fetch`: fetch only, no branch moves. `off`: no git command. |
| `[contexts.<name>] store.path` | string: path | `<loom home>/contexts/<name>` | Root of the context's task store. Same path rules as `match.paths`; `~` is the user's home, not the loom home. Two contexts cannot have the same `store.path`. |
| `[repos] roots` | array of strings: paths | `["~/perso", "~/work"]` | Same path rules as `match.paths`. May be empty. |
| `[repos] max_depth` | integer | `4` | 0 to 4294967295. Deepest directory level examined below each root, the root being level 0. |
| `[repos] exclude` | array of strings: glob patterns | `[]` | Each pattern is not empty and is a valid glob (`*`, `?`, `**`, `[…]`, `{a,b}`). A pattern without `/` matches a directory name at any depth (`archive`, `*-old`); a pattern with `/` matches the whole directory path relative to its root, with `/` separators (`clients/old`, `**/fixtures`). A matching directory is not walked. Hidden directories and `node_modules`, `vendor`, `target`, `dist`, `build`, `.venv`, `venv`, `__pycache__` are always excluded. |
| `[repos] fetch_interval` | string: duration | `"1h"` | Duration syntax of `[update] check_interval`. At least `"1m"`. Time between two updates of a repository, ±10 % at random, and between two refreshes of every repository state. A failed fetch waits 2, 4, then 8 times as long. |
| `[repos] fetch_concurrency` | integer | `4` | 1 to 4294967295. Updates running at the same time; above 64, 64. |
| `[repos."<path>"]` | table | none | `<path>`: same path rules as `match.paths`. Two keys naming the same path (`"~/a"` and `"~/a/"`) are rejected. |
| `[repos."<path>"] context` | string | none | Names a defined context. |
| `[repos."<path>"] update` | string: `"ff"`, `"fetch"`, `"off"` | none | Update policy of this repository, over its context's, and over the `fetch` default of an `unassigned` repository. |
| `[forges."<host>"]` | table | none | `<host>`: a lowercase host name (ASCII letters, digits, `-`, `.`), without port or path. |
| `[forges."<host>"] kind` | string: `"github"`, `"gitlab"` | required | Forge of the host. |

Paths are compared as written, after removing `.` components and repeated or trailing `/`; `~` is kept unexpanded for this check.

The same `match.paths` entry in two contexts is rejected, and so is the same `match.hosts` entry (path segments compared without ASCII case) and the same `store.path`. The error is at the second entry and names the location of the first:

```text
Error: invalid configuration /home/me/.loom/config.toml:5:16: match.paths `~/work/` of context `company-b` is also `~/work` of context `company-a` (line 2, column 16): an entry can belong to one context only
```

Errors in an array element (`roots`, `exclude`, `match.paths`, `match.hosts`) are located at the start of the array.

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

Schedule: the first check runs `min(5 min, check_interval)` ± 10 % after `loomd` starts, never at once; each next one `check_interval` ± 10 % after the last check (`last_check` of `state/update.json`), but never before that first delay. A failed check is retried at the next interval.

`loomd` authenticates with `LOOM_GITHUB_TOKEN` or `GITHUB_TOKEN` when set in its environment; it never runs `gh`.

## User service

`loom service install` installs `loomd` as a user service. `loom service` reads `config.toml` only for `[daemon] service`.

| Platform | Manager | File | Logs |
| --- | --- | --- | --- |
| macOS | launchd user agent, label `io.github.gsuquet.loomd`, domain `gui/<uid>` | `~/Library/LaunchAgents/io.github.gsuquet.loomd.plist` | `<loom home>/log/loomd.log` (stdout and stderr) |
| Linux | systemd user unit `loomd.service` | `$XDG_CONFIG_HOME/systemd/user/loomd.service` when `XDG_CONFIG_HOME` is an absolute path, otherwise `~/.config/systemd/user/loomd.service` | systemd journal: `journalctl --user -u loomd.service` |

The service runs the `loomd` found by `loom service install`: the `loomd` next to the `loom` executable (symbolic links to `loom` resolved), otherwise the first `loomd` file in a `PATH` directory. Its path is written canonicalized (absolute, symbolic links resolved). The resolved loom home is passed as `LOOM_HOME`.

Property list content (macOS): `ProgramArguments` = [`loomd` path], `EnvironmentVariables` = `LOOM_HOME`, `RunAtLoad` true, `KeepAlive` true, `ProcessType` `Background`, `StandardOutPath` and `StandardErrorPath` = `<loom home>/log/loomd.log`. Values are XML-escaped.

Unit content (Linux). `KillMode=process` makes systemd stop only `loomd` when it exits, not the watchdog it starts after an auto-upgrade. `RestartSec=1s` restarts a failing `loomd` every second; systemd's default start limit (5 starts in 10 s) still stops a crash loop, and the watchdog clears it after a rollback. A unit written before either line was added is updated by running `loom service install` again.

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

Values are double-quoted; `\` and `"` are backslash-escaped, `%` is written `%%`, and `$` in `ExecStart` is written `$$`.

A `loomd` path or loom home that is not valid UTF-8 or contains a control character is refused with exit code 1. The file is rewritten by `install` whenever its content differs from the generated one: hand edits are overwritten.

The auto-upgrade watchdog treats `loomd` as supervised when the service file exists, sets `LOOM_HOME` to the watchdog's loom home, and the service is loaded (macOS: `launchctl print gui/<uid>/io.github.gsuquet.loomd` succeeds; Linux: `systemctl --user is-enabled loomd.service` succeeds). It then never starts `loomd` itself, and after a rollback runs `launchctl kickstart -k gui/<uid>/io.github.gsuquet.loomd` (macOS) or `systemctl --user reset-failed loomd.service` then `systemctl --user restart loomd.service` (Linux). Otherwise it starts `loomd` detached. See [how loomd upgrades itself](../explanation/auto-upgrade.md).

## `[telemetry]`

Export of traces over OTLP, for diagnosing loom itself. Read by `loom` at each command and by `loomd` at start.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `enabled` | boolean | `false` | Export traces. |
| `otlp_endpoint` | string | none | Full URL of the OTLP/HTTP traces endpoint, such as `http://localhost:4318/v1/traces`. Must start with `http://` or `https://` and have a host; anything else is rejected with the location of the value. |

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

- Protocol: OTLP over HTTP with protobuf bodies (`http/protobuf`). `OTEL_EXPORTER_OTLP_PROTOCOL` is ignored; gRPC is not supported.
- Spans are batched: `loomd` sends a batch every 5 s (`OTEL_BSP_SCHEDULE_DELAY`), and at exit waits at most 5 s for the last one. `loom` sends its spans when the command ends and waits at most 2 s, so an unreachable or slow collector delays a command by at most 2 s.
- An export that fails is logged on stderr (for `loomd` started by `loom status`, in `log/loomd.log`) and the spans are dropped. It never changes the exit code.
- Resource: `service.name` is `loom` or `loomd` unless `OTEL_SERVICE_NAME` or `service.name` in `OTEL_RESOURCE_ATTRIBUTES` sets it; `service.version` is the build version; `loom.channel` is the build channel (`stable`, `dev`, `local`).

Spans:

| Process | Span | Kind | Attributes |
| --- | --- | --- | --- |
| `loom` | One root span per command, named after the command path: `loom version`, `loom status`, `loom service install`. | internal | `loom.context` when `-c` is given. Error status and message when the command fails. |
| `loom` | `GET <path>`: each request to `loomd`, child of the command span. | client | `http.request.method`, `url.path`, `http.response.status_code`. Error status when the request fails. |
| `loomd` | `<METHOD> <route>` (`GET /v1/status`), or `<METHOD>` alone when no route matches: each API request. | server | `http.request.method`, `url.path`, `http.route`, `http.response.status_code`. Error status on a 5xx. |
| `loomd` | `update.check`: root span of each update check. | internal | `update.channel`, `update.current`, `update.version` (version found), `update.outcome`: `up_to_date`, `available`, `skipped_failed`, `upgrade_in_progress`, `swapped`, `preflight_failed`, `failed`. Error status on `preflight_failed` and `failed`. |
| `loom`, `loomd` | `update.decide`: the update decision, child of `update.check` (or of the `loom upgrade` command span). | internal | `current`, `channel`, `mode`, `pinned`, `releases`, `failed_versions`. |
| `loom`, `loomd` | `update.download`: download, verification and unpacking of a release, child of `update.check` (or of the `loom upgrade` command span). | internal | `update.version`, `update.target`. Error status when it fails. |
| `loom`, `loomd` | `update.verify`: one per verification, child of `update.download`. | internal | `update.verify.what`: `signature`, `checksum`, `version`. Error status when it fails. |
| `loomd` | `update.preflight`: the pre-flight check, child of `update.check`. | internal | `update.version`, `update.outcome`: `passed`, `failed`. |
| `loomd` | `update.swap`: replacement of the binaries and start of the watchdog, child of `update.check`. | internal | `update.from`, `update.to`, `update.outcome`: `swapped`, `failed`. |
| `loom` | `update.watchdog`: the watchdog, child of the `loom __upgrade-watchdog` command span. | internal | `update.from`, `update.to`, `update.outcome`: `upgraded`, `rolled_back`, `rollback_failed`. Error status unless `upgraded`. |

`loom` sends the [W3C trace context](https://www.w3.org/TR/trace-context/) (`traceparent`, `tracestate` headers) with each request; the request span of `loomd` is a child of the client span, in the same trace. Spans at `info` level are exported; `-v` and `-vv` add `debug` and `trace` spans. `-q` does not reduce what is exported.

## `index.db`

Index of the git repositories, in `<loom home>/index.db`: SQLite, machine-local, written by `loomd` only and read through the [API](api.md#repositories).

| Path | Mode | Content |
| --- | --- | --- |
| `index.db` | `0600` | SQLite database in WAL mode. Created when `loomd` starts if missing; an existing file is set to `0600` at each start. |
| `index.db-wal`, `index.db-shm` | `0600` | Write-ahead log and its shared-memory index, created by SQLite while `loomd` runs. |

When it starts, `loomd` upgrades the schema of `index.db` in one transaction (the version is `PRAGMA user_version`). A database written by a newer loom is never downgraded: `loomd` exits with code 1:

```text
Error: cannot start the repository index
  caused by: the index /home/me/.loom/index.db has schema version 2, this loom supports up to 1: upgrade loom, or stop loomd and delete the file to rebuild it
```

`loomd` then scans the repositories (see [`POST /v1/repos/scan`](api.md#repositories)) and serves the API during that scan. After the scan, it reads the state of every repository in the background, then again every `[repos] fetch_interval` (see [state refresh](api.md#state-refresh)), and updates the repositories on their schedule (see [updates](api.md#updates)). The result of the last update of each repository is kept in the index, across restarts.

`loom repo scan --rebuild` rebuilds the index from a scan of the roots while `loomd` runs (see [`loom repo scan`](cli/conventions.md#loom-repo-scan)). It can also be rebuilt by hand: stop `loomd`, delete `index.db`, `index.db-wal` and `index.db-shm`, start `loomd`. Registered repositories outside the scan are lost, slugs are assigned again, states are read again, and the results of the last updates are lost (every repository is then updated at once).

The index also holds the tasks of the [context stores](#context-stores), read from their files: `loomd` reconciles them when it starts and as their files change, and `loom reindex` ([`POST /v1/tasks/reindex`](api.md#tasks)) rebuilds them from the files while it runs, without touching the repositories. A deleted `index.db` gets the same task rows back at the next start.

## Context stores

Each context of `config.toml` has a task store: `[contexts.<name>] store.path`, by default `<loom home>/contexts/<name>`. `loomd` creates `<store>/tasks/` (mode `0700`) when it starts; a store that cannot be created stops it with code 1.

| Path | Mode | Content |
| --- | --- | --- |
| `tasks/<id>/` | `0700` | One directory per task, named by its ID (8 lowercase Crockford base32 characters). Other names in `tasks/` are ignored. |
| `tasks/<id>/task.md` | `0600` | The task: TOML frontmatter between `+++` lines, then free notes ([format](task-file.md)). Replaced atomically: written to `.task.md.tmp` beside it, synced, renamed over it, and the directory synced. |
| `tasks/<id>/events.jsonl` | `0600` | The task's events, one JSON object per line, appended and synced, never rewritten. A last line cut by a crash is closed by a line ending before the next append. |
| `tasks/.<id>.tmp/` | `0700` | A task being created: written whole, then renamed to `tasks/<id>/`. A leftover of a crash is ignored, and replaced when the same ID is created again. |

`loomd` watches `tasks/` of every store (FSEvents on macOS, inotify on Linux) and reconciles each task whose files change, and every task when it starts:

| Change | Result |
| --- | --- |
| `task.md` as `loomd` last read or wrote it (same SHA-256) | Nothing. |
| A field of the frontmatter differs from the task its `events.jsonl` describes | An `edited` event with the actor `{"type":"file"}` is appended, with each changed field (`title`, `kind`, `status`, `importance`, `due`, `pinned`, `links`, `relations`); the task is indexed. `task.md` is not written. |
| Only the notes changed, or `events.jsonl` has no `created` event, a line of a newer version, or changes that do not follow from each other | The task is indexed, no event. |
| `task.md` is invalid | The task is indexed read-only with the error and its line; no event; the file is never rewritten. |
| The task directory or its `task.md` is gone | The task leaves the index. |
| A file cannot be read (permissions, I/O), or `tasks/` cannot be listed | Logged with its path; the task keeps its index entry; the other tasks are reconciled. |

When the watcher cannot start, `loomd` logs a warning and picks the hand edits up at its next start. When it reports lost events, every task is reconciled.

## `run/`

Runtime files of `loomd`, in `<loom home>/run/`.

| Path | Mode | Content |
| --- | --- | --- |
| `run/` | `0700` | Created if missing; an existing directory is set to `0700` at start. |
| `run/loomd.lock` | `0600` | Single-instance lock. Holds the PID of the running `loomd`, followed by a newline. Kept after `loomd` stops. |
| `run/loomd.sock` | `0600` | Unix socket of the [API](api.md). Removed when `loomd` stops gracefully. |

### Single instance

At start, `loomd` takes an exclusive lock on `run/loomd.lock` (`flock`), held until the process exits. If another process holds it, `loomd` exits with code 1:

```text
Error: another loomd is running (PID 4242): /home/me/.loom/run/loomd.lock is locked
```

The `(PID …)` part is omitted when the lock file holds no PID.

### Stale socket

With the lock held, any existing file at `run/loomd.sock` was left by a daemon that did not stop gracefully (killed, crashed). `loomd` removes it before listening, and logs `removed stale socket` at info level.

### Socket path length

A unix socket path is limited to 107 bytes on Linux and 103 bytes on macOS. When `<loom home>/run/loomd.sock` is longer, `loomd` exits with code 1 and a message naming the path; use a shorter loom home. `loom status` checks the same limit before connecting, and fails the same way.

## `log/`

Log of a `loomd` started by `loom status`, in `<loom home>/log/`.

| Path | Mode | Content |
| --- | --- | --- |
| `log/` | `0700` | Created by `loom status` before it starts `loomd`; an existing directory is set to `0700`. |
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

- Before replacing the binaries, the running version is copied into `versions/<version>/` (an existing save of that version is replaced).
- After replacing them, two previous versions are kept: the one just saved, plus the highest others by semver precedence, two in all. The directory of the newly installed version, when present (after `loom rollback`, or an upgrade back to a version saved earlier), duplicates the running binaries and is deleted; except when it was just saved, because a build of the same version (`--from`) replaced it. Other directories are deleted.
- `loom rollback` restores the highest version other than the running one among the directories holding both binaries.

The auto-upgrade of `loomd` saves and prunes `versions/` the same way. The watchdog restores `versions/<previous version>/` without saving the failed version, then deletes `versions/<previous version>/` (it duplicates the running binaries again) and keeps the two highest other versions.

## `staging/`

Downloads being verified and unpacked by `loom upgrade` and the auto-upgrade of `loomd`, in `staging/upgrade-<random>/` (mode `0700`). Each directory is removed when the command or the upgrade ends, successful or not; a process killed by a signal can leave one behind, which is safe to delete.

The pre-flight check of the auto-upgrade runs the downloaded `loomd` with a throwaway loom home in `/tmp/loom-preflight-<random>/` (the system temporary directory when `/tmp` is missing), removed when the check ends.

## `state/`

State kept across restarts of `loomd`, in `<loom home>/state/`.

| Path | Mode | Content |
| --- | --- | --- |
| `state/` | `0700` | Created at the first write; an existing directory is set to `0700` at each write. |
| `state/update.json` | `0600` | Auto-upgrade state, JSON, schema [`update_state.json`](schemas/update_state.json). |
| `state/update.lock` | `0600` | Empty. Locked (`flock`) while `update.json` is read, changed and written. |

`update.json` is written to a temporary file in `state/`, synced, then renamed over the previous one. A missing file is an empty state. An invalid file is logged and replaced by an empty state at the next write. Fields this version does not know (written by another version) are accepted and kept when the file is rewritten.

| Field | Type | Content |
| --- | --- | --- |
| `channel` | object or absent | Channel chosen explicitly: `name` (`stable` or `dev`) and `chosen_at` (`Timestamp`). Written by `loom upgrade --channel` when it succeeds, `--check` included, `--dry-run` excluded; `--version` and `--from` leave it unchanged. Used when `[update] channel` is not set (see [channel resolution](#channel-resolution)). |
| `last_check` | `Timestamp` or absent | Instant of the last check, successful or not. |
| `available` | version or absent | Newer version found by the last check under `notify`. |
| `pending` | object or absent | Upgrade in progress: `from`, `to` (versions), `started_at` (`Timestamp`). Set when `loomd` starts downloading, cleared by every outcome. Older than one hour: stale, cleared at the next check and recorded as `interrupted`. |
| `failed_versions` | list of versions | Versions that failed the pre-flight check or the watchdog. Never applied automatically again. |
| `last_result` | object or absent | Outcome of the last check or upgrade, tagged by `kind`, with `at` (`Timestamp`). |

`last_result` kinds:

| `kind` | Other fields | Meaning |
| --- | --- | --- |
| `up_to_date` | | No newer version. |
| `available` | `version` | Newer version recorded (`notify`). |
| `skipped_failed` | `version` | Every newer version is in `failed_versions`. |
| `upgraded` | `from`, `to` | The new `loomd` answered the watchdog. |
| `preflight_failed` | `version`, `message` | The downloaded `loomd` failed the pre-flight check; nothing installed. |
| `rolled_back` | `from`, `to`, `message` | The new `loomd` did not answer the watchdog; `from` restored, `to` added to `failed_versions`. |
| `interrupted` | `from`, `to` | A stale `pending` was cleared. |
| `failed` | `message` | The check or the upgrade failed before replacing anything (network, verification, install directory not writable, I/O). Retried at the next interval. |

Delete `state/update.json` to forget the failed versions and the chosen channel.
