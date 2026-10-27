# CLI conventions

> Behavior shared by every command of `loom` and `loomd`, as shipped. Commands and flags: [loom.md](loom.md), [loomd.md](loomd.md) (generated).

## Syntax

- `-h`/`--help` on every binary and command, `-V`/`--version` on every binary.
- Short flags can be grouped: `-yv` is `-y -v`.
- Values are given as `--opt value` or `--opt=value`, `-c value` or `-cvalue`.
- After `--`, no argument is read as an option.
- Global options of `loom` are accepted before or after the command name: `loom -c personal version` and `loom version -c personal` are equivalent.

## Global options

| Option | Behavior |
| --- | --- |
| `-c`, `--context <NAME>` | Context to work in. `NAME`: 1 to 64 characters, lowercase ASCII letters, digits and `-`, first character a letter, last character not `-`. A path (contains `/`, starts with `.` or `~`) or a `key=value` pair is rejected with a usage error. |
| `-q`, `--quiet` | Errors only on stderr. Cannot be combined with `-v`. |
| `-v`, `--verbose` | More diagnostics on stderr. Repeatable: `-vv`. |
| `--color <WHEN>` | `auto` (default): color only when the stream is a terminal and `NO_COLOR` is unset or empty. `always`, `never`: ignore both. |
| `-y`, `--yes` | Answers yes to every confirmation. The only way to skip a confirmation. |

`loomd` accepts `-q`, `-v` and `--color` with the same behavior. Its log levels: `-q` errors only, default info, `-v` debug, `-vv` trace.

## Confirmations

| `-y` | stdin is a terminal | Behavior |
| --- | --- | --- |
| given | any | Proceeds without asking. |
| absent | yes | Asks on the terminal. |
| absent | no | Fails with exit code 1: `confirmation required but stdin is not a terminal; pass -y/--yes to confirm`. |

Shipped on `loom repo scan --rebuild`.

## Dry run

Commands that change state accept `-n`/`--dry-run`: they print on stdout what they would change and change nothing. Read-only queries still run. Shipped on `loom service install`, `loom service uninstall`, `loom upgrade`, `loom rollback`, `loom repo scan`, `loom repo register`, `loom repo forget` and `loom repo sync`.

## Streams

- stdout: the command's data only (human output, or JSON with `--json`).
- stderr: errors, warnings, diagnostics.
- When the reader of stdout closes it (`loom version | head -n 1`), the command ends with exit code 0 and prints nothing on stderr.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success, including `--help` and `--version`. |
| `1` | Error. The message is printed on stderr: `Error: <message>`, then one `  caused by: <cause>` line per underlying cause. |
| `2` | Usage error: unknown option, missing command, invalid value, conflicting options. |
| `130` | Interrupted by `SIGINT` (Ctrl-C). |

If `SIGTERM` stops `loomd` (from a service manager or from `kill`), `loomd` exits with `0` after a graceful shutdown. If `SIGINT` stops `loomd`, it exits with `130` after the same shutdown. After the auto-upgrade replaced the binaries, `loomd` stops by itself with the same graceful shutdown and exits with `0`.

The graceful shutdown has these steps:

1. `loomd` stops to accept requests and waits for the requests in flight, for 10 s at most. A second `SIGTERM` or `SIGINT` ends this wait at once, and `loomd` cuts the requests that still run. The exit code is the one of the first signal.
2. `loomd` stops the git commands of the store sync (`git fetch`, `git push`) and of the remote checks (`git ls-remote`): their whole process group receives `SIGTERM`, and `SIGKILL` 5 s later. No such command starts after this step, and the store sync goes on with no other store.
3. `loomd` waits for the store sync thread, then for the updater thread, 2 s at most for each. Then it stops the task store and the repository index, and waits 2 s at most for the work that a cut request left on a blocking thread. A repository update that runs is not stopped (see [repository updates](../../explanation/repo-updates.md)).

Thus, a `loomd` that stops while a sync or a remote check runs leaves no git process. After `SIGTERM`, git removes its lock files. After `SIGKILL`, it cannot: a lock file can stay in the store repository, and the next commit of the store reports it.

## Environment variables

| Variable | Effect |
| --- | --- |
| `NO_COLOR` | Non-empty: no color with `--color auto`. |
| `LOOM_CONTEXT` | Non-empty: context of `loom add` and `loom ls`, after `-c` and before the current directory. An invalid name is an error. See [task commands](#task-commands). |
| `LOOM_HOME` | Non-empty: loom home directory of `loomd`, unless `--loom-home` is given, and of `loom`. See [configuration](../configuration.md). |
| `PATH` | Searched for `loomd` by `loom status`, `loom service install` and the `loom repo` commands when no `loomd` is next to the `loom` executable. |
| `PWD` | The current directory of `loom repo`, of the context of `loom add` and `loom ls`, and of the registration on use, when it names the same directory as the working directory (symbolic links kept). See [`loom repo`](#repository-argument). |
| `XDG_CONFIG_HOME` | Absolute path: `loom service` on Linux writes the systemd unit under `$XDG_CONFIG_HOME/systemd/user/` instead of `~/.config/systemd/user/`. |
| `LOOM_GITHUB_TOKEN`, `GITHUB_TOKEN` | Optional GitHub token for `loom upgrade`, first non-empty one wins. Raises the API rate limit. Never printed. |
| `LOOM_RELEASES_URL` | Replaces `https://api.github.com` in `loom upgrade` (test hook). Plain `http://` is then allowed. |
| `LOOM_UPDATE_PUBKEY` | Test hook, debug builds only: minisign public key (base64 key line) replacing the embedded release key in `loom upgrade` and the auto-upgrade of `loomd`. Ignored by release builds. |
| `LOOM_UPDATE_CHECK_INTERVAL` | Test hook, debug builds only: duration (`"1s"`) replacing `[update] check_interval` in `loomd`, without its 10 min minimum. An invalid value is logged and ignored. Ignored by release builds. |
| `LOOM_UPDATE_HEALTH_DEADLINE` | Test hook, debug builds only: duration replacing the 60 s deadline of the auto-upgrade watchdog. Ignored by release builds. |
| `LOOM_TEST_BUILD_VERSION` | Test hook, debug builds only: version reported by `loom` and `loomd` (`--version`, `loom version`, `/v1/status`) instead of the build's. Ignored by release builds. |
| `LOOM_TEST_TODAY` | Test hook, debug builds only: date (`YYYY-MM-DD`) of the priority list of `loomd` (`GET /v1/tasks?ranked=true`) and of its [schedules](../configuration.md#schedules), instead of the local date. An invalid value is ignored. Ignored by release builds. |
| `LOOM_TEST_PROMPT_BUDGET_MS` | Test hook, debug builds only: number of milliseconds that replaces the 100 ms that `loom prompt` waits for `loomd`. An invalid value is ignored. Ignored by release builds. |
| `TZ` | Time zone of the local date of the priority list and of the [schedules](../configuration.md#schedules) in `loomd`; `/etc/localtime` when unset, UTC when neither can be read. |
| `LOOM_TEST_EVENTS_HEARTBEAT` | Test hook, debug builds only: duration that replaces the 15 s interval between two heartbeats of `GET /v1/events`. `loomd` logs an invalid value and ignores it. Release builds ignore it. |
| `LOOM_TEST_SCHEDULE_INTERVAL` | Test hook, debug builds only: duration that replaces the 1 min interval between two checks of the [schedules](../configuration.md#schedules) in `loomd`. `loomd` logs an invalid value and ignores it. Release builds ignore it. |
| `LOOM_TEST_SHUTDOWN_GRACE` | Test hook, debug builds only: duration that replaces the 10 s that the requests in flight have to finish after the first signal. `loomd` logs an invalid value and ignores it. Release builds ignore it. |
| `LOOM_TEST_FAIL_UNLESS_HOME_PREFIX` | Test hook, debug builds only: `loomd` exits with code `1` at start when its loom home path does not start with this text. Ignored by release builds. |
| `LOOM_TEST_CRASH_STAGE` | Test hook, debug builds only: stage where `loomd` sends `SIGKILL` to itself during a store write (`after_task_stage`, `after_task_rename`, `after_events_append`, `after_create_stage`, `before_archive_rename`, `after_archive_rename`). Ignored by release builds. |
| `OTEL_SDK_DISABLED` | `true` (any case): no traces are exported, even with `[telemetry] enabled = true`. See [`[telemetry]`](../configuration.md#telemetry). |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | Traces endpoint, used as given, when `[telemetry] otlp_endpoint` is not set. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Base URL; traces go to `<value>/v1/traces` when neither `otlp_endpoint` nor `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` is set. |
| `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_EXPORTER_OTLP_TRACES_HEADERS` | Headers sent with each export, `key=value` pairs separated by commas (for example an authorization header). The traces variable wins. |
| `OTEL_EXPORTER_OTLP_TIMEOUT`, `OTEL_EXPORTER_OTLP_TRACES_TIMEOUT` | Timeout of one export, in milliseconds; default 10000. The traces variable wins. `loom` still waits at most 2 s at exit. |
| `OTEL_SERVICE_NAME` | `service.name` of the exported traces, instead of `loom` and `loomd`. |
| `OTEL_RESOURCE_ATTRIBUTES` | Extra resource attributes, `key=value` pairs separated by commas. |
| `OTEL_BSP_SCHEDULE_DELAY`, `OTEL_BSP_MAX_QUEUE_SIZE`, `OTEL_BSP_MAX_EXPORT_BATCH_SIZE` | Batching of spans: delay between two exports in milliseconds (default 5000), queue size (default 2048), batch size (default 512). |

The `OTEL_*` variables only apply when `[telemetry] enabled = true`. `loom status` passes its environment to the `loomd` it starts.

## `loom version` output

Human output (`loom version`, identical to `loom --version`): the version on the first line, then one field per line. `loom -V` prints only `loom <version>`.

```text
loom 0.1.0-dev.42
commit:      0123456789abcdef0123456789abcdef01234567
commit_date: 2026-10-12T10:00:00+02:00
target:      aarch64-apple-darwin
channel:     dev
profile:     release
```

JSON output (`loom version --json`), `VersionInfo` schema:

| Field | Type | Content |
| --- | --- | --- |
| `version` | string | `LOOM_BUILD_VERSION` at build time when set, otherwise the crate version. |
| `commit` | string | Full git commit hash of the build, `unknown` when built outside a git checkout. |
| `commit_date` | string | Committer date of `commit` (strict ISO 8601), `unknown` outside a git checkout. |
| `target` | string | Target triple the binary was compiled for. |
| `channel` | `stable` \| `dev` \| `local` | `LOOM_CHANNEL` at build time; `local` when unset. |
| `profile` | `debug` \| `release` | Cargo profile family of the build. |

`loomd --version` prints the same build information, with the prefix `loomd`. `loomd -V` prints only `loomd <version>`. Both binaries built together report identical build information.

## `loom status` output

`loom status` queries `GET /v1/status` on the socket of the loom home. The loom home is `LOOM_HOME`, otherwise `~/.loom` (`loom` has no `--loom-home` flag). Each request times out after 5 s.

### Starting loomd

If nothing accepts connections on the socket (no socket file, or no process listens on it), `loom status` starts `loomd`, unless `--no-start` is given:

1. It runs the first `loomd` that exists: next to the `loom` executable, then in each `PATH` entry in order.
2. `loomd` runs in its own process group, with `LOOM_HOME` set to the loom home, stdin and stdout closed, and stderr appended to `<loom home>/log/loomd.log` (see [configuration](../configuration.md)). `loom` does not wait for it to exit.
3. `loom` queries the socket every 50 ms for up to 5 s, then prints the status.

`loom status` logs `loomd is not running, starting it` at info level on stderr (hidden by `-q`).

### Human output

The daemon version on the first line, then one field per line.

```text
loomd 0.1.0-dev.42 running
pid:         4242
started_at:  2026-10-12T08:00:00Z
uptime_secs: 90
api_version: v1
commit:      0123456789abcdef0123456789abcdef01234567
channel:     dev
loom_home:   /home/me/.loom
socket_path: /home/me/.loom/run/loomd.sock
config_path: /home/me/.loom/config.toml
disk:        35 MB (versions 30 MB, index 4.2 MB, logs 25 KB, contexts 0 B)
updates:     auto, channel dev (chosen), every 6h
last_check:  2026-10-12T08:05:00Z
available:   none
failed:      0.1.1
last_result: upgraded 0.1.0-dev.41 -> 0.1.0-dev.42 at 2026-10-12T08:00:30Z
```

The `disk` line comes from the `disk` object of the status ([API reference](../api.md#diskusage)). Its format is `<total> (versions <versions>, index <index>, logs <logs>, contexts <sum of the context stores>)`. If the status has no `disk` object, `loom status` prints no `disk` line. Sizes use decimal units (`B`, `KB`, `MB`, `GB`, `TB`, `PB`, `EB`), and 1 KB is 1,000 bytes. A size below 10 is rounded to one decimal (`4.2 MB`), A size of 10 or more is rounded to a whole number (`35 MB`).

The update lines come from the `update` object of the status ([API reference](../api.md#updatestatus)). If the status has no `update` object, `loom status` prints none of these lines.

| Line | Content |
| --- | --- |
| `updates` | `off`; `<policy>, no channel: not checked` for a local build without a configured or chosen channel; otherwise `<policy>, channel <channel> (<source>), every <check_interval>`. `<source>` says where the channel comes from: `config` (`[update] channel`), `chosen` (last `loom upgrade --channel`, or `install.sh --channel`), `build` (the build channel). See [channel resolution](../configuration.md#channel-resolution). If the daemon does not report the source, the line has no parenthesis. |
| `last_check` | Instant of the last check, `never` before the first. |
| `available` | Newer version recorded under `notify`, `none` otherwise or once that version runs. |
| `pending` | `<from> -> <to> since <instant>`, only while an upgrade is in progress. |
| `failed` | Versions never applied automatically again, comma-separated, or `none`. |
| `last_result` | `none`, or one of: `up to date at <t>`, `<version> available at <t>`, `<version> skipped at <t>: it failed before`, `upgraded <from> -> <to> at <t>`, `<version> failed the pre-flight check at <t>: <message>`, `rolled back <failed> -> <restored> at <t>: <message>`, `upgrade <from> -> <to> interrupted, found at <t>`, `failed at <t>: <message>`. |

### JSON output

`loom status --json` prints the `StatusResponse` that loomd returns, unchanged, as pretty JSON followed by a newline. Fields: [API reference](../api.md#statusresponse). Schema: [`status_response.json`](../schemas/status_response.json).

### Version mismatch

If the daemon and the `loom` binary have a different `version` or `commit`, `loom status` prints a warning on stderr (hidden by `-q`), then exits with `0`:

```text
 WARN loom 0.1.0-dev.43 (commit 89abcdef0123) and the running loomd 0.1.0-dev.42 (commit 0123456789ab) are different builds: restart loomd (PID 4242) to run the same build
```

### Exit codes

| Code | Case | Message on stderr |
| --- | --- | --- |
| `0` | The daemon answered, including after a version mismatch warning. | |
| `1` | `--no-start` and the daemon does not run. | ``Error: loomd is not running: cannot connect to <socket>: <cause>; run `loom status` without --no-start to start it`` |
| `1` | No `loomd` next to `loom` or in `PATH`. | `Error: cannot start loomd: no loomd next to <loom executable> or in PATH; install loomd next to loom` |
| `1` | The started `loomd` exited, or did not answer within 5 s. | `Error: cannot start loomd: <reason>; last lines of <loom home>/log/loomd.log:` followed by up to 5 lines that this start wrote to the log. |
| `1` | The daemon answered with an error. | `Error: loomd returned an error (HTTP <status>, <code>): <message>` |
| `1` | The daemon answered something that is not the API: invalid body, error status without an `ApiError` body, no answer within 5 s. | `Error: unexpected answer from loomd: <detail>; check that the process listening on <socket> is a loomd of the same version as loom` |
| `1` | The socket path is too long. | See [socket path length](../configuration.md#socket-path-length). |
| `2` | Usage error. | |

`loomd` exits with code `1` on a failure, and also when another `loomd` holds the lock. If the `loomd` that `loom status` started exits with `1`, `loom status` probes `run/loomd.lock`. It tries once to take the lock, does not wait, and releases the lock at once. Then:

- No process holds the lock, or the file does not exist: no other `loomd` starts. `loom status` fails at once with the end of the log.
- Another process holds the lock: another `loomd` won the race. `loom status` continues to query the socket until the end of the 5 s. If that daemon answers, `loom status` succeeds with its status.

If a `loomd` that `loom status` started still runs but did not answer within 5 s, `loom status` does not stop it.

## `loom service` output

`loom service` manages `loomd` as a user service: a launchd user agent on macOS, a systemd user unit on Linux. Files and keys: [configuration](../configuration.md#user-service).

### `loom service status`

Human output, one field per line:

```text
platform:  macos
unit:      /Users/me/Library/LaunchAgents/io.github.gsuquet.loomd.plist
installed: yes
loaded:    yes
running:   yes
```

JSON output (`loom service status --json`), `ServiceStatus` schema ([service_status.json](../schemas/service_status.json)):

| Field | Type | Content |
| --- | --- | --- |
| `platform` | `macos` \| `linux` | Service manager family: launchd or systemd. |
| `unit_path` | string | Absolute path of the property list (macOS) or unit file (Linux). |
| `installed` | boolean | The file at `unit_path` exists. |
| `loaded` | boolean | macOS: `launchctl print gui/<uid>/io.github.gsuquet.loomd` succeeds. Linux: `systemctl --user is-enabled loomd.service` succeeds. |
| `running` | boolean | macOS: loaded, and the `launchctl print` output has a `state = running` line. Linux: `systemctl --user is-active loomd.service` succeeds. |

`status` exits with `0` whatever the state.

### `loom service install` and `uninstall`

On success, one line on stdout:

| Command | Case | Output |
| --- | --- | --- |
| `install` | Something changed | `loomd service installed: <unit path>` |
| `install` | File identical and service loaded: nothing run | `loomd service already installed: <unit path>` |
| `uninstall` | Something changed | `loomd service uninstalled: <unit path>` |
| `uninstall` | No file and service not loaded: nothing run | `loomd service not installed: <unit path>` |

With `-n`/`--dry-run`, if something would change, stdout lists the steps instead, in order, one step per line. Each step is one of: `write <path>:` followed by the full file content, `create directory <path>`, `remove <path>` or `run <command>`. Single quotes enclose the arguments that contain spaces or quotes.

```text
write /Users/me/Library/LaunchAgents/io.github.gsuquet.loomd.plist:
<?xml version="1.0" encoding="UTF-8"?>
...
</plist>
create directory /Users/me/.loom/log
run launchctl bootstrap gui/501 /Users/me/Library/LaunchAgents/io.github.gsuquet.loomd.plist
```

The table gives the commands that `install` and `uninstall` run in each case. "Loaded" and "running" have the same meaning as in `status`.

| Platform | Command | Case | Commands that change state |
| --- | --- | --- | --- |
| macOS | `install` | No file, or different content | `launchctl bootout gui/<uid>/io.github.gsuquet.loomd` if loaded, then `launchctl bootstrap gui/<uid> <plist>` |
| macOS | `install` | Identical file, not loaded | `launchctl bootstrap gui/<uid> <plist>` |
| macOS | `uninstall` | Any | `launchctl bootout gui/<uid>/io.github.gsuquet.loomd` if loaded, then the file is removed |
| Linux | `install` | No file, or different content | `systemctl --user daemon-reload`, `systemctl --user enable --now loomd.service`, then `systemctl --user restart loomd.service` if it was running |
| Linux | `install` | Identical file, not enabled | `systemctl --user daemon-reload`, `systemctl --user enable --now loomd.service` |
| Linux | `uninstall` | Any | `systemctl --user disable --now loomd.service` if enabled or running; if the file exists, it is removed, then `systemctl --user daemon-reload` |

On macOS, `<uid>` comes from `id -u`. On macOS, `install` also creates `<loom home>/log` (mode `0700`) before `bootstrap`. `install` creates the parent directories of the unit file that do not exist.

A `bootstrap` follows a `bootout` in the same run when the plist changed while the service was loaded. launchd can then still be in the unload of the service. Thus `loom` tries that `bootstrap` again while it exits non-zero: up to 5 attempts, 200 ms apart. If all attempts fail, `loom` reports the stderr of the last attempt. A `bootstrap` that does not follow a `bootout` runs once.

`install` and `uninstall` never ask for confirmation.

### `loom service restart`

Steps, in order:

1. `loom` parses and validates `config.toml` as `loomd` does. If the file is invalid, `loom` prints the error, exits with `1` and does not touch `loomd`.
2. If the service is installed for this loom home and loaded (as for the [auto-upgrade watchdog](../configuration.md)), its manager restarts `loomd`. On macOS, `loom` runs `launchctl kickstart -k gui/<uid>/io.github.gsuquet.loomd`. On Linux, `loom` runs `systemctl --user reset-failed loomd.service`, then `systemctl --user restart loomd.service`. `loom` sends no signal.

   If the manager reports that the service does not run but a `loomd` holds the lock, `loom` first sends `SIGTERM` to that `loomd`. A `loom` command started this `loomd` while the service was stopped. Then it waits up to 10 s for the lock, as in step 3. Without this step, the `loomd` of the service would fail on the lock.
3. If there is no service and a `loomd` holds the lock, `loom` sends `SIGTERM` to the PID in the lock. Then it waits up to 10 s until the lock is released. Then it starts `loomd` detached, as any command does.
4. If there is no service and no `loomd`, `loom` starts `loomd` detached.
5. `loom` waits for an answer to `GET /v1/status` from a `loomd` with a PID other than the previous one. It waits up to 15 s after the service manager, otherwise for the usual start window.

On success, one line on stdout:

```text
loomd restarted (pid 812 -> 9431, 0.3.0, via launchd) in 640 ms
loomd was not running: started (pid 9431, 0.3.0, via start) in 640 ms
```

On a failure, `loom` exits with `1` and prints the step, the PID and the path of the log. These failures can occur:

- The old `loomd` does not exit within 10 s. It continues to run, because `loom` never sends `SIGKILL`.
- The old `loomd` still answers after the restart by the service manager.
- No new `loomd` answers in time.
- `loom` cannot send `SIGTERM`.

The output `loomd restarted (pid ? -> …)` means that a `loomd` held the lock but did not write its PID yet.

JSON output (`loom service restart --json`), `DaemonRestartReport` schema ([daemon_restart_report.json](../schemas/daemon_restart_report.json)):

| Field | Type | Content |
| --- | --- | --- |
| `was_running` | boolean | A `loomd` held the lock when the command started. |
| `previous_pid` | integer or `null` | PID of that `loomd`; `null` when none ran. |
| `pid` | integer | PID of the `loomd` that answers now. |
| `version` | string | Its version. |
| `via` | `launchd` \| `systemd` \| `signal` \| `start` | Who restarted or started it. |
| `elapsed_ms` | integer | Milliseconds from the start of the command to the first answer of the new `loomd`. |

### Exit behavior

| Code | Case |
| --- | --- |
| `0` | Success, including the no-op cases and `--dry-run`. |
| `1` | `install` refused by `[daemon] service = false`; invalid `config.toml`; `loomd` not found; a service manager command could not start or exited non-zero (its command line, exit code and stderr are printed); a file could not be read or written; platform other than macOS and Linux. |
| `2` | Usage error, such as `loom service` without a subcommand. |

Refusal message:

```text
the loomd service is disabled by `[daemon] service = false` in /Users/me/.loom/config.toml: set it to true or remove it to install the service
```

The message when a service manager command fails:

```text
`launchctl bootstrap gui/501 /Users/me/Library/LaunchAgents/io.github.gsuquet.loomd.plist` exited with code 5: Bootstrap failed: 5: Input/output error
```

`install` reads `config.toml` first. If the configuration refuses `install`, no command runs and no file is written. The steps of a plan run in order and stop at the first failure.

## Task commands

If `loomd` does not run, the task commands start it. Their requests time out after 15 s. The reason is that right after it starts, `loomd` waits up to 10 s for the tasks that it indexes, and then answers.

### Context

`loom add` and `loom ls` work in one context, the first of:

1. `-c`/`--context`.
2. `LOOM_CONTEXT`, if it is not empty. An invalid name is an error.
3. The context of the indexed repository that contains the current directory: its `[repos."<path>"]` override, the `match.hosts` of its remote, its `match.paths` prefix, or `default_context`. An unassigned repository gives no context.
4. The `match.paths` prefix of the current directory.
5. `default_context`.

If the repository of the current directory is not in the index yet (for example, a new clone), `loom` registers it first. Thus its remote decides the context. If no rule gives a context, `loom` exits with `1` and the message `no context: pass -c <name>, …`.

`loom ls --all-contexts` lists every context. It ignores `LOOM_CONTEXT`, which applies to a full shell. It cannot be combined with `-c`, which applies to this command. `loom show` takes an ID of any context.

An ID (`loom show`, `--parent`) is 1 to 8 letters and digits of Crockford base32. Crockford base32 has no `u`, and reads `i`, `l` and `o` as `1`, `1` and `0`. Any other value is a usage error (exit `2`).

### `loom add`

The title is the arguments, with spaces between them. The status of the task is `inbox`, or `ready` with `--ready`. Its kind is `chore`, unless `--kind` is given. Its importance is `--importance`, otherwise the default of the kind.

`--due` takes `YYYY-MM-DD`, `today`, `tomorrow`, `+<n>d` or `+<n>w`, relative to the local date. `--parent` takes an ID or a unique prefix. The task goes to the context of the parent. If `-c` or `LOOM_CONTEXT` names another context, it is an error.

`--format` (`talk`, `article`, `training`, `workshop`) applies to `--kind content` only. Output: the shortest unique prefix, the context and the title, or the `TaskDetail` with `--json`.

### `loom ls`

Without `--status`, `loom ls` prints the priority list of the context, with the position of each task in a `#` column. The list has the `ready` and `active` tasks, and the `waiting` tasks whose date has come. With `--status`, it prints the tasks with that status, sorted by ID. With `--archived`, it prints the archived tasks (`archive/<yyyy>/`), sorted by ID. With `--archived` and `--status`, it prints only the archived tasks with that status. No other view shows an archived task. Columns: `ID` (shortest unique prefix), `CONTEXT`, `STATUS`, `P`, `KIND`, `DUE`, `TITLE` (`* ` before a pinned task, and the error for an invalid `task.md`). `--why` (not with `--status` or `--archived`) adds `WHY`, the rule that placed the task after the previous one:

| `WHY` | Rule |
| --- | --- |
| `first` | Heads the list. |
| `pinned` | The previous task is pinned, this one is not. |
| `overdue or due today` | The previous task is overdue or due today, this one is not or is due later. |
| `more important` | The previous task has a higher importance. |
| `due earlier` | The previous task is due earlier, or this one has no due date. |
| `older` | The previous task was created earlier. |
| `tie, by id` | Every other rule ties. |

`--json`: the `TaskList`. If there is no task, the output is empty.

### `loom show`

`loom show` prints one field per line: `id`, `context`, `title`, `kind`, `status` and `importance`. Then it prints `due`, `pinned` and `parent` if they are set, then `created` and `updated`. A task that this version cannot change also has `read-only`. For an archived task, `read-only` is `archived in archive/<yyyy>`. Then come a blank line and the notes. `--json`: the `TaskDetail`.

For an unknown ID, `loom show` exits with `1`. If several tasks start with the prefix, it exits with `1` and lists them.

### Gestures on a task

`loom set`, `done`, `cancel`, `snooze` and `undo` take a task ID or prefix. They print one line, `<prefix> <context> <status> <title>`, or the `TaskDetail` with `--json`. loomd refuses a transition that the rules forbid (`done` and `cancelled` move to `ready` only). It also refuses a change of a read-only task. Then the command exits with `1` and the error code of loomd.

| Command | Request |
| --- | --- |
| `loom done <id>` | `POST /v1/tasks/{id}/actions/done` |
| `loom cancel <id>` | `POST /v1/tasks/{id}/actions/cancelled` |
| `loom snooze <id> <until> [--on <who>]` | `POST /v1/tasks/{id}/actions/waiting` with `until` (a date as for `--due`) and `on` |
| `loom undo <id>` | `POST /v1/tasks/{id}/undo` |
| `loom set <id> <key>=<value>…` | `PATCH /v1/tasks/{id}`, then `pin` or `unpin` for `pinned` |

`loom set` keys (each key at most once, and a value can contain `=`):

| Key | Value |
| --- | --- |
| `title` | The new title. |
| `kind` | A kind of `loom add --kind`. |
| `format` | `talk`, `article`, `training` or `workshop`; only together with `kind=content`, even for a task that is already `content`. |
| `importance` | `p0` to `p3`. |
| `due` | A date as for `--due`, or `none` to remove it. |
| `parent` | An ID or prefix, or `none`. The other relations are kept. loomd refuses a task of another context, the task itself or one of its descendants. |
| `pinned` | `yes`/`true` or `no`/`false`. |

A malformed `key=value` or an unknown key is a usage error (exit `2`). A repeated key or an invalid value exits with `1` before `loom` sends a request to loomd.

### `loom edit`

`loom edit` opens the `task.md` of the task in an editor. The path is `<store>/tasks/<id>/task.md`, with the store of its context in `config.toml`. The editor is `VISUAL`, otherwise `EDITOR`, otherwise `vi`. `loom` runs it through `sh -c`, with the path as its argument.

`loom edit` does not open an archived task. It exits with `1` and this message: `task <prefix> is archived (archive/<yyyy>): an archived task is read-only. To reopen it, set its status in archive/<yyyy>/<id>/task.md, then move its directory back to tasks/.` See [open an archived task again](../../how-to-guides/manage-tasks.md#open-an-archived-task-again).

For the other tasks, after the editor closes:

- File unchanged: prints `<prefix> unchanged`.
- File invalid (`task.md` format): exit `1` with the path and the error. The file stays as you saved it. loomd shows the task as read-only until the file is correct.
- Other cases: `loom` waits up to 5 s until the index of loomd holds the saved task and notes, then prints the task line. If only spacing changed, `loom` prints the line at once.
- If the index does not hold the save in that time and loomd recorded the hand edit, the task changed again after the save. `loom` prints `<prefix> saved; the task changed again after the save`, a warning with the actors, and the task line.
- If loomd did not record the hand edit in that time, `loom` prints `<prefix> saved; loomd has not recorded it within 5 s`. This occurs if loomd did not read the file yet.
- Other events can be added to the log of the task while the editor is open. For example: a `loom done` in another terminal, a sync, an agent. In that case, `loom` prints a warning on stderr with their actors. The save replaced the file, and loomd recorded it as an edit that can revert these events. `loom undo <id>` reverts that edit.
- The editor exits non-zero: exit `1`, the file left as it is.
- The task is a closed one and the hourly archive pass moves it while the editor is open: the save targets `tasks/<id>/task.md`, whose directory is gone. Most editors fail to save; one that creates the missing directory leaves a `tasks/<id>/` beside the archived task, which then wins over it ([task files](../task-file.md)).

## `loom upgrade`

### What it does

1. Looks up a release through the GitHub REST API (`https://api.github.com`, or `LOOM_RELEASES_URL`). `loom upgrade` checks server certificates against the trust store of the operating system (Keychain on macOS, the system CA bundle on Linux). Thus a proxy that inspects TLS works if the system trusts it. `loom upgrade` uses the proxy variables `ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY` (the first that is set wins) and `NO_PROXY`, in upper or lower case.

   | Options | Request | Release considered |
   | --- | --- | --- |
   | `--channel stable`, or none with the effective channel `stable` | `GET /repos/gsuquet/loom/releases/latest` | The latest stable release. |
   | `--channel dev`, or none with the effective channel `dev` | `GET /repos/gsuquet/loom/releases?per_page=30` | Every non-draft release tagged `vX.Y.Z` or `vX.Y.Z-dev.N` in the 30 most recent. Other tags are skipped. The highest by semver precedence wins, so a stable release above every dev build is installed. |
   | `--version X.Y.Z` | `GET /repos/gsuquet/loom/releases/tags/vX.Y.Z` | That release. A 404 means not released. |
   | `--from PATH` | none | The local build. |

   Without `--channel`, `--version` or `--from`, the effective channel is `[update] channel`, otherwise the channel last given to `--channel`, otherwise the build channel ([channel resolution](../configuration.md#channel-resolution)). A local build (channel `local`) has no channel of its own: without any of them, `loom upgrade` fails.

   `loom upgrade` remembers `--channel`. When the command succeeds, it writes the channel to `channel` of `<loom home>/state/update.json`. This includes `--check` and the up-to-date case. With `--dry-run`, it does not write the channel. The auto-upgrade of loomd and the next `loom upgrade` use this channel.

   If `[update] channel` names another channel, a warning says that it takes precedence. If `loom` cannot write the state, `--check` fails (exit code `1`). In the other cases, it is a warning. `--version` and `--from` leave the remembered channel unchanged.

   Before the lookup, `loom upgrade` reads `<loom home>/state/update.json`, except with `--check`. It fails while an auto-upgrade of loomd is in progress (a `pending` upgrade that started less than one hour ago). The message is `loomd is upgrading from <from> to <to> (started <instant>): retry when it is done, in a minute or two`.

2. Decides by semver precedence. A newer release is installed, and the same version is up to date. A `--version` lower than the current version is refused (use `loom rollback`). `--from` always installs, whatever its version.

   With `--channel` or the effective channel, `loom upgrade` skips the versions in `failed_versions` of `state/update.json`, as the auto-upgrade does. If all newer versions failed, the decision is `skipped`, with the reason `failed`. `--version` installs a failed version anyway, so that you can try it again on purpose.
3. Checks that the install directory is writable, before any download.
4. Downloads `SHA256SUMS` and `SHA256SUMS.minisig`, and verifies the signature with the public key embedded at build time ([releases](../releases.md#signature)). Then downloads `loom-<version>-<target>.tar.gz` and checks its SHA-256 against `SHA256SUMS`. A `*-linux-gnu` build takes the `*-linux-musl` archive. If a step fails, the upgrade stops here, and no installed file has changed.
5. Extracts the archive into `<loom home>/staging/`. Only the regular files `loom`, `loomd`, `LICENSE` and `README.md` at the archive root are accepted. Any other entry (directory, link, path) rejects the archive. Then runs `loom version --json` and `loomd -V` from the staging directory: both must report the release version.
6. Copies the installed `loom` and `loomd` into `<loom home>/versions/<running version>/`.
7. Replaces `loomd`, then `loom`, in the directory of the current `loom` (symbolic links resolved). Each new file is written next to its target, synced, set to mode `0755`, then renamed over the target. If `loomd` does not exist, it is created.
8. Keeps two previous versions in `versions/` (see [configuration](../configuration.md#versions)).
9. If a loomd holds `<loom home>/run/loomd.lock`, sends `SIGTERM` to the PID in the file (`kill -TERM`), and does not wait. A service manager restarts it on the new binary. If no process holds the lock file, its PID is stale, and `loom upgrade` sends no signal to it.

`--from PATH` takes a `.tar.gz` archive with the release layout, or a directory that contains `loom` and `loomd`. `loom upgrade` does not verify its signature and checksum. A warning on stderr says so. Steps 5 to 9 apply.

`--check` stops after step 2. `--dry-run` stops after step 5.

### Human output

```text
$ loom upgrade --check
loom 0.2.0 is available (stable), running 0.1.0
$ loom upgrade
upgraded loom and loomd from 0.1.0 to 0.2.0 in /home/me/.local/bin
saved 0.1.0 in /home/me/.loom/versions/0.1.0
removed saved versions 0.0.9
sent SIGTERM to loomd (PID 4242) so that it restarts on the new version
$ loom upgrade
loom 0.2.0 is up to date (stable)
$ loom upgrade --dry-run
would upgrade loom and loomd from 0.1.0 to 0.2.0 in /home/me/.local/bin
```

The `removed` line appears only when saved versions were deleted. If no loomd holds the lock, the last line is `loomd is not running`. With `--from`, a last line `source:   <path> (not verified)` is added. Progress (`downloading …`, `verified …`) goes to stderr, except with `-q`.

### JSON output

`loom upgrade --json`: [`UpgradeReport`](../schemas/upgrade_report.json).

| Field | Content |
| --- | --- |
| `current_version` | Version of the current `loom`. |
| `source` | `{"kind": "release", "channel": "stable" \| "dev" \| null, "pinned": "<version>" \| null}` or `{"kind": "local", "path": "<absolute path>"}`. |
| `decision` | `{"kind": "up_to_date"}`, `{"kind": "apply", "version": …}`, or `{"kind": "skipped", "reason": {…}}` with reason `kind` `pinned_not_released` or `pinned_below_current` (and `pinned`), or `failed` (and `version`). `available` and `updates_off` are defined for the daemon and not produced by `loom upgrade`. |
| `check`, `dry_run` | Flags given. |
| `install` | `null` with `--check` or when nothing is installed; otherwise an install report (below). |

Install report (also in `loom rollback`):

| Field | Content |
| --- | --- |
| `version` | Version installed, or that would be with `--dry-run`. |
| `install_dir` | Directory of the current `loom`. |
| `verified` | `true` for a release (signature and checksum verified); `false` for `--from` and `loom rollback`. |
| `installed` | `false` with `--dry-run`. |
| `saved_to` | `<loom home>/versions/<previous version>`; `null` with `--dry-run`. |
| `pruned` | Versions deleted from `versions/`. |
| `daemon` | `{"kind": "skipped"}` (dry run), `{"kind": "not_running"}`, `{"kind": "signalled", "pid": …}`, or `{"kind": "failed", "pid": … \| null, "message": …}`. |

### Exit status

| Code | Cases |
| --- | --- |
| `0` | Installed; up to date; `--check` whatever the decision; `--dry-run` verified. Also when `loom` cannot send the signal to loomd: a warning on stderr says that it continues to run the replaced version. |
| `1` | Auto-upgrade of loomd in progress; every newer version failed; local build without `--channel`, `--version` or `--from`; `--version` not released or lower than the current version; install directory not writable; HTTP error (`401`, `403` and `429` name `LOOM_GITHUB_TOKEN`); missing asset; invalid signature; checksum mismatch; rejected archive; binaries that do not report the expected version; I/O error. Every download or verification error ends with `nothing was installed`. |
| `2` | Usage error, including `--channel`, `--version` and `--from` together, or `--check` with `--from` or `--dry-run`. |

## `loom rollback`

`loom rollback` fails, like `loom upgrade`, while an auto-upgrade of loomd is in progress. It restores the highest version in `<loom home>/versions/` other than the current one (semver precedence). Only directories that contain both `loom` and `loomd` count. The saved `loom version --json` and `loomd -V` must report the version of the directory. Then steps 6 to 9 of `loom upgrade` apply. The current version is saved, the binaries are replaced, two previous versions are kept, and loomd gets a signal.

If no higher version is saved, a second rollback returns to the version that the first rollback replaced.

Human output: as `loom upgrade`, with `rolled back` (`would roll back` with `--dry-run`).

JSON output (`--json`): [`RollbackReport`](../schemas/rollback_report.json): `current_version`, `dry_run` and `install` (the install report above, `verified` always `false`).

| Code | Cases |
| --- | --- |
| `0` | Restored, or would be with `--dry-run`. |
| `1` | Auto-upgrade of loomd in progress; `nothing to roll back to: no saved version other than <version> in <dir>`; install directory not writable; saved binaries that do not report their version; I/O error. |
| `2` | Usage error. |

## `loom repo`

`loom repo` reads and changes the repository index of loomd through the [API](../api.md#repositories). If loomd does not run, these commands start it as [`loom status`](#starting-loomd) does: `ls`, `show`, `scan`, `register` (without `-n` or `--on-use`), `forget` and `sync`. Then they wait for the scan of loomd on start, so that the index that they read is complete. `doctor`, `register -n` and `register --on-use` never start it.

### Repository argument

`<REPO>` of `show` and `forget` is resolved in this order:

| Argument | Resolution |
| --- | --- |
| None (`show` only) | The repository of the current directory. |
| Starts with `/`, `.` or `~` | A path: absolute, relative to the current directory, or under `HOME` (`~`, `~/…`). `.` and `..` components are applied lexically. |
| Contains `/` | The repository with this exact slug (`<context>/<name>`); when none has it, a path relative to the current directory. |
| Anything else | The repositories with this name (directory name). One: that repository. Several: narrowed by the first context, in this order, that holds at least one of them: `-c`, then the context of the current directory (the context of its repository when the index has it, otherwise the context its path gets from `match.paths`). If that context holds one, it is the answer. In the other cases, or if no context holds one of them, the command fails and lists the candidates. None: a path relative to the current directory. |

A path must exist. `loom` goes up from the path to the nearest directory with a `.git` entry (the repository root), and keeps symbolic links. `loom` looks for that root in the index by path. Then, for repositories with the same directory name, it looks by canonical path (the same directory through other symbolic links). The current directory is `PWD` if `PWD` names the same directory as the working directory of the process and has no `.` or `..` component. Otherwise, it is the working directory.

`show` registers a root that is not in the index. `forget` fails on such a root.

Ambiguous name, exit code `1`:

```text
Error: `loom` names several repositories; give one of:
  personal/loom (/home/me/perso/loom)
  work/loom (/home/me/work/loom)
```

### Registration on use

A `loom` command that runs inside a git repository sends one `POST /v1/repos/register` with `on_use: true` for the repository of the current directory. The exceptions are `repo show`, `repo register`, `repo forget`, `repo scan --rebuild`, `repo sync`, `shell-init`, `prompt` and the internal watchdog. The request goes on a separate thread. loomd registers the repository if the index does not have it, and sets its `last_used`.

The command waits for the request at most 50 ms after the command started, and never fails because of it. If the socket file does not exist, nothing is sent. The registration on use never starts loomd. `loom repo show` sets `last_used` the same way. `loom repo register --on-use --background`, run by the [shell integration](#loom-shell-init), sends the same request for its `<PATH>` (default: the current directory).

### `loom repo ls`

Repositories sorted by context name, then name, then slug. `-c <NAME>` keeps the repositories of one context (`-c unassigned` keeps those without one).

`--format table` (default): a header, then one row per repository, columns aligned, the path last. The first column is `!` for a repository without context. If there is at least one such repository, a warning on stderr gives their count and names `loom repo doctor`. If the index is empty, nothing is printed.

```text
  SLUG             HEAD     STATE                PATH
  personal/loom    main     2 changed, ahead 1   /home/me/perso/loom
! unassigned/tool  main     clean                /home/me/src/tool
  work/api         feat/x   rebase, behind 3     /home/me/work/api
```

| Column | Content |
| --- | --- |
| `HEAD` | Branch; `<branch> (unborn)`; `detached <commit>`; `unknown`; `?` before the state is first read. |
| `STATE` | Comma-separated: operation in progress, `<n> changed`, `<n> untracked` (`1000+` when capped), `ahead <n>`, `behind <n>`; each unknown field as `operation unknown`, `changes unknown`, `untracked unknown`, `upstream unknown`, `ahead unknown`, `behind unknown`. `clean` when nothing applies, `unknown` when operation, changes, untracked files and upstream are all unknown, `not read yet` before the state is first read. |

`--format paths`: one absolute path per line, nothing else.

`--json`: [`RepoList`](../schemas/repo_list.json), in the same order. `--json` and `--format` cannot be combined (exit code `2`).

### `loom repo show`

`loom repo show` marks the repository as used, and registers it if the index does not have it. It reads the state of the repository if it has no state yet, or if `--refresh` is given (`GET /v1/repos/{context}/{name}?refresh=true`). Then it prints one `field: value` line for each field, with the names of the JSON output:

```text
slug:           personal/loom
path:           /home/me/perso/loom
context:        personal (match.paths "~/perso")
remotes:        origin git@github.com:gsuquet/loom.git (primary)
forge:          github
registered:     no
last_used:      2026-10-13T08:00:00Z
head:           main
dirty:          2
untracked:      unknown
operation:      none
upstream:       origin/main, ahead 1, behind 0
default_branch: main
last_commit:    2026-10-12T17:42:10Z
last_fetch:     never
refreshed_at:   2026-10-13T08:00:00Z
error:          untracked: time budget of 2s exceeded
```

| Field | Content |
| --- | --- |
| `context` | Context, then the rule that chose it: `[repos."<path>"] context`, `match.hosts "<entry>"`, `match.paths "<entry>"`, `default_context`, or `unassigned (no rule matches)`. |
| `remotes` | One line per remote: name, URL as configured, `(primary)` for the primary remote; `none` without remote. |
| `forge`, `last_used` | `none`, `never` when absent. |
| State fields | `unknown` when not computed. `operation`, `upstream`, `default_branch`, `last_commit`: `none` when known to be absent; `last_fetch`: `never`. `error` only when set. Before a state exists: a single `state: not read yet` line. |

`--json`: [`RepoSummary`](../schemas/repo_summary.json).

### `loom repo scan`

`POST /v1/repos/scan`. Human output: `<added> added, <updated> updated, <removed> removed in <ms> ms`, then one `cannot read <path>: <message>` line per path the scan could not read. `-n`/`--dry-run`: `would add <n>, update <n>, remove <n>`, nothing changed. `--json`: [`ScanResult`](../schemas/scan_result.json).

`--rebuild` drops the whole index (states, results of the last updates, `last_used`, registrations) and scans the roots again. Thus the registered repositories outside the roots are lost. Before it asks, it prints a warning on stderr with the names of the registered repositories outside the roots.

Then it follows the [confirmations](#confirmations) rules. On the terminal, it asks `Rebuild the repository index (<n> registered repositories outside the roots lost)? [y/N]`. Any answer other than `y` or `yes` fails with `rebuild cancelled: nothing changed`. With `-n`, it does not ask and changes nothing. It reports the counts of a rebuild: every repository of the index removed, every repository found added.

### `loom repo register` and `forget`

`register [<PATH>]` (default: the current directory) finds the repository root at or above the path and registers it: `registered <slug>: <path>`. `-n`: `would register <root>`, and no request to loomd. `--json`: [`RepoSummary`](../schemas/repo_summary.json).

`register --on-use` sends the request as a [registration on use](#registration-on-use) (`on_use: true`). For a repository in the index, only its `last_used` is set. A repository that is not in the index is registered. The command never starts loomd, and fails if loomd does not run. Human output: `used <slug>: <path>`.

`register --on-use --background` is the registration on use itself. It waits for loomd at most 50 ms after it started. It prints nothing on stdout or stderr, whatever `-v` asks. It exits with `0` in each of these cases:

- loomd does not run (no socket file).
- loomd does not answer in time.
- No git repository is at or above the path.
- The configuration is invalid.

Only usage errors exit with `2`. `--background` requires `--on-use` and cannot be combined with `-n` or `--json`.

`forget <REPO>` removes the repository from the index without confirmation: `forgot <slug>: <path>`. `-n`: `would forget <slug>: <path>`. `--json`: the removed [`RepoSummary`](../schemas/repo_summary.json). A scan adds a repository under a root back. `register` adds any repository back.

### `loom repo doctor`

`loom repo doctor` runs the checks below, in this order. It prints one line for each check, with its fix indented below it:

```text
ok    config: /home/me/.loom/config.toml is valid
ok    roots: 3 roots exist
ok    contexts: 2 contexts, no overlapping match.paths
ok    loomd: loomd 0.2.0 running, pid 4242
ok    disk: 35 MB (versions 30 MB, index 4.2 MB, logs 25 KB, contexts 0 B) in /home/me/.loom
warn  disk-versions: 3 previous versions kept (30 MB), rollback needs at most 2: 0.1.0 is not needed
      fix: delete it (`rm -r /home/me/.loom/versions/0.1.0`), or let the next upgrade or rollback prune it
ok    disk-wal: index.db-wal is 4.1 MB, below 64 MB
ok    disk-free: 120 GB free on the volume of /home/me/.loom
warn  unassigned: 1 repository has no context: /home/me/src/tool
      fix: add a context that matches them to /home/me/.loom/config.toml, then restart loomd:
             [contexts.<name>]
             match.paths = ["~/src"]
           or match their remotes with `match.hosts`, or set `default_context = "<name>"`
ok    ssh-agent: loomd sees an agent socket at /run/user/1000/ssh-agent.socket
ok    home: loomd expands `~` to /home/me
ok    remote git@github.com (ssh, via personal/loom): reachable
fail  remote https://gitlab.example (https, via work/api): fatal: could not read Username for 'https://gitlab.example': terminal prompts disabled
      fix: git has no credential for https://gitlab.example and cannot prompt: store one with a credential helper (run `git -C /home/me/work/api ls-remote origin HEAD` once in a terminal with a helper configured), or switch the remote to SSH (`git -C /home/me/work/api remote set-url origin <ssh url>`)
warn  updates: remote not found (6): work/a, work/b, work/c, …
      fix: the project was moved or deleted upstream, or access to it was lost: update the remote (`git remote set-url`), or forget the clone (`loom repo forget <slug>`)
warn  updates: local changes (1): personal/notes
      fix: commit, stash or discard them, then `loom repo sync <slug>`
ok    machine: this machine is `laptop`; contexts held elsewhere, not loaded here: lab
ok    backup personal: ~/.loom/contexts/personal and git@github.com:me/personal-store.git: pushed 1 commit
fail  backup work: ~/.loom/contexts/work and /srv/work-store.git: diverged: 1 local and 2 remote commits (nothing merged)
      fix: loom never merges a store: in ~/.loom/contexts/work, `git fetch backup`, then merge or rebase onto backup/main by hand, then `loom sync -c work`
warn  tasks: ~/.loom/contexts/work/tasks/k3q7zm2a/task.md (work): line 3: unknown status `blocked`
      fix: edit ~/.loom/contexts/work/tasks/k3q7zm2a/task.md to fix the file; loomd reads it again on its own
```

| Check | Status |
| --- | --- |
| `config` | `fail` when `config.toml` cannot be read or is invalid. Then the checks of the roots and contexts are skipped. |
| `roots` | One `warn` per scan root (`[repos] roots`, then each `match.paths`, `~` expanded) that is not a directory, with its origin; otherwise one `ok`. |
| `contexts` | `fail` when `match.paths` entries of two contexts are the same directory once `~` is expanded, or two `[repos."<path>"]` keys name the same repository: loomd refuses such a configuration. Nested entries of different contexts are valid (the longest prefix wins) and are listed in the `ok` line. |
| `store paths` | `fail` when two contexts have the same store directory once the defaults (`<loom home>/contexts/<name>`) are applied, for example a `store.path` that is the default directory of another context. Configuration alone refuses two equal `store.path` entries. Paths are compared after symbolic links are resolved (for the directories that exist), and without case on macOS. Otherwise `ok`. |
| `loomd` | `warn` when loomd does not run: the checks below are skipped, and loomd is not started. `fail` when it answers with an error. |
| `disk` | The disk usage loomd measured ([`DiskUsage`](../api.md#diskusage)): `ok` with the total and its parts. `warn` when a part could not be measured (counted as 0), with the list of warnings. `warn` when loomd is older than this field. Then the other `disk-*` checks are skipped. |
| `disk-versions` | `warn` when `versions/` holds a version that `loom rollback` does not need, with the names of these versions and how to delete them. The retention rule of the upgrades applies: the 2 highest versions other than the version that loomd runs are kept, and a copy of the version that loomd runs is not needed. If the version of loomd is not a semantic version, the rule uses the count only. Directories not named after a version are ignored. |
| `disk-wal` | `warn` when `index.db-wal` is larger than 64 MB (64,000,000 bytes): SQLite did not copy it into `index.db` yet. The fix is to restart loomd. |
| `disk-free` | `warn` when less than 1 GB (1,000,000,000 bytes) is free for an unprivileged process on the volume of the loom home. Absent when the free space is unknown (the `disk` check then warns). |
| `unassigned` | `warn` with the count, one example and the configuration that would assign it. |
| `ssh-agent` | `SSH_AUTH_SOCK` in loomd's environment ([`GET /v1/diagnostics`](../api.md#daemondiagnostics)): `ok` when it is a socket. Otherwise, `fail` when an indexed remote uses SSH, `warn` when none does. The fix depends on the platform (systemd: `systemctl --user import-environment SSH_AUTH_SOCK`, then restart; launchd: `launchctl kickstart -k gui/$(id -u)/io.github.gsuquet.loomd`). `warn` when loomd is older than this endpoint. |
| `home` | `warn` when loomd's `HOME` differs from the one of `loom`: `~` in the configuration then expands elsewhere. |
| `remote <endpoint> (<scheme>, via <slug>)` | One per distinct endpoint and scheme of the primary remotes of the index, from [`POST /v1/diagnostics/remote-check`](../api.md#remote-check): loomd runs `git -C <repo> ls-remote --exit-code <primary remote> HEAD` in one repository of the endpoint. It uses the environment, git configuration (`core.sshCommand`, `includeIf`, credential helpers) and non-interactive settings of its updates. Endpoint: `<user>@<host>[:<port>]` for SSH (user `git` when the URL has none, an SSH alias as written), `<scheme>://<host>[:<port>]` for HTTP(S) and `git://`, the URL of a local or other remote. Scheme: `ssh`, `https`, `http`, `git`, `local`, `other`. `ok` (`reachable`) when the remote answers, even without `HEAD`. `fail` otherwise. The detail is the last line of the git error output that is not the generic git text `Could not read from remote repository`. The fix matches the kind of failure: SSH authentication (agent, `core.sshCommand`, host key), HTTPS authentication (credential helper, or switch the remote to SSH), not found (update the remote or forget the clone), network (no answer within 20 s, name resolution, connection: network or VPN), other (run the same `git ls-remote` in a terminal). One `ok` when no repository has a remote. `warn` when loomd is older than the endpoint. |
| `forge <context> <host>` | One per context held on this machine and forge host with a credential ([forges](../configuration.md#forges)), from [`POST /v1/diagnostics/forge-check`](../api.md#forge-check): loomd gets the token (`token_cmd` or `token_env`), then calls `GET /user` of the forge API with it. `ok` when the token can publish reviews (GitHub: the `repo` scope in `x-oauth-scopes`; GitLab: the `api` scope from `GET /personal_access_tokens/self`). `warn` "cannot publish" when the token can only read, with the scope to add. `warn` "scopes unknown" when the forge does not give them (a GitHub fine-grained token, the OAuth token of `glab`). `fail` when the token cannot be resolved: the command is not found (the fix gives the absolute path of the program in the `PATH` of `loom`, as loomd runs with a minimal `PATH`), fails, gives no output, or runs more than 10 s; the variable is unset or empty. `fail` when the forge refuses the token (`401`: create a new token), when the token is not authorized for the SAML single sign-on of a GitHub organization (the fix gives the authorization URL of `x-github-sso`), or when the API cannot be reached. The detail and the fix never contain the token. One `ok` when no context has a credential. `warn` when loomd is older than the endpoint. |
| `machine` | `ok`, naming this machine (the `id` of [`machine.toml`](../configuration.md#machinetoml)) and the contexts whose `machines` does not name it ([`GET /v1/tasks/backup`](../api.md#tasks)). `warn` when loomd is older than this endpoint. |
| `backup <context>` | One per context store held here. Without `store.backup.remote`: `ok` (commits stay local). Before the first sync since loomd started: `ok`. After a sync that left the store and its remote equal (`up to date`, `pushed`, `pulled`): `ok`. `fail` when they diverged, or when the remote refused the push (the commits stay local), with the fix. `warn` when a step failed (`commit`, `remote`, `fetch`, `compare`, `push`, `fast-forward`), or when changes of the store are not committed, with git's error. |
| `tasks` | From [`GET /v1/tasks/health`](../api.md#tasks). One `warn` per task file that loomd cannot read (path of `task.md`, context, error with the line), per directory that loomd cannot scan or watch, and one when the watcher does not run, each with its fix. At most 20 `warn` lines, then `and <n> more problems` (`loom reindex` lists every invalid file). One `ok` when there is no problem. `warn` when loomd is older than this endpoint; `fail` when loomd answers with another error. |
| `updates` | The last update of every repository ([`last_update`](../api.md#lastupdate)), skips asked for (`policy_off`, `no_remote`) left out: one `warn` per reason, as `<reason> (<count>): <slug>, <slug>, <slug>, …` (at most 3 slugs, sorted), with its fix. Reasons, in this order: `fetch refused by SSH`, `fetch without HTTPS credential`, `remote not found`, `remote unreachable`, `fetch failed` (a failed fetch, classified from git's error output as for `remote`), `fast-forward failed`, `default branch move failed`, `repository unreadable`, `repository busy`, `state unknown`, `operation in progress`, `local changes`, `HEAD detached`, `branch without commit`, `branch without upstream`, `diverged from upstream`, `fast-forward refused by git`, `default branch diverged`. One `ok` when there is none. |

`--json`: [`DoctorReport`](../schemas/doctor_report.json): `checks`, each with `check`, `status` (`ok`, `warn`, `fail`), `detail` and `fix` (absent when there is nothing to fix).

### Exit behavior

| Code | Cases |
| --- | --- |
| `0` | Success, including `-n`; `doctor` without any `fail`; `register --on-use --background` always. |
| `1` | loomd cannot be started, does not run (`register --on-use`), or answers with an error; `<REPO>` not found, ambiguous, not inside a git repository, or (for `forget`) not in the index; `register` outside a git repository; `--rebuild` refused or cancelled; `doctor` with at least one `fail` (after printing the report: `Error: 1 check failed`, `Error: <n> checks failed`). |
| `2` | Usage error: missing subcommand or `forget` argument, `--json` with `--format`, unknown `--format`. |

## `loom sync`

`loom sync` syncs the [context stores](../configuration.md#store-backup) held on this machine with their `store.backup.remote`, through loomd ([`POST /v1/tasks/sync`](../api.md#tasks)). It syncs every store that has a `store.backup.remote`, or the store of the context that `-c` names. If loomd does not run, `loom sync` starts it. It waits for every sync, at most 600 s. It prints one line for each store synced, `<context>: <outcome>`, and the changes not committed:

```text
personal: pushed 2 commits
work: diverged: 1 local and 2 remote commits (nothing merged)
  uncommitted changes: git commit failed: exit status: 1: …
```

| Outcome | Meaning |
| --- | --- |
| `up to date` | The store and its remote hold the same commits. |
| `pushed <n> commits` | The local commits were pushed. |
| `pulled <n> commits` | The store was fast-forwarded to the remote. |
| `diverged: <n> local and <m> remote commits (nothing merged)` | Both sides have commits the other lacks: nothing was pushed, pulled or merged. |
| `push rejected, commits kept local: <git error>` | The remote refused the push. |
| `<step> failed: <error>` | A step failed: `commit`, `remote`, `fetch`, `compare`, `push`, `fast-forward`. |

If no store held here has a `store.backup.remote`, `loom sync` prints `No context store has a store.backup.remote.`

`--json`: [`BackupReport`](../schemas/backup_report.json), the stores synced only.

| Code | Cases |
| --- | --- |
| `0` | Every store synced is equal to its remote and has its changes committed. |
| `1` | A store is not synced (after printing the report: `Error: 1 context store is not synced: …`); `-c` names a context without `store.backup.remote`, not held on this machine, or unknown; loomd cannot be started or answers with an error. |
| `2` | Usage error. |

### `loom repo sync`

`loom repo sync [<REPO>...] [-n] [--json]` asks loomd for an update of repositories now ([`POST /v1/repos/sync`](../api.md#updates)). The update is the same as on the schedule of loomd ([how updates work](../../explanation/repo-updates.md)). It fetches the primary remote. It fast-forwards the branch that `HEAD` is on, and the default branch, only under the conditions of the update policy. It ignores the schedule and the backoff of unreachable hosts.

`loom` resolves each `<REPO>` as [repository argument](#repository-argument) describes, and each `<REPO>` must be in the index. Without `<REPO>`, the update applies to every repository of the index. Like `ls`, the command starts loomd when necessary and waits for its scan on start. It never registers the repository of the current directory on use.

The command waits until every update ends (at most one hour). Updates run `fetch_concurrency` at a time.

#### Human output

One line per repository, sorted by slug: the slug, two spaces, then what happened. Several outcomes of one update are separated by `; `.

```text
$ loom repo sync
personal/dotfiles  fetched
personal/loom  fast-forwarded main 1a2b3c4..5d6e7f8
personal/notes  skipped: 2 changed tracked paths
personal/site  fetched; moved default branch main 0a1b2c3..4d5e6f7
unassigned/scratch  skipped: no remote
work/api  failed: fetch: timed out after 120s
work/legacy  skipped: update policy is off
```

| Outcome | Meaning |
| --- | --- |
| `fetched` | The primary remote was fetched; no branch had to move. |
| `fast-forwarded <branch> <from>..<to>` | The branch `HEAD` is on was fast-forwarded (commit ids abbreviated to 7 digits). |
| `moved default branch <branch> <from>..<to>` | The local default branch, checked out nowhere, was fast-forwarded. |
| `skipped: <reason>` | A step was not done; the reasons are listed in [`SkipReason`](../api.md#lastupdate). |
| `failed: <step>: <error>[: <last line of git's stderr>]` | A git command failed or timed out; `step` is `fetch`, `fast_forward` or `move_default_branch`. |

With `-n`/`--dry-run`, nothing is fetched or merged. Each line says what an update would do, from the state that is read now. A branch move depends on what the fetch brings, so the dry run does not predict it (`then decide`). The exception is when the working tree already prevents a fast-forward of the branch that `HEAD` is on. The dry run never predicts a move of the default branch.

```text
$ loom repo sync -n
personal/loom  would fetch, then decide
personal/notes  would fetch, skip: 2 changed tracked paths
work/docs  would fetch
work/legacy  skip: update policy is off
```

#### JSON output

`loom repo sync --json`: [`SyncResult`](../schemas/sync_result.json), as returned by the API.

#### Exit status

| Code | Cases |
| --- | --- |
| `0` | Every repository was handled, whatever its outcome (skipped and failed updates included). |
| `1` | loomd cannot be started or reached, a slug names no repository (`not_found`), or the request fails. |
| `2` | Usage error. |

## `loom search`

`loom search <QUERY>...` searches the repositories of the index through `GET /v1/search` (see [search](../api.md#search)). Like `loom repo ls`, it starts loomd if loomd does not run. The command joins the words with spaces: `loom search lo api` and `loom search "lo api"` are the same query. `-c <NAME>` keeps the repositories of one context. `--limit <N>` (default `20`, at least `1`) sets the maximum number of results.

Match rules: each word of the query must start a word of the name, slug, path, remote URLs, context or branch of the repository. Words are split at each character that is not a letter or a digit. Case and diacritics are ignored. Characters such as `"`, `*`, `(`, `:` and words such as `OR` or `NOT` are searched as text, never as operators.

Rank order, best first:

1. The name is the query (`loom-api` for `loom api`).
2. Each query word starts a word of the name.
3. Each query word starts a word of the slug.
4. The other matches (path, remotes, context, branch, or words in several fields).

In each rank, the order is the most recently used repository first, then full-text relevance, then slug. The last use is `last_used`, which registration on use and `loom repo show` set. A repository that was never used comes last.

Human output: a header, then one row per result, best first, columns aligned, the path last. `MATCHED` lists the fields that contain at least one query word (`name`, `slug`, `path`, `remotes`, `context`, `branch`). If nothing matches, nothing is printed, and the exit code is still `0`.

```text
SLUG               MATCHED         PATH
personal/loom      name,slug,path  /home/me/perso/loom
personal/loom-api  name,slug,path  /home/me/perso/loom-api
work/tool          remotes         /home/me/work/tool
```

`--json`: [`SearchResults`](../schemas/search_results.json): `results`, in the same order, each with `kind` (`repo`), `slug`, `name`, `context`, `path`, `score` and `matched_fields`.

| Code | Cases |
| --- | --- |
| `0` | Success, including no result. |
| `1` | loomd cannot be started or answers with an error. |
| `2` | Usage error: no query, `--limit` below `1` or not a number. |

## `loom shell-init`

`loom shell-init <SHELL>` prints on stdout a script to evaluate in the startup file of the shell (`eval "$(loom shell-init zsh)"` in `~/.zshrc`, `eval "$(loom shell-init bash)"` in `~/.bashrc`). The script is fixed text embedded in the binary. The command reads no configuration, never sends a request to loomd, and sends no [registration on use](#registration-on-use). Setup: [shell integration](../../how-to-guides/shell-integration.md).

If `command -v loom` finds no `loom`, the evaluated script does nothing. Otherwise, it installs a hook. The hook runs `loom repo register --on-use --background` for the current directory, at most once per directory (`$PWD`, symbolic links kept) in each shell session:

| Shell | Hook | Runs |
| --- | --- | --- |
| `zsh` | `_loom_chpwd`, added to `chpwd_functions` with `add-zsh-hook`; directories seen in the associative array `_loom_seen`. | When the script is evaluated (the start directory), then on each directory change. |
| `bash` (3.2 or later) | `__loom_prompt`, appended to `PROMPT_COMMAND` on a line of its own (an existing value is kept, and if the script is evaluated twice, the hook is added once); directories seen in the string `__loom_seen`, last directory in `__loom_pwd`. | At each prompt where `$PWD` differs from the previous prompt's, the first prompt included. Returns the exit status it was called with. |

The hook starts `loom` detached (zsh `&!`, bash `( … & )`), with stdin, stdout and stderr on `/dev/null`. Thus no job is created, nothing is printed, and the prompt does not wait. The hook runs no `git`. `loom` finds the repository root at or above the directory, and does nothing outside a repository. If `loom` is no longer on `PATH`, the hook fails silently.

| Code | Cases |
| --- | --- |
| `0` | The script is printed. |
| `2` | Missing or unsupported shell. |

## `loom prompt`

`loom prompt` prints one line for a shell prompt: `<context>`, or `<context> <prefix> <title>` when the context has a focus task. `<prefix>` is the shortest unique prefix of the task ID. Setup: [add the context and the focus task to the prompt](../../how-to-guides/shell-integration.md#add-the-context-and-the-focus-task-to-the-prompt).

The context is the first of:

1. `-c`/`--context`.
2. `LOOM_CONTEXT`, if it is not empty.
3. The context of the indexed repository that contains the current directory, as for the [task commands](#context).
4. The `match.paths` prefix of the current directory.
5. `default_context`.

These are the differences from the task commands:

- An invalid `LOOM_CONTEXT` gives no context. It is not an error.
- If the current directory is in a git repository that `loomd` does not have in its index, there is no context. The repository could give another context than the rules of its path. `loom prompt` does not register the repository. The [shell integration](#loom-shell-init) registers it.
- If `loomd` does not run, `loom prompt` does not start it. Outside a git repository, the line has the context and no focus task. In a git repository, the line is empty.

`loom prompt` sends at most two requests to `loomd`, at the same time: `GET /v1/focus`, and `GET /v1/repos` when the current directory is in a git repository and the context depends on it. It waits for `loomd` at most 100 ms after it started, connection included. If `loomd` does not answer in that time, `loom prompt` continues without the answer. The time to read `config.toml` and to find the repository of the current directory is not in this limit. It reads no `[telemetry]`, exports no trace, and writes no log. It sends no [registration on use](#registration-on-use).

| Option | Effect |
| --- | --- |
| `--max-title <N>` | Maximum number of characters of the title, 30 by default. A longer title is cut, and its last character is `…`. With `0`, the line has no title. |
| `--shell zsh` | Each `%` becomes `%%`. Thus zsh prompt expansion shows `%` as it is. |
| `--shell bash` | No change. bash does not expand the output of a command substitution in `PS1` again. |
| `--json` | The `PromptSegment`: `context` and `focus` (a `FocusEntry`), each absent when unknown. Not with `--shell` or `--max-title`. |

Control characters in the prefix and the title (for example a line feed or an escape character) become spaces. Invisible characters that can reorder or hide text also become spaces: U+200B to U+200F, U+2028 to U+202E, U+2060 to U+2069 and U+FEFF. `--max-title` counts Unicode scalar values, not columns.

With the zsh option `PROMPT_BANG`, zsh shows a `!` of the title as the history number. `--shell zsh` does not change `!`. Without a context, the output is empty (`{}` with `--json`).

| Code | Cases |
| --- | --- |
| `0` | Every case, with nothing on stderr: a failure gives a shorter line or an empty one. |
| `2` | Usage error: unknown option or invalid value. |

## Build-time variables

| Variable | Values | Effect |
| --- | --- | --- |
| `LOOM_BUILD_VERSION` | semver string | Overrides the reported version (dev prereleases such as `0.1.0-dev.42`). |
| `LOOM_CHANNEL` | `stable`, `dev`, `local` | Sets the reported channel. Any other value fails the build. |
