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

### `loom help`

Print this message or the help of the given subcommand(s)

```text
loom help [COMMAND]
```
