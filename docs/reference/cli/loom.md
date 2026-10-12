# `loom` CLI reference

> Generated from the `clap` definitions by `mise run docs`. Do not edit by hand.
> Conventions shared by every command: [conventions.md](conventions.md).

Personal work engine: CLI and TUI

## Usage

```text
loom [OPTIONS] <COMMAND>
```

### Global options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `-c <NAME>`, `--context <NAME>` |  |  | Context to work in (lowercase letters, digits and `-`, starting with a letter) |
| `-q`, `--quiet` |  |  | Print errors only on stderr |
| `-v`, `--verbose` | repeatable |  | Print more diagnostics on stderr; repeat for more (`-vv`) |
| `--color <WHEN>` | `auto`, `always`, `never` | `auto` | When to use colors: `auto` colors a terminal unless `NO_COLOR` is set |
| `-y`, `--yes` |  |  | Answer yes to every confirmation (the only way to skip one) |
| `-h`, `--help` |  |  | Print help |
| `-V`, `--version` |  |  | Print version |

## Commands

### `loom version`

Print version, commit, target, channel and profile of this build

```text
loom version [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--json` |  |  | Print the build information as JSON (`VersionInfo` schema) |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom status`

Show the state of loomd, starting it when it is not running

```text
loom status [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--json` |  |  | Print the daemon state as JSON (`StatusResponse` schema) |
| `--no-start` |  |  | Fail instead of starting loomd when it is not running |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom service`

Install, remove or inspect loomd as a user service (launchd on macOS, systemd on Linux)

```text
loom service [OPTIONS] <COMMAND>
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom service install`

Install loomd as a user service and start it; no-op when already installed and loaded. Refused when `[daemon] service = false`

```text
loom service install [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `-n`, `--dry-run` |  |  | Print the file that would be written and the commands that would run; change nothing |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom service uninstall`

Stop loomd, unload the user service and remove its file; no-op when not installed

```text
loom service uninstall [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `-n`, `--dry-run` |  |  | Print the commands that would run and the file that would be removed; change nothing |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom service status`

Report whether the user service is installed, loaded and running

```text
loom service status [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--json` |  |  | Print the state as JSON (`ServiceStatus` schema) |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom service help`

Print this message or the help of the given subcommand(s)

```text
loom service help [COMMAND]
```

### `loom upgrade`

Upgrade `loom` and `loomd` to the latest release of a channel, to a given release, or to a local build.

Both binaries are replaced in the directory of the running `loom`; the replaced version is kept in `<loom home>/versions/`. Releases are verified (minisign signature of `SHA256SUMS`, then the archive checksum) before anything is replaced.

```text
loom upgrade [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--channel <CHANNEL>` | `stable`, `dev` |  | Release channel to upgrade on. Default: the channel of this build (required for a local build, unless `--version` or `--from` is given) |
| `--version <VERSION>` |  |  | Release to install: `X.Y.Z` or `X.Y.Z-dev.N`. Never lower than the running version (use `loom rollback`) |
| `--from <PATH>` |  |  | Install a local build: a release archive (`.tar.gz`) or a directory holding `loom` and `loomd`. Not verified; installed even when its version is not newer |
| `--check` |  |  | Print the decision only: no download, nothing changed |
| `-n`, `--dry-run` |  |  | Download and verify, print what would be replaced, change nothing |
| `--json` |  |  | Print the result as JSON (`UpgradeReport` schema) |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

### `loom rollback`

Restore the previous version: the highest version kept in `<loom home>/versions/` other than the running one

```text
loom rollback [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `-n`, `--dry-run` |  |  | Print what would be restored, change nothing |
| `--json` |  |  | Print the result as JSON (`RollbackReport` schema) |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom help`

Print this message or the help of the given subcommand(s)

```text
loom help [COMMAND]
```
