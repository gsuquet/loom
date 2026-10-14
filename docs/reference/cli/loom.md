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

### `loom service restart`

Restart loomd and wait for the new one to answer, so a change to `config.toml` takes effect.

The configuration is checked first: when it is invalid, loomd is left running. A loaded user service is restarted by its manager (`launchctl kickstart -k`, `systemctl --user restart`); otherwise loomd gets `SIGTERM`, then a new one is started. Starts loomd when it is not running.

```text
loom service restart [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--json` |  |  | Print the report as JSON (`DaemonRestartReport` schema) |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

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

Check roots, contexts, the disk usage of the loom home, unassigned repositories, loomd's SSH agent, each remote host as loomd's git reaches it, and the last updates; print the fixes. Exits with 1 when a check fails

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

### `loom search`

Search the tasks (title, notes, link keys) and the indexed repositories (name, slug, path, remote URL, context, branch).

Every word of the query must start a word of the result (case and diacritics ignored). Results are ranked: active, ready and waiting tasks, then repositories, then inbox tasks, then done and cancelled tasks. Among tasks: the query as a link key (`CORE-1042`), then every word in the title, then most recently changed. Among repositories: exact name, then name prefix, then slug, then the other fields; then most recently used. `-c` keeps one context, `--type` one kind. Starts loomd when it is not running.

```text
loom search [OPTIONS] <QUERY>...
```

#### Arguments

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `<QUERY>` |  |  | Words to search for, joined with spaces (quoting the query is optional) |

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--limit <N>` |  | `20` | Most results to print |
| `--type <TYPE>` | `task`, `repo` |  | Only results of this kind |
| `--json` |  |  | Print the results as JSON (`SearchResults` schema) |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

### `loom add`

Capture a task: in the inbox, unless `--ready`.

The context is the first of: `-c`, `LOOM_CONTEXT`, the context of the repository of the current directory, its `match.paths` prefix, `default_context`. The kind is `chore` unless `--kind` is given; the importance, the default of the kind. Starts loomd when it is not running.

```text
loom add [OPTIONS] <TITLE>...
```

#### Arguments

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `<TITLE>` |  |  | Title, words joined with spaces (quoting it is optional) |

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--kind <KIND>` | `story`, `bug`, `chore`, `research`, `investigation`, `review`, `support`, `content`, `admin`, `interview`, `learning` | `chore` | Kind of the task |
| `--format <FORMAT>` | `talk`, `article`, `training`, `workshop` |  | Format of a `content` task |
| `--importance <IMPORTANCE>` | `p0`, `p1`, `p2`, `p3` |  | Importance; the default of the kind when absent (p1 for bug, investigation, support and review; p3 for chore, research and learning; p2 otherwise) |
| `--due <DATE>` |  |  | Due date: `YYYY-MM-DD`, `today`, `tomorrow`, `+<n>d` or `+<n>w` |
| `--parent <ID>` |  |  | Parent task: an ID or a unique prefix of one. The task goes to the parent's context; `-c` or `LOOM_CONTEXT` naming another context is an error |
| `--ready` |  |  | Skip the inbox: the task is ready |
| `--json` |  |  | Print the created task as JSON (`TaskDetail` schema) |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

### `loom ls`

List the priority list of a context, or its tasks with a status.

The context is resolved as for `loom add`; `--all-contexts` lists every context. The priority list holds the ready and active tasks, and the waiting ones whose date has come, in priority order. Starts loomd when it is not running.

```text
loom ls [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--all-contexts` |  |  | List every context. `LOOM_CONTEXT` is ignored; `-c` is an error |
| `--status <STATUS>` | `inbox`, `ready`, `active`, `waiting`, `done`, `cancelled` |  | The tasks with this status, sorted by ID, instead of the priority list |
| `--why` |  |  | Add a `WHY` column: the rule that placed each task after the previous one |
| `--json` |  |  | Print the list as JSON (`TaskList` schema) |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

### `loom show`

Show a task: its fields and notes. `ID` is a task ID or a unique prefix of one, of any context

```text
loom show [OPTIONS] <ID>
```

#### Arguments

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `<ID>` |  |  | Task ID, or a unique prefix of one |

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--json` |  |  | Print the task as JSON (`TaskDetail` schema) |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom reindex`

Rebuild the tasks index from the task files of every context store.

The files are the source of truth: rows of tasks whose directory is gone are dropped, an invalid `task.md` is indexed read-only and listed, never rewritten. A task that cannot be read is listed and keeps its previous entry; the others are indexed. Hand edits not recorded yet are recorded first, so a reindex may append `edited` events. The repositories of the index are left as they are. Starts loomd when it is not running.

```text
loom reindex [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--json` |  |  | Print the result as JSON (`ReindexResult` schema) |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

### `loom sync`

Back up the context stores: push their commits to `store.backup.remote`, or fast-forward them to it.

Syncs every context store held on this machine that has a `store.backup.remote`, or the one `-c` names. Changes not committed yet are committed first. A store whose remote has commits it lacks while it has commits the remote lacks has diverged: it is reported and left as it is, never merged, rebased or forced. Fails when a store is not synced. loomd also syncs at start and every `[tasks] sync_interval`. Starts loomd when it is not running.

```text
loom sync [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--json` |  |  | Print the result as JSON (`BackupReport` schema) |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

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
