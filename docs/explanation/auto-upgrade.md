# How loomd upgrades itself

> The check cycle, the pre-flight check, the watchdog, the rollback and the failed versions of the auto-upgrade of `loomd`, and why they work this way. Keys and files: [configuration](../reference/configuration.md#keys); decision: [ADR-0011](../adr/0011-auto-upgrade-rollback.md).

## The problem

An upgrade replaces the binary of a daemon that runs all the time. If the new release cannot start, nobody is there to notice: the daemon is gone, and with it the code that would have put the old version back. So the code that judges the new version, and rolls it back, must never be the new version itself.

## The check cycle

`loomd` runs one updater thread. It sleeps until the next check, then asks the GitHub releases API for the latest release of its channel, and decides with the same rules as `loom upgrade` (semver precedence, never downgrade, failed versions skipped).

- The first check runs about 5 minutes after `loomd` starts, never at once: a daemon restarted in a loop does not hammer the API.
- The next ones run every `check_interval` (6 h by default) after the previous one. Each delay moves by up to ±10 % at random, so that machines started together do not check together.
- `policy = "off"`, or a local build without `channel`, never checks: the thread does not even build an HTTP client.
- Under `notify`, a newer version is recorded as `available` in `state/update.json`, and `loom status` shows it. Nothing is downloaded.
- Under `auto`, the upgrade starts.

Every outcome lands in `last_result` of `state/update.json`, which `loom status` prints.

## The upgrade

1. `loomd` records the upgrade as `pending` (from, to, start time). While it is there, `loom upgrade` and `loom rollback` refuse to run, and another `loomd` skips its checks.
2. It checks that the directory of its own binary can be written, then downloads the release and verifies it exactly as `loom upgrade` does: minisign signature of `SHA256SUMS`, checksum of the archive, both binaries reporting the release version.
3. **Pre-flight check.** It starts the downloaded `loomd` with a throwaway loom home under `/tmp`, waits up to 15 s for it to answer `GET /v1/status` with the release version, then kills it. A release that cannot start at all (wrong architecture, missing library, crash at start) is caught here, before anything installed is touched. It goes into `failed_versions`, and the running `loomd` carries on.
4. It saves the installed `loom` and `loomd` in `~/.loom/versions/<running version>/` and replaces them, like `loom upgrade`.
5. It starts the **watchdog** from the saved copy: `~/.loom/versions/<running version>/loom __upgrade-watchdog …`, detached, its output appended to `~/.loom/log/loomd.log`.
6. It stops itself (exit code 0).

## The watchdog

The watchdog is the previous version's `loom`, which was running a moment ago and is known to work. It waits up to 60 s for a `loomd` to answer on the socket with the new version.

Someone has to start the new `loomd`:

- under `loom service` (launchd `KeepAlive`, systemd `Restart=always`), the service manager restarts it at once;
- otherwise, when nobody holds `run/loomd.lock` for 15 s, the watchdog starts it itself, the way `loom status` does.

When the new `loomd` answers in time, the watchdog records `upgraded` and exits.

## The rollback

When the deadline passes without an answer from the new version (it crashed, hangs, or reports another version), the watchdog:

1. copies the saved previous `loom` and `loomd` back into the install directory, with its own code;
2. adds the new version to `failed_versions` and records `rolled_back` with the reason;
3. stops the unhealthy `loomd` if it still holds the lock;
4. waits, with the same rules, for the previous `loomd` to answer again: the service manager restarts it, or the watchdog starts it.

The failed binaries are not kept in `versions/`, so `loom rollback` never brings them back. The saved copy of the restored version is deleted too: it is the running version again, and would take one of the two kept slots.

## Traces

With `[telemetry]` enabled, each check of loomd is one trace, rooted at `update.check`; an upgrade adds `update.download` (with its `update.verify` steps), `update.preflight` and `update.swap`. The watchdog exports its own trace, `loom __upgrade-watchdog` with `update.watchdog` and its outcome. Span list: [configuration](../reference/configuration.md#telemetry).

## Failed versions

A version in `failed_versions` is never installed automatically again, neither by `loomd` nor by `loom upgrade` following a channel. The next release above it is installed as usual. `loom upgrade --version <version>` installs it anyway: that is the way to retry it on purpose, for example after fixing what made it fail on this machine. Deleting `~/.loom/state/update.json` forgets every failed version.

## What it does not catch

- A release that serves `/v1/status` with the right version but is broken elsewhere: the health check only covers starting and answering.
- A failure that only shows after the deadline.

## When it is stuck

| Symptom | Cause | What to do |
| --- | --- | --- |
| `loom upgrade` says `loomd is upgrading from … to …` | An upgrade is in progress. | Wait a minute and retry. A `pending` older than one hour is ignored and cleared at the next check. |
| `loom status` cannot reach loomd after an upgrade | The new and the restored `loomd` both failed to start, or the watchdog was killed (machine restarted during the deadline). | Read `~/.loom/log/loomd.log` (and `journalctl --user -u loomd.service` on Linux), then `loom rollback` and `loom status`. |
| `last_result` is `failed … not writable` | The directory of `loomd` cannot be written by its user. | Fix the permissions, or upgrade by hand with `loom upgrade` as a user who can write there. |
| The same version keeps being skipped | It is in `failed_versions`. | Retry it with `loom upgrade --version <version>`, or wait for the next release. |
| An installed systemd unit lacks `KillMode=process` | Installed before the auto-upgrade shipped; systemd may stop the watchdog with `loomd`. | Run `loom service install` again. |
