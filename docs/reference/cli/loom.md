# `loom` CLI reference

> Generated from the `clap` definitions by `mise run docs`. Do not edit by hand.
> Conventions shared by every command: [conventions.md](conventions.md).

Personal work engine: CLI and TUI.

With no command, opens the TUI when standard input and standard output are terminals, and prints the help otherwise.

## Usage

```text
loom [OPTIONS] [COMMAND]
```

### Global options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `-c <NAME>`, `--context <NAME>` |  |  | Context to work in (lowercase letters, digits and `-`, with a letter first) |
| `-q`, `--quiet` |  |  | Print errors only on stderr |
| `-v`, `--verbose` | repeatable |  | Print more diagnostics on stderr. Repeat the flag for more (`-vv`) |
| `--color <WHEN>` | `auto`, `always`, `never` | `auto` | When to use colors: `auto` colors a terminal unless `NO_COLOR` is set |
| `-y`, `--yes` |  |  | Answer yes to every confirmation (the only way to skip one) |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |
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

Show the state of loomd, and start loomd if it is stopped

```text
loom status [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--json` |  |  | Print the daemon state as JSON (`StatusResponse` schema) |
| `--no-start` |  |  | Fail if loomd is stopped, and do not start it |
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

Install loomd as a user service and start it. No-op if it is already installed and loaded. Refused if `[daemon] service = false`

```text
loom service install [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `-n`, `--dry-run` |  |  | Print the file that would be written and the commands that would run. Change nothing |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom service uninstall`

Stop loomd, unload the user service and remove its file. No-op if it is not installed

```text
loom service uninstall [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `-n`, `--dry-run` |  |  | Print the commands that would run and the file that would be removed. Change nothing |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom service status`

Report whether the user service is installed and loaded, and whether it runs

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

The command checks the configuration first. If the configuration is invalid, loomd continues to run. If the user service is loaded, its manager restarts it (`launchctl kickstart -k`, `systemctl --user restart`). If not, loomd gets `SIGTERM`, then a new loomd starts. Starts loomd if it is stopped.

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

The command replaces the two binaries in the directory of the `loom` that runs. It keeps the replaced version in `<loom home>/versions/`. It verifies the releases (minisign signature of `SHA256SUMS`, then the archive checksum) before it replaces anything.

```text
loom upgrade [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--channel <CHANNEL>` | `stable`, `dev` |  | Release channel to upgrade on, remembered in `state/update.json` for loomd and the next `loom upgrade` (not with `--dry-run`). Default: `[update] channel`, otherwise the channel last given here, otherwise the channel of this build (required for a local build, unless `--version` or `--from` is given) |
| `--version <VERSION>` |  |  | Release to install: `X.Y.Z` or `X.Y.Z-dev.N`. Never lower than the version that runs (use `loom rollback`) |
| `--from <PATH>` |  |  | Install a local build: a release archive (`.tar.gz`) or a directory that holds `loom` and `loomd`. Not verified. Installed even if its version is not newer |
| `--check` |  |  | Print the decision only: no download, nothing installed (a `--channel` is still remembered) |
| `-n`, `--dry-run` |  |  | Download and verify, print what would be replaced, change nothing |
| `--json` |  |  | Print the result as JSON (`UpgradeReport` schema) |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

### `loom rollback`

Restore the previous version: the highest version kept in `<loom home>/versions/` other than the version that runs

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

Starts loomd if it is stopped (except `doctor`). If any `loom` command runs inside a repository that the index does not know, the command registers that repository.

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
| `--refresh` |  |  | Read the state of the repository now. Do not show the stored state |
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

Add a repository outside the roots to the index. Scans keep it while it exists

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

Remove a repository from the index (to undo, register it again)

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

Check roots, contexts, the disk usage of the loom home and unassigned repositories. Check loomd's SSH agent, each remote host as loomd's git reaches it, and the last updates. Print the fixes. If a check fails, the command exits with 1

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

Ignores the schedule and the backoff of unreachable hosts. Follows the update policy of each repository. Prints one line per repository: its slug and what happened.

```text
loom repo sync [OPTIONS] [REPO]...
```

#### Arguments

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `<REPO>` |  |  | Repositories to update: path, `<context>/<name>` or name. Default: every repository of the index |

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `-n`, `--dry-run` |  |  | Print what each update would do, decided from the state read now. Fetch nothing, change nothing |
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

Every word of the query must start a word of the result (case and diacritics ignored). Results are ranked: active, ready and waiting tasks, then repositories, then inbox tasks, then done and cancelled tasks. Among tasks: the query as a link key (`CORE-1042`), then every word in the title, then most recently changed. Among repositories: exact name, then name prefix, then slug, then the other fields. In each group, the most recently used comes first. `-c` keeps one context, `--type` one kind. Starts loomd if it is stopped.

```text
loom search [OPTIONS] <QUERY>...
```

#### Arguments

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `<QUERY>` |  |  | Words to search for, joined with spaces (quotes around the query are optional) |

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

The context is the first of: `-c`, `LOOM_CONTEXT`, the context of the repository of the current directory, its `match.paths` prefix, `default_context`. The kind is `chore` unless `--kind` is given. The importance is the default of the kind. Starts loomd if it is stopped.

```text
loom add [OPTIONS] <TITLE>...
```

#### Arguments

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `<TITLE>` |  |  | Title, words joined with spaces (quotes around the title are optional) |

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--kind <KIND>` | `story`, `bug`, `chore`, `research`, `investigation`, `review`, `support`, `content`, `admin`, `interview`, `learning` | `chore` | Kind of the task |
| `--format <FORMAT>` | `talk`, `article`, `training`, `workshop` |  | Format of a `content` task |
| `--importance <IMPORTANCE>` | `p0`, `p1`, `p2`, `p3` |  | Importance. If absent, the default of the kind (p1 for bug, investigation, support and review; p3 for chore, research and learning; p2 otherwise) |
| `--due <DATE>` |  |  | Due date: `YYYY-MM-DD`, `today`, `tomorrow`, `+<n>d` or `+<n>w` |
| `--parent <ID>` |  |  | Parent task: an ID or a unique prefix of one. The task goes to the parent's context. If `-c` or `LOOM_CONTEXT` names another context, the command fails |
| `--repo <REPO>` |  |  | Repository of the task: `<context>/<name>`, a name, a path, or `none`. If absent, and the current directory is inside a repository of the task's context, that repository |
| `--ready` |  |  | Skip the inbox: the task is ready |
| `--json` |  |  | Print the created task as JSON (`TaskDetail` schema) |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

### `loom ls`

List the priority list of a context, or its tasks with a status.

The context is resolved as for `loom add`. `--all-contexts` lists every context. The priority list holds the ready and active tasks, and the waiting ones whose date has come, in priority order. Starts loomd if it is stopped.

```text
loom ls [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--all-contexts` |  |  | List every context. `LOOM_CONTEXT` is ignored. `-c` is an error |
| `--status <STATUS>` | `inbox`, `ready`, `active`, `waiting`, `done`, `cancelled` |  | The tasks with this status, sorted by ID, instead of the priority list |
| `--why` |  |  | Add a `WHY` column: the rule that placed each task after the previous one |
| `--archived` |  |  | The archived tasks (in `archive/<yyyy>/`), sorted by ID, instead of the priority list. With `--status`, only those with that status |
| `--json` |  |  | Print the list as JSON (`TaskList` schema) |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

### `loom show`

Show a task: its fields and notes. `ID` is a task ID or a unique prefix of one, of any context. A warning on standard error lists the lines of its event log that loom does not replay

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

### `loom set`

Change fields of a task with `key=value` pairs. The keys are `title`, `kind`, `format` (with `kind=content`), `importance`, `due` (a date, or `none`), `parent` (an ID, or `none`), `repo` (a repository, or `none`), `pinned` (`yes` or `no`), `status` (any status except `waiting`, which `loom snooze` sets) and `context` (moves the task to another context). A task with a parent or children does not move. Loom applies the move last

```text
loom set [OPTIONS] <ID> <KEY=VALUE>...
```

#### Arguments

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `<ID>` |  |  | Task ID, or a unique prefix of one |
| `<KEY=VALUE>` |  |  | Changes, as `key=value` |

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--json` |  |  | Print the task as JSON (`TaskDetail` schema) |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom go`

Make a task the focus of its context, set its status to `active`, and change to its repository.

If another task was the focus of that context, its status returns to `ready`. Prints the task and the repository path. If the shell integration (`loom shell-init`) is loaded, the shell changes to that directory. If the task has no repository, the directory does not change. Starts loomd if it is stopped.

```text
loom go [OPTIONS] <ID>
```

#### Arguments

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `<ID>` |  |  | Task ID, or a unique prefix of one |

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--json` |  |  | Print the result as JSON (`GoResult` schema) |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

### `loom focus`

Show the focus task of each context, or of the context that `-c` names.

The focus is stored in `index.db` on this machine, never in the context store. Starts loomd if it is stopped.

```text
loom focus [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--json` |  |  | Print the list as JSON (`FocusList` schema) |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

### `loom prompt`

Print the context of the current directory and its focus task, for a shell prompt.

The line is `<context>`, or `<context> <prefix> <title>` when the context has a focus task. The context is resolved in the order of `loom add`. If the current directory is in a git repository that loomd does not know, the line is empty. The command never starts loomd and never registers a repository. It waits for loomd for at most 100 ms. If loomd does not answer in that time, the line has no focus task, and in a git repository the line is empty. Except for usage errors, the command never prints on standard error and exits with 0: when it cannot find the context, it prints nothing.

```text
loom prompt [OPTIONS]
```

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--shell <SHELL>` | `zsh`, `bash` |  | Shell of the prompt. With `zsh`, `%` becomes `%%`, so that prompt expansion shows it. With `bash`, the line stays as it is |
| `--max-title <N>` |  | `30` | Maximum number of characters of the task title. A longer title ends with `…`. With 0, the line has no title |
| `--json` |  |  | Print the segment as JSON (`PromptSegment` schema) |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

### `loom log`

Record a note event on the focus task of the context, or on `--task`.

Without `--task`, the context is resolved as for `loom add`. If that context has no focus task, the command fails. Starts loomd if it is stopped.

```text
loom log [OPTIONS] <TEXT>...
```

#### Arguments

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `<TEXT>` |  |  | Note text, words joined with spaces (quotes around the text are optional) |

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--task <ID>` |  |  | Task to record the note on, instead of the focus task of the context |
| `--json` |  |  | Print the task as JSON (`TaskDetail` schema) |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

### `loom accept`

Accept a task from the inbox: set its status to ready, and change its fields.

ID is a task ID or a unique prefix of one. Starts loomd if it is stopped.

```text
loom accept [OPTIONS] <ID>
```

#### Arguments

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `<ID>` |  |  | Task ID, or a unique prefix of one |

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--kind <KIND>` | `story`, `bug`, `chore`, `research`, `investigation`, `review`, `support`, `content`, `admin`, `interview`, `learning` |  | Kind of the task. If changed without --importance, resets importance to the kind's default |
| `--format <FORMAT>` | `talk`, `article`, `training`, `workshop` |  | Format of a `content` task |
| `--importance <IMPORTANCE>` | `p0`, `p1`, `p2`, `p3` |  | Importance of the task. If omitted when kind changes, resets to the kind's default; if omitted when kind is unchanged, keeps existing importance |
| `--due <DATE>` |  |  | Due date: `YYYY-MM-DD`, `today`, `tomorrow`, `+<n>d`, `+<n>w` or `none` |
| `--json` |  |  | Print the task as JSON (`TaskDetail` schema) |
| `-h`, `--help` |  |  | Print help (see a summary with '-h') |

Global options apply.

### `loom done`

Mark a task done

```text
loom done [OPTIONS] <ID>
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

### `loom cancel`

Cancel a task

```text
loom cancel [OPTIONS] <ID>
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

### `loom snooze`

Put a task in `waiting` until a date. Until then, the priority list does not show it

```text
loom snooze [OPTIONS] <ID> <UNTIL>
```

#### Arguments

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `<ID>` |  |  | Task ID, or a unique prefix of one |
| `<UNTIL>` |  |  | Until when: `YYYY-MM-DD`, `today`, `tomorrow`, `+<n>d` or `+<n>w` |

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--on <WHO>` |  |  | Who or what the task waits on |
| `--json` |  |  | Print the task as JSON (`TaskDetail` schema) |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom undo`

Revert the last change of a task (an edit, a status change, a pin, a hand edit, an undo). An undo of an undo applies the change again

```text
loom undo [OPTIONS] <ID>
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

### `loom edit`

Open the `task.md` of a task in `VISUAL`, `EDITOR` or `vi`. loomd records the changes as an edit. If the file is invalid, loomd reports it and keeps it as saved

```text
loom edit [OPTIONS] <ID>
```

#### Arguments

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `<ID>` |  |  | Task ID, or a unique prefix of one |

#### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `-h`, `--help` |  |  | Print help |

Global options apply.

### `loom reindex`

Rebuild the tasks index from the task files of every context store.

The files are the source of truth. The reindex drops the rows of tasks whose directory is gone. It indexes an invalid `task.md` as read-only and lists it, but never rewrites it. If a task cannot be read, the reindex lists it and keeps its previous entry. It indexes the other tasks.

Hand edits that are not recorded yet are recorded first, so a reindex can append `edited` events. The repositories of the index stay as they are. Starts loomd if it is stopped.

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

Syncs every context store on this machine that has a `store.backup.remote`, or only the store that `-c` names. Changes that are not committed yet are committed first. A store has diverged if the store and its remote each have commits that the other does not have. The sync reports a diverged store and leaves it as it is: it never merges, rebases or forces it.

The command fails if a store is not synced. loomd also syncs at start and at each `[tasks] sync_interval`. Starts loomd if it is stopped.

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

The shell runs `loom repo register --on-use --background` detached in the directory where it starts, and then in each directory that it enters. It does this one time for each directory in a shell session. Thus loomd registers the repository of the directory. A `loom` shell function also changes the current directory to the repository of the task on `loom go`. The integration does nothing if `loom` is not on `PATH`.

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
