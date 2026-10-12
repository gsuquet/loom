# ADR-0011: Auto-upgrade with pre-flight, watchdog and automatic rollback

- Status: accepted
- Date: 2026-10-12

## Context

Issue #12: loomd keeps itself up to date according to `config.toml`, and a release whose daemon does not come up is rolled back without the user's action. The brief requires that **the rollback never depends on the new binary being able to run**, that a failed version is never retried automatically, and that `policy = "off"` never touches the network.

#11 already provides the building blocks in `loom-update`: discovery and decision (`decide`, which takes `failed_versions`), verified download into `staging/`, the rename-based swap, the copies of previous versions in `versions/<version>/`, and `SIGTERM` to the loomd holding `run/loomd.lock`. ADR-0009 left the health check and the rollback mechanism to this task.

The difficulty: after the swap, the process that must judge the new loomd cannot be the new loomd (it may not start), and cannot be the old loomd either (it has to exit, to release the single-instance lock and let the new binary start).

## Decision

### Two layers of checks

1. **Pre-flight, before any swap.** The running loomd downloads and verifies the release (#11 code), then starts the **staged** `loomd` with `--loom-home` pointing at a throwaway directory under `/tmp` (`/tmp/loom-preflight-*`: short, so the socket path stays under the `sun_path` limit), waits up to 15 s for `GET /v1/status` on that home's socket, checks that the reported `daemon.version` is the release version, then kills it and deletes the directory. A failure (exit, no answer, other version) records the version in `failed_versions` and nothing is swapped. The throwaway home has no `config.toml` and the child does not inherit `LOOM_UPDATE_CHECK_INTERVAL`, so it never checks for updates itself.
2. **Post-restart watchdog, after the swap.** The old loomd saves the installed binaries in `versions/<old>/` (#11), swaps, then spawns a detached watchdog **from the saved old binaries**: `versions/<old>/loom __upgrade-watchdog --from <old> --expect <new> --deadline <secs> --install-dir <dir>`, in its own process group, stdin and stdout closed, stderr appended to `log/loomd.log`. Then the old loomd shuts down gracefully (exit code 0). The watchdog runs code of the version that was running before, which is known to work.

The watchdog:

- polls the socket until a loomd answers with `daemon.version == <new>`, for at most the deadline (default 60 s);
- when nobody holds `run/loomd.lock` for a grace period (deadline / 4, at most 15 s), starts `<install dir>/loomd` detached itself, as `loom status` does. Under a service manager (launchd `KeepAlive`, systemd `Restart=always`) the manager restarts loomd first and the grace period never elapses;
- **success**: clears `pending`, records `last_result = upgraded`, exits 0;
- **failure**: copies `versions/<old>/loom` and `loomd` back into the install directory (same rename-based swap), adds `<new>` to `failed_versions`, records `last_result = rolled_back`, deletes `versions/<old>/` (it duplicates the running binaries again) and keeps the two highest other versions, sends `SIGTERM` to a loomd still holding the lock (the unhealthy new one), then waits, with the same deadline and grace rules, for a loomd answering `<old>`, starting it detached when nobody restarts it. Exits 1 after any rollback (nobody reads the code of a detached process; the error status marks the trace); the binaries are restored even when the old loomd does not come back.

`__upgrade-watchdog` is an internal subcommand: hidden from `loom --help` and from the generated CLI reference (`cli_doc` skips hidden commands). It is not a stable interface; it only has to match the binaries of its own version, which saved it.

### Rollback does not save the failed version

The watchdog restores with `install::replace_binaries` only: the failed binaries are not copied into `versions/`, so `loom rollback` never lands on them later.

### Versions bookkeeping

After any swap (`loom upgrade`, `loom rollback`, the auto-upgrade, the watchdog's rollback), `versions/<version now running>/` is deleted: it is a copy of the installed binaries and would take one of the two kept slots. The only exception is a build of the same version replacing another (`--from`): that directory was just saved and holds the previous build. Retention then keeps the version just saved and the highest others, two in all.

### Traces

With `[telemetry]` enabled, an auto-upgrade gives two traces. In loomd, the root span `update.check` (outcome, versions) with the children `update.decide`, `update.download` (itself parent of one `update.verify` per verification: signature, checksum, version), `update.preflight` and `update.swap`. In the watchdog, the command span `loom __upgrade-watchdog` with the child `update.watchdog` (outcome `upgraded`, `rolled_back` or `rollback_failed`). Both processes use the subscriber of `loom-telemetry`, and every HTTP request goes through the `loom_update::github` client and its shared TLS configuration (`loom_update::tls`). The two traces are not linked: the watchdog is a new process started after the check span ended.

### State

`<loom home>/state/update.json` (directory `0700`, file `0600`), type `UpdateState` in `loom-api` with a committed JSON Schema:

| Field | Meaning |
| --- | --- |
| `last_check` | Instant of the last check, successful or not. |
| `available` | Version found by the last check under `notify`. |
| `pending` | Upgrade in progress: `from`, `to`, `started_at`. Set before the download, cleared by the pre-flight failure, the watchdog, or any failure before the swap. |
| `failed_versions` | Versions that failed the pre-flight or the watchdog. Passed to `decide`: never applied automatically again. |
| `last_result` | Outcome of the last check or upgrade (tagged by `kind`). |

Writes are read-modify-write under an exclusive `flock` on `state/update.lock`, then an atomic write (temporary file in `state/`, `fsync`, `rename`). Readers (the status endpoint) read without the lock: the rename gives them a whole file. Unknown fields are accepted, so an older watchdog can read a file written by a newer loomd; an unreadable file is logged and replaced by an empty state (the failed versions are then lost: this is preferred over a daemon that never checks again).

A `pending` older than one hour is stale (the watchdog was killed, the machine rebooted): the next check clears it and records `interrupted`.

### Concurrency

- One upgrade at a time in loomd: a single updater thread runs checks and upgrades, sequentially; it is the in-process guard (no separate flag is needed while nothing else in loomd can start an upgrade).
- Across processes: `pending`. `loom upgrade` and `loom rollback` refuse while a non-stale `pending` exists, with a message naming both versions; a new loomd skips its checks while `pending` exists.
- Graceful shutdown of loomd waits for an in-progress swap (a mutex held during the swap), so a `SIGTERM` never leaves `loom` and `loomd` from different versions.
- The install directory (the directory of the running `loomd`, symbolic links resolved) must be writable before anything is downloaded; otherwise the check records `failed` with the reason and logs a warning.

### Configuration

```toml
[update]
policy = "auto"         # "auto" | "notify" | "off"
channel = "stable"      # "stable" | "dev"
check_interval = "6h"   # at least 10m
```

- `policy` default: `auto` for release builds (channel `stable` or `dev`), `notify` for local builds. The issue's goal is that installed releases keep themselves current; a local build is a developer's binary, often inside a cargo `target/` directory, that an automatic install would overwrite. The default is a function of the build channel only, so it stays a one-line rule.
- `channel` default: the build channel; a local build has none, so without `channel` it never checks (effectively off, no network).
- `check_interval`: a duration string made of `<integer><unit>` parts, units `s`, `m`, `h`, `d` (`"6h"`, `"1h30m"`), at least 10 minutes, validated when the file is parsed. Default 6 h.
- Scheduling is a pure function `next_check(last_check, started_at, interval, jitter, random)`: `last_check + interval ± 10 %`, but never earlier than `started_at + min(5 min, interval) ± 10 %`, so a restart never checks at boot. The random number is drawn by the binary (`std::hash::RandomState`), not by the function.
- `off` never builds an HTTP client.

`loom upgrade` (manual) keeps its #11 behaviour and adds: refusal while `pending`, and `failed_versions` passed to `decide` for channel lookups. `loom upgrade --version X` ignores `failed_versions`, so a user can retry a failed version deliberately.

### systemd unit: `KillMode=process`

systemd kills every process of a service's control group when the main process exits, including a detached watchdog in its own process group. The generated unit gets `KillMode=process`, so only `loomd` itself is stopped. Users who installed the service before this change rerun `loom service install`.

### launchd property list: unchanged

launchd.plist(5), key `AbandonProcessGroup`: "When a job dies, launchd kills any remaining processes with the same process group ID as the job. Setting this key to true disables that behavior." The watchdog is started with `process_group(0)`, so it has its own process group and is not killed when the old loomd exits; the property list needs no change. Source: the `launchd.plist(5)` manual page, text checked on 2026-10-12 on macOS (Darwin 25.6). The behaviour itself is to be verified on a real machine with the service installed (not covered by the tests, which never touch the host's launchd).

### Test hooks

Debug builds only (ignored by release builds, like `LOOM_UPDATE_PUBKEY`):

| Variable | Effect |
| --- | --- |
| `LOOM_UPDATE_CHECK_INTERVAL` | Replaces `check_interval`, without the 10 min minimum. |
| `LOOM_UPDATE_HEALTH_DEADLINE` | Replaces the 60 s watchdog deadline. |
| `LOOM_TEST_BUILD_VERSION` | Replaces the version both binaries report. |
| `LOOM_TEST_FAIL_UNLESS_HOME_PREFIX` | loomd exits with code 1 at start unless its loom home path starts with this prefix. |

The integration tests build release archives whose `loom` and `loomd` are shell wrappers that set `LOOM_TEST_BUILD_VERSION=99.0.0` (and, for the "fails after restart" case, `LOOM_TEST_FAIL_UNLESS_HOME_PREFIX=/tmp/loom-preflight-`, which passes the pre-flight and fails in the real home) and `exec` the real built binaries. A second `cargo build` with another `LOOM_BUILD_VERSION` into a separate target directory was rejected: a cold build of the dependency tree takes minutes per test run.

## Consequences

- A release whose loomd cannot start, crashes, or answers another version is rolled back within the deadline plus the grace period, by code that predates it.
- The pre-flight catches most broken releases (missing dynamic libraries, wrong architecture, crash at start) without touching the install.
- A broken release whose daemon starts in a fresh home but not in the user's home (configuration or state migration) is caught by the watchdog only.
- One upgrade costs a few seconds of downtime of the API (old loomd exit, new start).
- Unsupervised loomd: the watchdog restarts it after the grace period (up to 15 s of downtime).
- Supervised by launchd with `KeepAlive`: launchd throttles restarts of a job that ran less than 10 s (`ThrottleInterval`); the grace period (15 s with the default deadline) is longer, so the watchdog does not race launchd. With a shorter deadline (test hook) both may start a loomd; the loser exits on the lock.
- The watchdog trusts the version reported by `/v1/status`; a release that serves the endpoint but is broken elsewhere is not detected.
- If the watchdog itself is killed (reboot during the deadline), `pending` stays until it is stale (1 h); the broken binaries stay installed until `loom rollback`.
- `loom` and `loomd` are assumed to be installed in the same directory (as `install.sh` does): the auto-upgrade replaces both in the directory of the running `loomd`.

## Unverified points

1. systemd's default `KillMode=control-group` killing the detached watchdog when `loomd` exits under `Restart=always`, and `KillMode=process` preventing it (systemd.kill(5), not re-read in this task).
2. launchd killing only the job's process group, not its other descendants (launchd.plist(5) `AbandonProcessGroup`, quoted above from the manual page; behaviour with the service installed to be verified on a real machine).

Neither is covered by automated tests: the tests never touch the host's launchd or systemd.
