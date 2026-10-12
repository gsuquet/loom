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

### `loom help`

Print this message or the help of the given subcommand(s)

```text
loom help [COMMAND]
```
