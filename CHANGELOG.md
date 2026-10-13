# Changelog

All notable changes to Project Loom will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Changed
- Workspace version is `0.2.0`: dev prereleases are now `0.2.0-dev.N`, above `0.1.0`.

### Fixed
- A machine installed with `install.sh --channel dev` no longer leaves the dev channel after the auto-upgrade moves it to a stable release. The channel given to `loom upgrade --channel` (run by `install.sh --channel`) is remembered in `state/update.json`; the effective channel is `[update] channel`, then the remembered one, then the build channel, and `loom status` shows which (`config`, `chosen`, `build`). Fields of `state/update.json` unknown to a version are now kept when it rewrites the file.

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

[Unreleased]: https://github.com/gsuquet/loom/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/gsuquet/loom/releases/tag/v0.1.0
