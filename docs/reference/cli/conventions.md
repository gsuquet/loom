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
| `1` | Error. The message is printed on stderr. |
| `2` | Usage error: unknown option, missing command, invalid value, conflicting options. |
| `130` | Interrupted by `SIGINT` (Ctrl-C). |

`loomd` stopped by `SIGTERM` (a service manager stopping it, or `kill`) exits with `0` after a graceful shutdown; stopped by `SIGINT`, it exits with `130` after the same shutdown.

## Environment variables

| Variable | Effect |
| --- | --- |
| `NO_COLOR` | Non-empty: no color with `--color auto`. |
| `LOOM_HOME` | Non-empty: loom home directory of `loomd`, unless `--loom-home` is given. See [configuration](../configuration.md). |

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

## Build-time variables

| Variable | Values | Effect |
| --- | --- | --- |
| `LOOM_BUILD_VERSION` | semver string | Overrides the reported version (dev prereleases such as `0.1.0-dev.42`). |
| `LOOM_CHANNEL` | `stable`, `dev`, `local` | Sets the reported channel. Any other value fails the build. |
