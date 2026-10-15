# Upgrade and roll back loom

> Upgrade `loom` and `loomd` to a newer release, change the channel, test a local build, or return to the previous version. For the reference, see [`loom upgrade` and `loom rollback`](../reference/cli/conventions.md#loom-upgrade).

`loom upgrade` replaces `loom` and `loomd` in the directory of the `loom` that you run (for example `~/.local/bin`). Run it as a user who can write to that directory.

## Check for a newer version

```bash
loom upgrade --check
```

The command changes nothing. It prints the decision:

```text
loom 0.2.0 is available (stable), running 0.1.0
```

## Upgrade

```bash
loom upgrade
```

`loom upgrade` uses the channel in `[update] channel`. If this key is not set, it uses the channel last given with `--channel` (or to `install.sh`). If there is no such channel, it uses the channel of the installed build. Then the command does these steps:

1. It downloads the release.
2. It verifies the signature and the checksum of the release.
3. It keeps the replaced version in `~/.loom/versions/`.
4. It replaces the two binaries.
5. If loomd runs, it stops loomd, so that loomd restarts on the new version.

To see what the command replaces, use `--dry-run`. The command downloads and verifies the release, but installs nothing:

```bash
loom upgrade --dry-run
```

To install a specific release, give its version:

```bash
loom upgrade --version 0.2.0
```

`--version` never installs a version lower than the current version. For a lower version, use `loom rollback`.

If GitHub returns `403` or `429` (API rate limit), set a token. Then run the command again:

```bash
export LOOM_GITHUB_TOKEN=<token>
loom upgrade
```

## Switch channel

Give the channel one time:

```bash
loom upgrade --channel dev
```

loom records the channel. From then on, `loomd` and `loom upgrade` use it, also after an upgrade to a stable release.

`loom upgrade --channel dev --check` records the channel but installs nothing. If `[update] channel` is set in `config.toml`, it has priority. `loom status` shows the source of the channel (`config`, `chosen` or `build`).

To return to stable, upgrade with `--channel stable`. The dev builds of a version sort before the release of that version. Thus this command installs the next stable release when it is published. To return to an older stable release immediately, use `loom rollback` or `--from`.

## Test a local build

Build the two binaries. Then install them from the build directory:

```bash
cargo build --release -p loom -p loomd
loom upgrade --from target/release
```

`--from` also accepts a release archive (`loom-<version>-<target>.tar.gz`). The command installs the local build whatever its version. It does not verify a signature or a checksum, and it prints a warning about this. A local build has the channel `local`. Later, to upgrade it to a release, give `--channel` one time, for example `loom upgrade --channel stable`. You do not need this step if a channel is already configured or recorded.

## Roll back

```bash
loom rollback
```

The command restores the highest version in `~/.loom/versions/` that is not the current version. It keeps the version that it replaces. Thus a second `loom rollback` returns to that version. To see which version the command restores, add `--dry-run`.

If no other version is kept, `loom rollback` fails with `nothing to roll back to`. This is the case before the first upgrade on a machine (`loom upgrade` or the auto-upgrade of `loomd`): `install.sh` keeps no version.

## Let loomd upgrade itself

`loomd` checks for releases every 6 hours. A release build automatically installs the new releases of its channel. This channel is the one given to `loom upgrade --channel` or `install.sh --channel`, otherwise the channel of the build. A local build only reports the new releases. To select the behavior, set `[update]` in `~/.loom/config.toml` ([keys](../reference/configuration.md#keys)):

```toml
[update]
policy = "notify"   # "auto": install; "notify": report only; "off": never check
channel = "stable"
```

`loomd` reads the file when it starts. Restart it to apply the change:

- Linux service: `systemctl --user restart loomd.service`
- macOS service: `launchctl kickstart -k gui/$(id -u)/io.github.gsuquet.loomd`
- without the service: stop loomd with `kill <pid>` (`loom status` shows the PID). Then run `loom status`, which starts loomd again.

Then check the result:

```bash
loom status
```

The `updates`, `available` and `last_result` lines show what loomd found and did. For how this works, and how loomd rolls back a release that fails, see the [explanation](../explanation/auto-upgrade.md).

## Retry a version that failed

If a version fails the auto-upgrade health check, `loom status` shows it on the `failed` line. loomd never installs this version automatically again. To try it one more time, install it with `--version`:

```bash
loom upgrade --version 0.2.0
```
