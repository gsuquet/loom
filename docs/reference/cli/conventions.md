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

## Dry run

Commands that change state accept `-n`/`--dry-run`: they print on stdout what they would change and change nothing. Read-only queries still run. Shipped on `loom service install` and `loom service uninstall`.

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
| `PATH` | Searched for `loomd` by `loom status` and `loom service install` when no `loomd` is next to the `loom` executable. |
| `XDG_CONFIG_HOME` | Absolute path: `loom service` on Linux writes the systemd unit under `$XDG_CONFIG_HOME/systemd/user/` instead of `~/.config/systemd/user/`. |
| `LOOM_GITHUB_TOKEN`, `GITHUB_TOKEN` | Optional GitHub token for `loom upgrade`, first non-empty one wins. Raises the API rate limit. Never printed. |
| `LOOM_RELEASES_URL` | Replaces `https://api.github.com` in `loom upgrade` (test hook). Plain `http://` is then allowed. |
| `LOOM_UPDATE_PUBKEY` | Test hook, debug builds only: minisign public key (base64 key line) replacing the embedded release key in `loom upgrade`. Ignored by release builds. |
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

With `-n`/`--dry-run`, when something would change, stdout lists the steps instead, one per line, in order: `write <path>:` followed by the full file content, `create directory <path>`, `remove <path>`, `run <command>`. Arguments with spaces or quotes are shown single-quoted.

```text
write /Users/me/Library/LaunchAgents/io.github.gsuquet.loomd.plist:
<?xml version="1.0" encoding="UTF-8"?>
...
</plist>
create directory /Users/me/.loom/log
run launchctl bootstrap gui/501 /Users/me/Library/LaunchAgents/io.github.gsuquet.loomd.plist
```

Commands run, by case. "Loaded" and "running" are as in `status`.

| Platform | Command | Case | Commands that change state |
| --- | --- | --- | --- |
| macOS | `install` | No file, or different content | `launchctl bootout gui/<uid>/io.github.gsuquet.loomd` if loaded, then `launchctl bootstrap gui/<uid> <plist>` |
| macOS | `install` | Identical file, not loaded | `launchctl bootstrap gui/<uid> <plist>` |
| macOS | `uninstall` | Any | `launchctl bootout gui/<uid>/io.github.gsuquet.loomd` if loaded, then the file is removed |
| Linux | `install` | No file, or different content | `systemctl --user daemon-reload`, `systemctl --user enable --now loomd.service`, then `systemctl --user restart loomd.service` if it was running |
| Linux | `install` | Identical file, not enabled | `systemctl --user daemon-reload`, `systemctl --user enable --now loomd.service` |
| Linux | `uninstall` | Any | `systemctl --user disable --now loomd.service` if enabled or running; if the file exists, it is removed, then `systemctl --user daemon-reload` |

On macOS, `<uid>` comes from `id -u`, and `install` creates `<loom home>/log` (mode `0700`) before `bootstrap`. `install` creates the missing parent directories of the unit file.

When `bootstrap` follows a `bootout` in the same run (the plist changed while the service was loaded), launchd may still be unloading the service. That `bootstrap` is retried while it exits non-zero: up to 5 attempts, 200 ms apart, and the last attempt's stderr is reported if all fail. A `bootstrap` without a preceding `bootout` runs once.

`install` and `uninstall` never ask for confirmation.

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

A failing manager command:

```text
`launchctl bootstrap gui/501 /Users/me/Library/LaunchAgents/io.github.gsuquet.loomd.plist` exited with code 5: Bootstrap failed: 5: Input/output error
```

`install` reads `config.toml` before anything else: when refused, no command runs and no file is written. The steps of a plan run in order and stop at the first failure.

## `loom upgrade`

### What it does

1. Looks up a release through the GitHub REST API (`https://api.github.com`, or `LOOM_RELEASES_URL`). Server certificates are checked against the operating system's trust store (Keychain on macOS, the system CA bundle on Linux), so a TLS-inspecting proxy trusted by the system works. Proxy variables are honoured: `ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY` (first set wins) and `NO_PROXY`, upper or lower case.

   | Options | Request | Release considered |
   | --- | --- | --- |
   | `--channel stable`, or none on a stable build | `GET /repos/gsuquet/loom/releases/latest` | The latest stable release. |
   | `--channel dev`, or none on a dev build | `GET /repos/gsuquet/loom/releases?per_page=30` | Every non-draft release tagged `vX.Y.Z` or `vX.Y.Z-dev.N` in the 30 most recent; other tags are skipped. The highest by semver precedence wins, so a stable release above every dev build is installed. |
   | `--version X.Y.Z` | `GET /repos/gsuquet/loom/releases/tags/vX.Y.Z` | That release; a 404 means not released. |
   | `--from PATH` | none | The local build. |

   A local build (channel `local`) has no default channel: without `--channel`, `--version` or `--from`, `loom upgrade` fails.

2. Decides by semver precedence: a newer release is installed, the same version is up to date, and a `--version` lower than the running version is refused (use `loom rollback`). `--from` always installs, whatever its version.
3. Checks that the install directory is writable, before any download.
4. Downloads `SHA256SUMS` and `SHA256SUMS.minisig`, verifies the signature with the public key embedded at build time ([releases](../releases.md#signature)), then downloads `loom-<version>-<target>.tar.gz` and checks its SHA-256 against `SHA256SUMS`. A `*-linux-gnu` build takes the `*-linux-musl` archive. Any failure stops here: nothing installed has changed.
5. Extracts the archive into `<loom home>/staging/`. Only the regular files `loom`, `loomd`, `LICENSE` and `README.md` at the archive root are accepted; any other entry (directory, link, path) rejects the archive. Then runs `loom version --json` and `loomd -V` from the staging directory: both must report the release version.
6. Copies the installed `loom` and `loomd` into `<loom home>/versions/<running version>/`.
7. Replaces `loomd`, then `loom`, in the directory of the running `loom` (symbolic links resolved): each new file is written next to its target, synced, set to mode `0755`, then renamed over it. A missing `loomd` is created.
8. Keeps two previous versions in `versions/` (see [configuration](../configuration.md#versions)).
9. If a loomd holds `<loom home>/run/loomd.lock`, sends `SIGTERM` to the PID in the file (`kill -TERM`), without waiting. A service manager restarts it on the new binary. A PID in a lock file no process holds is stale and is not signalled.

`--from PATH` takes a `.tar.gz` archive with the release layout, or a directory holding `loom` and `loomd`. Its signature and checksum are not verified; a warning says so on stderr. Steps 5 to 9 apply.

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

The `removed` line appears only when saved versions were deleted. The last line is `loomd is not running` when no loomd holds the lock. With `--from`, a last line `source:   <path> (not verified)` is added. Progress (`downloading …`, `verified …`) goes to stderr, except with `-q`.

### JSON output

`loom upgrade --json`: [`UpgradeReport`](../schemas/upgrade_report.json).

| Field | Content |
| --- | --- |
| `current_version` | Version of the running `loom`. |
| `source` | `{"kind": "release", "channel": "stable" \| "dev" \| null, "pinned": "<version>" \| null}` or `{"kind": "local", "path": "<absolute path>"}`. |
| `decision` | `{"kind": "up_to_date"}`, `{"kind": "apply", "version": …}`, or `{"kind": "skipped", "reason": {…}}` with reason `kind` `pinned_not_released` or `pinned_below_current` (and `pinned`). `available`, `updates_off` and `failed` are defined for the daemon and not produced by `loom upgrade`. |
| `check`, `dry_run` | Flags given. |
| `install` | `null` with `--check` or when nothing is installed; otherwise an install report (below). |

Install report (also in `loom rollback`):

| Field | Content |
| --- | --- |
| `version` | Version installed, or that would be with `--dry-run`. |
| `install_dir` | Directory of the running `loom`. |
| `verified` | `true` for a release (signature and checksum verified); `false` for `--from` and `loom rollback`. |
| `installed` | `false` with `--dry-run`. |
| `saved_to` | `<loom home>/versions/<previous version>`; `null` with `--dry-run`. |
| `pruned` | Versions deleted from `versions/`. |
| `daemon` | `{"kind": "skipped"}` (dry run), `{"kind": "not_running"}`, `{"kind": "signalled", "pid": …}`, or `{"kind": "failed", "pid": … \| null, "message": …}`. |

### Exit status

| Code | Cases |
| --- | --- |
| `0` | Installed; up to date; `--check` whatever the decision; `--dry-run` verified. Also when loomd could not be signalled: a warning on stderr says it keeps running the replaced version. |
| `1` | Local build without `--channel`, `--version` or `--from`; `--version` not released or lower than the running version; install directory not writable; HTTP error (`401`, `403` and `429` name `LOOM_GITHUB_TOKEN`); missing asset; invalid signature; checksum mismatch; rejected archive; binaries not reporting the expected version; I/O error. Every download or verification error ends with `nothing was installed`. |
| `2` | Usage error, including `--channel`, `--version` and `--from` together, or `--check` with `--from` or `--dry-run`. |

## `loom rollback`

Restores the highest version in `<loom home>/versions/` other than the running one (semver precedence). Only directories holding both `loom` and `loomd` count. The saved `loom version --json` and `loomd -V` must report the directory's version. Then steps 6 to 9 of `loom upgrade` apply: the running version is saved, the binaries replaced, two previous versions kept, loomd signalled. A second rollback returns to the version rolled back from when no higher version is saved.

Human output: as `loom upgrade`, with `rolled back` (`would roll back` with `--dry-run`).

JSON output (`--json`): [`RollbackReport`](../schemas/rollback_report.json): `current_version`, `dry_run` and `install` (the install report above, `verified` always `false`).

| Code | Cases |
| --- | --- |
| `0` | Restored, or would be with `--dry-run`. |
| `1` | `nothing to roll back to: no saved version other than <version> in <dir>`; install directory not writable; saved binaries not reporting their version; I/O error. |
| `2` | Usage error. |

## Build-time variables

| Variable | Values | Effect |
| --- | --- | --- |
| `LOOM_BUILD_VERSION` | semver string | Overrides the reported version (dev prereleases such as `0.1.0-dev.42`). |
| `LOOM_CHANNEL` | `stable`, `dev`, `local` | Sets the reported channel. Any other value fails the build. |
