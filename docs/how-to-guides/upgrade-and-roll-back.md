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

`loom upgrade` follows the channel of the installed build. It downloads the release, verifies its signature and checksum, keeps the replaced version in `~/.loom/versions/`, replaces both binaries, then stops a running loomd so that it restarts on the new version.

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

Pass the channel once; the installed dev build then follows `dev` by default.

```bash
loom upgrade --channel dev
```

To go back to stable, upgrade with `--channel stable`. Dev builds of a version sort before its release, so this installs the next stable release once it is published; to return to an older stable release right away, use `loom rollback` or `--from`.

## Test a local build

Build both binaries, then install them from the build directory:

```bash
cargo build --release -p loom -p loomd
loom upgrade --from target/release
```

`--from` also takes a release archive (`loom-<version>-<target>.tar.gz`). The local build is installed whatever its version, without signature or checksum verification, and a warning says so. A local build has channel `local`: to upgrade it to a release later, pass `--channel`, for example `loom upgrade --channel stable`.

## Roll back

```bash
loom rollback
```

It restores the highest version kept in `~/.loom/versions/` other than the running one, and keeps the version it replaces, so a second `loom rollback` returns to it. Add `--dry-run` to see which version would be restored.

`loom rollback` fails with `nothing to roll back to` when no other version is kept: the first upgrade on a machine has nothing to go back to.
