# Changelog

All notable changes to Project Loom will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

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
