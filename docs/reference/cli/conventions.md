# CLI conventions

> Behavior shared by every command of `loom` and `loomd`, as shipped. Commands and flags: [loom.md](loom.md), [loomd.md](loomd.md) (generated).

## Syntax

- `-h`/`--help` on every binary and command, `-V`/`--version` on every binary.
- Short flags can be grouped: `-yv` is `-y -v`.
- Values are given as `--opt value` or `--opt=value`, `-c value` or `-cvalue`.
- `--` ends option parsing.
- Global options of `loom` are accepted before or after the command name: `loom -c personal version` and `loom version -c personal` are equivalent.

## Global options

| Option | Behavior |
| --- | --- |
| `-c`, `--context <NAME>` | Context to work in. `NAME`: 1 to 64 characters, lowercase ASCII letters, digits and `-`, starting with a letter, not ending with `-`. A path (contains `/`, starts with `.` or `~`) or a `key=value` pair is rejected with a usage error. |
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

No shipped command asks for confirmation yet.

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

`loomd` stopped by `SIGTERM` (a service manager stopping it, or `kill`) exits with `0` after a graceful shutdown; stopped by `SIGINT`, it exits with `130` after the same shutdown.

## Environment variables

| Variable | Effect |
| --- | --- |
| `NO_COLOR` | Non-empty: no color with `--color auto`. |
| `LOOM_HOME` | Non-empty: loom home directory of `loomd`, unless `--loom-home` is given, and of `loom`. See [configuration](../configuration.md). |
| `PATH` | Searched for `loomd` by `loom status` when no `loomd` is next to the `loom` executable. |

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

`loomd --version` prints the same build information prefixed with `loomd`; `loomd -V` prints only `loomd <version>`. Both binaries built together report identical build information.

## `loom status` output

`loom status` queries `GET /v1/status` on the socket of the loom home (`LOOM_HOME`, otherwise `~/.loom`; `loom` has no `--loom-home` flag). Each request times out after 5 s.

### Starting loomd

When nothing accepts connections on the socket (no socket file, or no process listening on it), `loom status` starts `loomd`, unless `--no-start` is given:

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
```

### JSON output

`loom status --json` prints the `StatusResponse` returned by loomd, unchanged, as pretty JSON followed by a newline. Fields: [API reference](../api.md#statusresponse); schema: [`status_response.json`](../schemas/status_response.json).

### Version mismatch

When the `version` or `commit` of the daemon differs from the `loom` binary's, `loom status` prints a warning on stderr (hidden by `-q`) and exits with `0`:

```text
 WARN loom 0.1.0-dev.43 (commit 89abcdef0123) and the running loomd 0.1.0-dev.42 (commit 0123456789ab) are different builds: restart loomd (PID 4242) to run the same build
```

### Exit codes

| Code | Case | Message on stderr |
| --- | --- | --- |
| `0` | The daemon answered, including after a version mismatch warning. | |
| `1` | `--no-start` and the daemon is not running. | ``Error: loomd is not running: cannot connect to <socket>: <cause>; run `loom status` without --no-start to start it`` |
| `1` | No `loomd` next to `loom` or in `PATH`. | `Error: cannot start loomd: no loomd next to <loom executable> or in PATH; install loomd next to loom` |
| `1` | The started `loomd` exited, or did not answer within 5 s. | `Error: cannot start loomd: <reason>; last lines of <loom home>/log/loomd.log:` followed by up to 5 lines that this start wrote to the log. |
| `1` | The daemon answered with an error. | `Error: loomd returned an error (HTTP <status>, <code>): <message>` |
| `1` | The daemon answered something that is not the API: invalid body, error status without an `ApiError` body, no answer within 5 s. | `Error: unexpected answer from loomd: <detail>; check that the process listening on <socket> is a loomd of the same version as loom` |
| `1` | The socket path is too long. | See [socket path length](../configuration.md#socket-path-length). |
| `2` | Usage error. | |

`loomd` exits with code `1` both on a failure and when another `loomd` holds the lock. When the started `loomd` exits with `1`, `loom status` probes `run/loomd.lock` without blocking (and releases it at once):

- Nobody holds it (or the file is missing): no other `loomd` is starting, and `loom status` fails at once with the end of the log.
- Another process holds it: another `loomd` won the race. `loom status` keeps querying the socket until the 5 s end, and succeeds with that daemon's status if it answers.

A started `loomd` that is still running but did not answer within 5 s is left running.

## Build-time variables

| Variable | Values | Effect |
| --- | --- | --- |
| `LOOM_BUILD_VERSION` | semver string | Overrides the reported version (dev prereleases such as `0.1.0-dev.42`). |
| `LOOM_CHANNEL` | `stable`, `dev`, `local` | Sets the reported channel. Any other value fails the build. |
