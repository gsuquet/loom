# Changelog

All notable changes to Project Loom will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Added
- Task commands: `loom add <title>` captures a task (inbox unless `--ready`; `--kind`, default `chore`; `--importance`; `--due` as `YYYY-MM-DD`, `today`, `tomorrow`, `+<n>d` or `+<n>w`; `--parent <id>`), `loom ls` prints the priority list of a context (`--why` adds the rule of each line, `--status` lists one status, `--all-contexts` every context), `loom show <id>` prints a task and its notes. The context is `-c`, then `LOOM_CONTEXT`, then the repository of the current directory, its `match.paths` prefix, `default_context`. Every command takes `--json` (`TaskDetail`, `TaskList`) and waits up to 15 s for loomd.
- Task gestures: `loom set <id> key=value…` (`title`, `kind`, `format`, `importance`, `due`, `parent`, `pinned`; `none` removes a due date or a parent), `loom done`, `loom cancel`, `loom snooze <id> <until> [--on <who>]` and `loom undo`, each printing the task on one line or its `TaskDetail` with `--json`. `loom edit <id>` opens `task.md` in `VISUAL` or `EDITOR`, reports an invalid file and keeps it as saved, and waits for loomd to record the edit.
- `loom service restart [--json]`: restarts loomd and returns once the new one answers, so a change to `config.toml` takes effect. The configuration is checked first (invalid: loomd is left running). A loaded user service is restarted by its manager (`launchctl kickstart -k`, `systemctl --user reset-failed` then `restart`), after a `SIGTERM` to a loomd running outside it; without one, loomd gets `SIGTERM` and a new one is started detached; a loomd not running is started. Fails with exit code 1, naming the step and the log, when the old loomd does not exit within 10 s (never `SIGKILL`) or no new loomd answers within 15 s. `--json` prints `DaemonRestartReport` (`was_running`, `previous_pid`, `pid`, `version`, `via`: `launchd`, `systemd`, `signal` or `start`, `elapsed_ms`).
- Disk usage of the loom home: `loom status` prints `disk: <total> (versions …, index …, logs …, contexts …)`, and `GET /v1/status` (`loom status --json`) carries a `disk` object with the allocated bytes of `versions/`, the index (and its WAL), `log/`, `state/`, each context store and the rest, the kept versions and the free space of the volume. Measured by loomd without following symbolic links, cached 5 minutes and measured again when `versions/` changes; an unreadable directory counts as 0 with a warning.
- `[contexts.<name>] store.path` in `config.toml`: root of the context's task store, `<loom home>/contexts/<name>` by default; two contexts cannot share one.
- Task store: loomd creates `tasks/` in each context store and indexes the task files (`tasks/<id>/task.md`, `events.jsonl`) in `index.db` when it starts, with full-text search over title, notes and link keys. An invalid `task.md` is indexed read-only with its error and never rewritten. Task files are written atomically (temporary file, fsync, rename, fsync of the directory) under a per-task lock.
- `loom reindex [--json]` (`POST /v1/tasks/reindex`, `ReindexResult` schema): rebuilds the tasks index from the files, lists the invalid ones and the ones that cannot be read (which keep their previous row, without stopping the others), leaves the repositories untouched. A deleted `index.db` gets the same task rows back.
- Hand edits of `task.md`: loomd watches the task stores (FSEvents on macOS, inotify on Linux) and reconciles every task at start. A changed field is indexed and recorded as an `edited` event of the new actor `file` (the diff between the file and the task its log describes); `task.md` is never rewritten for it. A notes-only change is indexed without an event; an invalid edit is flagged read-only. A task that cannot be read is logged with its path and skipped, the others are reconciled. `loom reindex` reconciles first, so it may append `edited` events.
- `loom search` covers the tasks: title, notes and link keys (`CORE-1042`, `!88`), with the tokenizer of the repository search. Results are ranked active, ready and waiting tasks first, then repositories, then inbox tasks, then done and cancelled ones; among tasks, a link key equal to the query first, then title matches, then most recently changed. `--type task|repo` (`type` in `GET /v1/search`) keeps one kind. The table gains `TYPE`, `CONTEXT` and `STATUS` columns (`TYPE  ITEM  CONTEXT  STATUS  MATCHED  DETAIL`): every task shows its context. In `SearchResults`, a task result has `kind` `task` and its own fields (`id`, `context`, `title`, `status`, `link_keys`, `score`, `matched_fields`); a repository result keeps its v0.2 fields. About 15 ms on 1,000 repositories and 5,000 tasks. A search right after `loomd` starts waits (up to 10 s) for the task files to be indexed instead of answering from a partial index; `loom search` waits 15 s for the answer (5 s for the other commands).
- Tasks API in loomd: `GET /v1/tasks` (by context and status, or the priority list with `ranked=true`, ordered by `loom-core`'s rule on the local date), `POST /v1/tasks`, `GET` and `PATCH /v1/tasks/{id}`, `POST /v1/tasks/{id}/actions/{action}` (status changes, `pin`, `unpin`, `log_note`), `GET /v1/tasks/{id}/events` and `POST /v1/tasks/{id}/undo` (the compensating change computed from the log), with JSON Schemas. `{id}` takes a unique ID prefix; changes run one at a time on the task thread, recorded as the actor `user`. New error codes, with typed `details`: `ambiguous_prefix` (the candidates), `read_only`, `illegal_transition`, `conflict`, `nothing_to_undo`. Right after `loomd` starts, task reads (and the `{id}` prefix resolution of every endpoint) wait up to 10 s for the tasks to be indexed, like a search.
- `~/.loom/machine.toml` (local, not versioned): `id` names the machine, created at first start with the host name. Events record it as their `machine_id` instead of the host name. `[contexts.<name>] machines = [...]` lists the machines that hold a context: on any other machine its store is not loaded.
- Each context store is a git repository, created by loomd at its first commit with a `.gitattributes` (`*.jsonl merge=union`) and a `.gitignore` (temporary files). Every change is committed right after it is written, `<type>(<id>): <summary>` (`edited(k3q7zm2a): title`), never signed and without hooks whatever the git configuration says; the user's identity applies. Each creation, edit, action and undo of the tasks API is one commit. A commit that fails is logged, the change stays written and the next commit of the store takes it.
- Backup of the context stores: `[contexts.<name>] store.backup.remote` in `config.toml`. loomd pushes the store's commits, or fast-forwards the store, at start, every `[tasks] sync_interval` (default 15 minutes) and on `loom sync [-c <context>] [--json]` (`POST /v1/tasks/sync`, `BackupRequest` and `BackupReport` schemas; `GET /v1/tasks/backup` reads the state). A store that diverged from its remote is reported and never merged, rebased or forced; a refused push keeps the commits local. `loom repo doctor` adds a `machine` check and a `backup <context>` check per store. How-to: back up context stores.
- `loom repo doctor` checks the disk usage: `disk` (parts that could not be measured), `disk-versions` (versions kept beyond what `loom rollback` needs), `disk-wal` (`index.db-wal` above 64 MB) and `disk-free` (less than 1 GB free on the volume of the loom home).

### Fixed
- loomd refuses a task parent (`relations.parent` of `POST /v1/tasks` or `PATCH /v1/tasks/{id}`) that is not an indexed task of the same context, or that is the task itself or one of its descendants: `400 invalid_request` naming the problem. Before, a create or an edit could link a task to another context's task or make it its own ancestor.
- On Linux, the pre-flight check of an upgrade and the probe of a staged release retry for up to about a second when the new binary is still busy (`Text file busy`): another thread forking while the file was written could make a valid upgrade fail.

## [0.2.0] - 2026-10-13

Repository index (v0.2): loom knows every git repository on the machine, which context it belongs to, whether it is up to date, and keeps it fetched.

### Added
- Contexts in `config.toml`: `default_context` and `[contexts.<name>]` with `match.hosts` (remote host and path prefix) and `match.paths`. A repository's context is, first match wins: `[repos."<path>"] context`, then hosts, then paths, then `default_context`, otherwise `unassigned`; never guessed from a directory name.
- Repository index in loomd (`~/.loom/index.db`, SQLite): scan of `[repos] roots` to `max_depth` without starting git (`.git` directories and files, worktrees, exclusions, removals), stable slugs (`<context>/<name>`), repositories registered by hand kept while their `.git` exists, schema migrations that refuse a newer database.
- Repository state read with `gix`, without starting git: head, tracked changes, untracked files, operation in progress, upstream ahead/behind, default branch, last commit, last fetch. A field that cannot be computed in its 2 s budget is `unknown`; filter drivers (git-lfs and others) are never run.
- `loom repo ls|show|scan|register|forget|doctor`: the index for humans, scripts (`--json`) and pickers (`--format paths`); repositories named by path, `ctx/name` or name; `doctor` checks roots, contexts, unassigned repositories, loomd's SSH agent and non-interactive SSH to every host.
- Registration on use: any `loom` command run inside an unindexed repository registers it (50 ms budget, never starts loomd); `loom shell-init zsh|bash` registers each directory the shell enters, in the background.
- `loom search <query>`: every word as a prefix of a word of the name, slug, path, remotes, context or branch; case and diacritics ignored; ranked by exact name, name prefix, slug, then most recently used. About 10 ms for 1,000 repositories.
- Automatic repository updates in loomd: `git fetch --no-prune` of the primary remote every `[repos] fetch_interval` (jittered, `fetch_concurrency` at a time, per-host backoff), then a fast-forward of the checked-out branch only when nothing local can be affected (no tracked change, no operation in progress, on a branch, upstream present, nothing ahead), and of the local default branch when it is checked out nowhere and strictly behind. Policy `ff | fetch | off` per context and per repository (`unassigned` repositories default to `fetch`). Never rebase, reset, stash, checkout, delete or prune; git runs non-interactively and honours the user's configuration and hooks. `loom repo sync [-n]` runs it now.
- API: `/v1/repos` (list, card, scan, register, forget, sync), `/v1/search`, `/v1/diagnostics`, with JSON Schemas.
- A safety suite in CI drives loomd over 23 repository states (dirty, staged, colliding untracked file, conflicted, ahead, diverged, detached, every operation in progress, worktree, submodule, filter driver, failing hook, tags, stash) and checks that nothing changes beyond what the update allows.

### Changed
- Workspace version is `0.2.0`: dev prereleases are now `0.2.0-dev.N`, above `0.1.0`.
- `loomd` needs `HOME` (to expand `~` in `[repos]` and `[contexts]`); an invalid `[contexts]`, `[repos]` or `[forges]` table stops it at start, like any configuration error.
- The pre-flight check of an auto-upgrade runs the new `loomd` with a throwaway `HOME`: it never walks the real home.
- CI: documentation-only pull requests run spelling and the secret scan only; the required check is `ci`; pull requests must be up to date with `main`; the release dry run and the end-to-end jobs only run when what they cover changes; no dev prerelease for a merge that changes nothing shipped.
- Dependencies refreshed within their requirements (`schemars` 1.2.3, `sha2` 0.11.1, `rustls-native-certs` 0.8.5, `syn` 3.0.9).

### Fixed
- A machine installed with `install.sh --channel dev` no longer leaves the dev channel after the auto-upgrade moves it to a stable release. The channel given to `loom upgrade --channel` (run by `install.sh --channel`) is remembered in `state/update.json`; the effective channel is `[update] channel`, then the remembered one, then the build channel, and `loom status` shows which (`config`, `chosen`, `build`). Fields of `state/update.json` unknown to a version are now kept when it rewrites the file.
- Registration on use writes the repository and `last_used` in one transaction (a reader could see a newly registered repository without `last_used`).
- Automatic repository updates no longer fail for good on a tag moved upstream (`! [rejected] 1.0.0 -> 1.0.0 (would clobber existing tag)` under `fetch.pruneTags` or `remote.<name>.tagOpt = --tags`): loomd fetches with `--no-prune --no-prune-tags --no-tags`, so updates only move branches and leave tags to you.
- An update that meets a lock held by another git process (`index.lock: File exists`, `cannot lock ref … but expected …`) is now `skipped` with the new reason `busy`, retried at the next interval without backoff, instead of `failed`. Locks are never removed.
- `loom repo doctor` checks each remote host the way updates reach it: loomd runs `git ls-remote` in one repository per host and scheme (new `POST /v1/diagnostics/remote-check`), with its environment and the user's git configuration (`core.sshCommand`, `includeIf`, credential helpers), HTTPS remotes included. It replaces the `ssh -T` check run from the shell, which failed where loomd's fetches succeeded. A new `updates` check groups the repositories whose last update failed or was skipped by reason, with a fix for each.

## [0.1.0] - 2026-10-12

First release of the rewrite: install, run and keep loom up to date.

### Added
- `install.sh`: installs `loom` and `loomd` into `~/.local/bin` (`--prefix`), stable or `dev` channel or a given version, mandatory SHA-256 check, minisign signature check when `minisign` is installed, idempotent, never edits shell rc files. Bootstrap: `curl -fsSL https://github.com/gsuquet/loom/releases/latest/download/install.sh | sh`.
- Releases for macOS and Linux (arm64, x86_64, static musl builds on Linux): stable on `vX.Y.Z` tags, `dev` prerelease on every merge to `main` (last 10 kept), `SHA256SUMS` signed with minisign (key `0C6B3C0017776D53`).
- `loom version [--json]`, `loomd --version`: version, commit, target, channel and profile embedded at build time.
- `loomd`: single instance per loom home, HTTP + JSON API over a unix socket (`GET /v1/status`), `config.toml` with located errors, graceful shutdown.
- `loom status [--json] [--no-start]`: queries loomd, starts it when needed, warns when the CLI and the daemon versions differ, shows the update state.
- `loom service install|uninstall|status`: loomd as a launchd user agent (macOS) or a systemd user unit (Linux), `-n/--dry-run`, opt-out with `[daemon] service = false`.
- `loom upgrade [--channel|--version|--from] [--check] [-n]` and `loom rollback`: signed and checksummed downloads, atomic replacement of both binaries, the two previous versions kept, local builds with `--from`, OS trust store for TLS.
- Auto-upgrade in loomd (`[update] policy = auto|notify|off`, `channel`, `check_interval`): pre-flight check of the new daemon, post-restart watchdog run from the previous version, automatic rollback, failed versions never retried automatically, recovery through the service manager.
- Telemetry, off by default (`[telemetry]`): OTLP/HTTP traces of CLI commands, API requests and upgrades, with CLI-to-daemon trace propagation; `mise run otel-dev` starts a local Jaeger.
- CLI conventions (ADR-0010): global `-c`, `-q`, `-v`, `--color`, `-y`; exit codes 0/1/2/130; CLI reference generated from the code.
- End-to-end upgrade scenario in CI on Linux, macOS and under a systemd user service.

### Changed
- Rewrite from scratch. The previous workspace (13 crates) is removed.
- Design documentation in `docs/design/` (target behavior), decisions in `docs/adr/`, plan in `docs/roadmap.md`, `AGENTS.md` for coding agents.

[Unreleased]: https://github.com/gsuquet/loom/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/gsuquet/loom/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/gsuquet/loom/releases/tag/v0.1.0
