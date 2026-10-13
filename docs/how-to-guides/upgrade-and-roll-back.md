# Upgrade and roll back loom

> Upgrade `loom` and `loomd` to a newer release, switch channel, test a local build, and go back to the previous version. Reference: [`loom upgrade` and `loom rollback`](../reference/cli/conventions.md#loom-upgrade).

`loom upgrade` replaces `loom` and `loomd` in the directory of the `loom` you run (for example `~/.local/bin`). Run it as a user who can write to that directory.

## Check for a newer version

```bash
loom upgrade --check
```

It prints the decision and changes nothing:

```text
loom 0.2.0 is available (stable), running 0.1.0
```

## Upgrade

```bash
loom upgrade
```

`loom upgrade` follows `[update] channel`, otherwise the channel last passed with `--channel` (or to `install.sh`), otherwise the channel of the installed build. It downloads the release, verifies its signature and checksum, keeps the replaced version in `~/.loom/versions/`, replaces both binaries, then stops a running loomd so that it restarts on the new version.

To see what would be replaced, with the download and verification but without installing anything:

```bash
loom upgrade --dry-run
```

To install a given release:

```bash
loom upgrade --version 0.2.0
```

`--version` never goes below the running version: use `loom rollback` for that.

If GitHub answers `403` or `429` (API rate limit), set a token and run the command again:

```bash
export LOOM_GITHUB_TOKEN=<token>
loom upgrade
```

## Switch channel

Pass the channel once: it is remembered, and `loomd` and `loom upgrade` follow it from then on, even after an upgrade to a stable release.

```bash
loom upgrade --channel dev
```

`loom upgrade --channel dev --check` records it without installing anything. `[update] channel` in `config.toml`, when set, takes precedence; `loom status` shows where the channel comes from (`config`, `chosen` or `build`).

To go back to stable, upgrade with `--channel stable`. Dev builds of a version sort before its release, so this installs the next stable release once it is published; to return to an older stable release right away, use `loom rollback` or `--from`.

## Test a local build

Build both binaries, then install them from the build directory:

```bash
cargo build --release -p loom -p loomd
loom upgrade --from target/release
```

`--from` also takes a release archive (`loom-<version>-<target>.tar.gz`). The local build is installed whatever its version, without signature or checksum verification, and a warning says so. A local build has channel `local`: to upgrade it to a release later, pass `--channel` once, for example `loom upgrade --channel stable`, unless a channel is already configured or remembered.

## Roll back

```bash
loom rollback
```

It restores the highest version kept in `~/.loom/versions/` other than the running one, and keeps the version it replaces, so a second `loom rollback` returns to it. Add `--dry-run` to see which version would be restored.

`loom rollback` fails with `nothing to roll back to` when no other version is kept: the first upgrade on a machine has nothing to go back to.

## Let loomd upgrade itself

`loomd` checks for releases every 6 hours. A release build installs new releases of its channel (the one passed to `loom upgrade --channel` or `install.sh --channel`, otherwise its own) by itself; a local build only reports them. To choose, set `[update]` in `~/.loom/config.toml` ([keys](../reference/configuration.md#keys)):

```toml
[update]
policy = "notify"   # "auto": install; "notify": report only; "off": never check
channel = "stable"
```

`loomd` reads the file when it starts. Restart it to apply the change:

- Linux service: `systemctl --user restart loomd.service`
- macOS service: `launchctl kickstart -k gui/$(id -u)/io.github.gsuquet.loomd`
- without the service: stop it (`kill <pid>`, the PID is in `loom status`), then run `loom status`, which starts it again.

Then:

```bash
loom status
```

The `updates`, `available` and `last_result` lines show what loomd found and did. How it works, and how it rolls back a release that fails: [explanation](../explanation/auto-upgrade.md).

## Retry a version that failed

A version that failed the auto-upgrade health check is listed on the `failed` line of `loom status` and never installed automatically again. To try it once more:

```bash
loom upgrade --version 0.2.0
```
