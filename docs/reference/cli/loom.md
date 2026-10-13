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
| `--channel <CHANNEL>` | `stable`, `dev` |  | Release channel to upgrade on, remembered in `state/update.json` for loomd and the next `loom upgrade` (not with `--dry-run`). Default: `[update] channel`, otherwise the channel last given here, otherwise the channel of this build (required for a local build, unless `--version` or `--from` is given) |
| `--version <VERSION>` |  |  | Release to install: `X.Y.Z` or `X.Y.Z-dev.N`. Never lower than the running version (use `loom rollback`) |
| `--from <PATH>` |  |  | Install a local build: a release archive (`.tar.gz`) or a directory holding `loom` and `loomd`. Not verified; installed even when its version is not newer |
| `--check` |  |  | Print the decision only: no download, nothing installed (a `--channel` is still remembered) |
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

### `loom repo`

List, show, scan, register and check the repositories loomd indexes.

Starts loomd when it is not running (except `doctor`). Run inside a repository the index does not know, any `loom` command registers it.

```text
loom repo [OPTIONS] <COMMAND>
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

### `loom repo ls`

List the indexed repositories, sorted by context then name (`-c` keeps one context)

```text
loom repo ls [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--json` |  |  | Print the list as JSON (`RepoList` schema) |
| `--format <FORMAT>` | `table`, `paths` | `table` | Output format |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

### `loom repo show`

Show a repository: identity, context and the rule that chose it, state

```text
loom repo show [OPTIONS] [REPO]
```

#### Arguments

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `<REPO>` |  |  | Repository: path, `<context>/<name>` or name. Default: the repository of the current directory. A path the index does not know is registered |

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--refresh` |  |  | Read the repository's state now instead of showing the stored one |
| `--json` |  |  | Print the repository as JSON (`RepoSummary` schema) |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom repo scan`

Scan the roots and the registered repositories now

```text
loom repo scan [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--rebuild` |  |  | Drop the whole index and rebuild it from the roots: registered repositories outside the roots, `last_used`, states and the results of the last updates are lost. Asks for confirmation |
| `-n`, `--dry-run` |  |  | Print what would change, change nothing |
| `--json` |  |  | Print the result as JSON (`ScanResult` schema) |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom repo register`

Add a repository outside the roots to the index; scans keep it while it exists

```text
loom repo register [OPTIONS] [PATH]
```

#### Arguments

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `<PATH>` |  |  | A directory inside the repository. Default: the current directory |

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `-n`, `--dry-run` |  |  | Print the repository that would be registered, change nothing |
| `--json` |  |  | Print the registered repository as JSON (`RepoSummary` schema) |
| `--on-use` |  |  | Register as a use, like any `loom` command run inside the repository: sets its `last_used`, leaves an indexed repository as it is, and never starts loomd |
| `--background` |  |  | With `--on-use`: wait for loomd at most 50 ms, print nothing, and exit with 0 whatever happens (run by the shell integration) |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom repo forget`

Remove a repository from the index (registering it again undoes it)

```text
loom repo forget [OPTIONS] <REPO>
```

#### Arguments

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `<REPO>` |  |  | Repository: path, `<context>/<name>` or name |

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `-n`, `--dry-run` |  |  | Print the repository that would be removed, change nothing |
| `--json` |  |  | Print the removed repository as JSON (`RepoSummary` schema) |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom repo doctor`

Check roots, contexts, unassigned repositories, loomd's SSH agent and SSH access to each host; print the fixes. Exits with 1 when a check fails

```text
loom repo doctor [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--json` |  |  | Print the report as JSON (`DoctorReport` schema) |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom repo sync`

Update repositories now, as loomd does on its schedule: fetch the primary remote, then fast-forward only when no local work can be affected.

Ignores the schedule and the backoff of unreachable hosts; follows the update policy of each repository. Prints one line per repository: its slug and what happened.

```text
loom repo sync [OPTIONS] [REPO]...
```

#### Arguments

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `<REPO>` |  |  | Repositories to update: path, `<context>/<name>` or name; every repository of the index when none is given |

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `-n`, `--dry-run` |  |  | Print what each update would do, decided from the state read now; fetch nothing, change nothing |
| `--json` |  |  | Print the result as JSON (`SyncResult` schema) |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

### `loom repo help`

Print this message or the help of the given subcommand(s)

```text
loom repo help [COMMAND]
```

### `loom shell-init`

Print the shell integration to evaluate in the shell's startup file: `eval "$(loom shell-init zsh)"` in `~/.zshrc`, `eval "$(loom shell-init bash)"` in `~/.bashrc`.

The directory the shell starts in, then each directory it enters, once per shell session, runs `loom repo register --on-use --background` detached, so that loomd registers the repository it is in. Does nothing when `loom` is not on `PATH`.

```text
loom shell-init [OPTIONS] <SHELL>
```

#### Arguments

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `<SHELL>` | `zsh`, `bash` |  | Shell to integrate with |

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

### `loom help`

Print this message or the help of the given subcommand(s)

```text
loom help [COMMAND]
```
