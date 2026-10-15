# How loomd upgrades itself

> This page tells about the check cycle, the pre-flight check, the watchdog, the rollback and the failed versions of the auto-upgrade of `loomd`. It also tells why they work this way. Keys and files: [configuration](../reference/configuration.md#keys). Decision: [ADR-0011](../adr/0011-auto-upgrade-rollback.md).

## The problem

An upgrade replaces the binary of a daemon that runs all the time. If the new release cannot start, nobody is there to see the problem. The daemon is gone, and with it the code that would restore the old version. Thus, the code that judges the new version and restores the old version must never be the new version itself.

## The check cycle

`loomd` runs one updater thread. The thread sleeps until the next check. Then it asks the GitHub releases API for the latest release of its channel. It decides with the same rules as `loom upgrade` (semver precedence, never downgrade, failed versions skipped).

- The first check runs about 5 minutes after `loomd` starts, never at once. Thus, a daemon that restarts in a loop does not send too many requests to the API.
- The next checks run every `check_interval` (6 h by default) after the previous one. Each delay changes by up to ±10 % at random, so that machines that started together do not check together.
- `policy = "off"`, or a local build without `channel`, never checks: the thread does not even create an HTTP client.
- Under `notify`, `loomd` records a newer version as `available` in `state/update.json`, and `loom status` shows it. `loomd` downloads nothing.
- Under `auto`, the upgrade starts.

Every outcome goes into `last_result` of `state/update.json`, which `loom status` prints.

### Which channel

The build channel alone does not tell which channel a machine must follow. The dev channel includes stable releases. Thus, a `dev` install moves to a stable release as soon as that release is higher than its dev build. The new binary is a `stable` build: if its build channel were the default, the machine would never see a dev build again. Thus, `loom upgrade --channel` and `install.sh --channel` (which runs it) write the explicitly chosen channel to `state/update.json`. Every version of the binaries shares this file.

`[update] channel` in `config.toml` still has priority. Thus, a configured machine never changes without the knowledge of its owner. No command ever writes `config.toml`. The build channel applies only when neither of the two exists. `loomd` reads the chosen channel before each check. `loom status` tells where its channel comes from (`config`, `chosen` or `build`).

## The upgrade

1. `loomd` records the upgrade as `pending` (from, to, start time). While it is there, `loom upgrade` and `loom rollback` refuse to run, and another `loomd` skips its checks.
2. It makes sure that it can write to the directory of its own binary. Then it downloads the release and verifies it exactly as `loom upgrade` does. It verifies the minisign signature of `SHA256SUMS` and the checksum of the archive. It also verifies that both binaries report the release version.
3. **Pre-flight check.** It starts the downloaded `loomd` with a temporary loom home under `/tmp`. It waits up to 15 s for an answer to `GET /v1/status` with the release version, then it kills the process. Its `HOME` is also the temporary directory, so the repository scan at start never goes through your real home. This check finds a release that cannot start at all (wrong architecture, library not found, crash at start), before any change to the installed files. The release goes into `failed_versions`, and the `loomd` that runs now continues.
4. It saves the installed `loom` and `loomd` in `~/.loom/versions/<running version>/` and replaces them, like `loom upgrade`.
5. It starts the **watchdog** from the saved copy: `~/.loom/versions/<running version>/loom __upgrade-watchdog …`. The watchdog runs detached and appends its output to `~/.loom/log/loomd.log`.
6. It stops itself (exit code 0).

## The watchdog

The watchdog is the `loom` of the previous version. This `loom` ran a moment ago, and it is known to work. It waits up to 60 s for a `loomd` to answer on the socket with the new version.

Someone has to start the new `loomd`:

- under `loom service` (launchd `KeepAlive`, systemd `Restart=always`), the service manager restarts it. The watchdog never starts one itself.
- otherwise, when nobody holds `run/loomd.lock` for 15 s, the watchdog starts it itself, as `loom status` does.

The watchdog counts the service only when the service file runs `loomd` for the same loom home (`LOOM_HOME`) and the service manager has it loaded. If the watchdog started a `loomd` next to a service, that `loomd` would hold the lock. Every start of the service would then fail on the lock: launchd tries again every 10 s, and systemd does not try again. The `loomd` that runs would have no supervisor.

When the new `loomd` answers in time, the watchdog records `upgraded` and exits.

## The rollback

When the deadline passes without an answer from the new version (it crashed, it hangs, or it reports another version), the watchdog:

1. copies the saved previous `loom` and `loomd` back into the install directory, with its own code.
2. adds the new version to `failed_versions` and records `rolled_back` with the reason.
3. under `loom service`, asks the service manager to run `loomd` again. On macOS, it runs `launchctl kickstart -k`, which also skips the 10 s restart throttle of launchd. On Linux, it runs `systemctl --user reset-failed`, then `restart`. If the new version exits at once on each restart, it can hit the start limit of systemd (5 starts in 10 s). This limit leaves the unit failed until `reset-failed`. Without a service, the watchdog stops the unhealthy `loomd` if it still holds the lock.
4. waits, with the same rules, for the previous `loomd` to answer again. The service manager restarts it, or the watchdog starts it.

The failed binaries are not kept in `versions/`, so `loom rollback` never restores them. The saved copy of the restored version is also deleted. This version runs again, and its copy would take one of the two kept slots.

## Traces

With `[telemetry]` enabled, each check of loomd is one trace, with the root `update.check`. An upgrade adds `update.download` (with its `update.verify` steps), `update.preflight` and `update.swap`. The watchdog exports its own trace, `loom __upgrade-watchdog` with `update.watchdog` and its outcome. Span list: [configuration](../reference/configuration.md#telemetry).

## Failed versions

A version in `failed_versions` is never installed automatically again. When `loomd` or `loom upgrade` follows a channel, it does not install this version. The next release above it is installed as usual. `loom upgrade --version <version>` installs it anyway. With this command, you can retry the version on purpose, for example after you fix the cause of the failure on this machine. If you delete `~/.loom/state/update.json`, loom forgets every failed version.

## What it does not catch

- A release that serves `/v1/status` with the right version but is broken elsewhere. The health check only makes sure that the release starts and answers.
- A failure that only shows after the deadline.

## When it is stuck

| Symptom | Cause | What to do |
| --- | --- | --- |
| `loom upgrade` says `loomd is upgrading from … to …` | An upgrade is in progress. | Wait a minute. Then try again. A `pending` older than one hour is ignored and cleared at the next check. |
| `loom status` cannot reach loomd after an upgrade | The new and the restored `loomd` both failed to start, or the watchdog was killed (machine restarted during the deadline). | Read `~/.loom/log/loomd.log` (and `journalctl --user -u loomd.service` on Linux), then `loom rollback` and `loom status`. |
| `last_result` is `failed … not writable` | The user of `loomd` cannot write to its directory. | Fix the permissions, or do the upgrade by hand with `loom upgrade` as a user who can write there. |
| The same version is skipped each time | It is in `failed_versions`. | Retry it with `loom upgrade --version <version>`, or wait for the next release. |
| An installed systemd unit lacks `KillMode=process` | Installed before the auto-upgrade shipped. Then systemd can stop the watchdog together with `loomd`. | Run `loom service install` again. |
| An installed systemd unit lacks `RestartSec=1s` | Installed before the rollback recovery shipped. Then a `loomd` that fails is restarted every 100 ms. | Run `loom service install` again. |
| `loomd.service` is `failed` after a rollback, or `loom service status` says `running: no` | The watchdog could not run `systemctl --user` or `launchctl` (see `loomd.log`). | `systemctl --user reset-failed loomd.service && systemctl --user restart loomd.service`, or `launchctl kickstart -k gui/$(id -u)/io.github.gsuquet.loomd`. |
