# Roadmap

> Versions, content and acceptance criteria. Only the next version is broken down into tasks.

| Version | Content | Done when |
| --- | --- | --- |
| v0.1 | Install, auto-upgrade, `dev` channel + `loom upgrade --from <path>`, rollback, loomd as launchd/systemd service, otel diagnostics, `loom version` / `loom status` | Clean install on macOS and Linux; a release is picked up in N h without action; a local build installs and rolls back; traces visible locally |
| v0.2 | Repo index (scan roots + register on use), contexts of repos, freshness, clean/dirty, fetch + ff-only on clean trees, repo cards, `loom search` | Dotfiles inventory scripts deleted; search < 100 ms; one week of auto-update with zero lost local changes |
| v0.3 | Tasks core: store, contexts, cron, archive, priority, multi-machine sync, TUI Today + Inbox + triage + task detail with in-place editing, `loom go` shell integration | I start my day with `loom` instead of anything else |
| v0.4 | Forge MRs: MRs to review (fetch `refs/*/head`, diff per SHA, MR mirror, TUI review view with drafts and batch publish), repo metadata, `loom pr` (draft, task-linked) | Reviews handled from loom without opening the forge inbox |
| v0.5 | Forge issues (GitLab, GitHub): two-way sync, creation with lint, TUI drafts view | A GitLab issue created and another edited in the TUI, both pushed with no change on untouched fields |
| v0.6 | Observation: events, Claude Code hooks, transcripts, calendar | A day of work reconstructable from the ledger |
| v0.7 | Delegation v1: one template (reviewer), versioned and measured from its first run, typed contract, readiness gate, context pack with preview, `needs_clarification`, agent findings inline in the review view, gates, proposed actions, loom MCP server, TUI run view + approval queue, desktop notifications | A review delegated, watched live, approved from the TUI |
| v0.8 | Jira two-way sync: ADF/wiki conversion with placeholders, transitions, ticket creation with lint | A Jira ticket edited and transitioned from loom, with no formatting damage on untouched content |
| v0.9 | Specialists (including the ticket writer), context packs, Datadog tools for agents (shaped results, spill to file) | An Investigation delegated with Datadog access, no credentials given to the agent |
| v0.10 | Factory on local runners: pipelines, intake rules, scheduler, autonomy policy, direct API runtime, replay evaluation | A Story labelled `loom:auto` reaches a draft MR overnight on my machine with no action from me, and I approve it in the morning |
| v0.11 | Central instance: StorePort over HTTP, central runner with one container per run, night windows, web UI | Long runs leave my laptop; a client context restricted to local runners never runs centrally |
| later | Freshservice, Slack, Datadog ingestion; learnings extraction; other harness adapters (Codex, Antigravity) | |

## v0.1 tasks

Task 0 (docs split, `AGENTS.md`, README) and task 2 are done. Issue numbers match task numbers up to 14. Each task is a GitHub issue in the `v0.1` milestone, labelled `agent-ready`, written with the [agent task template](../.github/ISSUE_TEMPLATE/agent-task.md).

| # | Issue | Task | Kind | Depends on | Owner |
| --- | --- | --- | --- | --- | --- |
| 1 | [#1](https://github.com/gsuquet/loom/issues/1) | Spike: `cargo-dist` (build, GitHub releases, installers, updater) vs custom pipeline -> ADR | Research | – | Agent |
| 2 | [#2](https://github.com/gsuquet/loom/issues/2) | Cargo workspace skeleton (`loom`, `loomd`, `loom-api`, `loom-update`), workspace lints, CI on macOS + Linux, mise tasks from scratch (`check` gate), `.gitignore` cleanup (commit `Cargo.lock`, drop old `.loom/` entries) | Chore | – | Agent |
| 3 | [#3](https://github.com/gsuquet/loom/issues/3) | `loom version`: version, commit, target, channel embedded at build time | Story | 2 | Agent |
| 4 | [#4](https://github.com/gsuquet/loom/issues/4) | Release pipeline: tag -> stable, merge to `main` -> `dev` prerelease, 4 targets (macOS/Linux × arm64/x86_64), checksums, signature | Story | 1, 3 | Agent + me (secrets, keys) |
| 5 | [#5](https://github.com/gsuquet/loom/issues/5) | `install.sh`: platform detection, download, checksum verification, idempotent install into `~/.local/bin` | Story | 4 | Agent |
| 6 | [#6](https://github.com/gsuquet/loom/issues/6) | `loom-api` types: status, versions, errors; serde + JSON Schema | Story | 2 | Agent, close review |
| 7 | [#7](https://github.com/gsuquet/loom/issues/7) | `loomd` skeleton: config, single-instance lock, HTTP over unix socket, `GET /status` | Story | 6 | Agent |
| 8 | [#8](https://github.com/gsuquet/loom/issues/8) | `loom status`: queries loomd, starts it if missing, flags CLI/daemon version mismatch | Story | 7 | Agent |
| 9 | [#9](https://github.com/gsuquet/loom/issues/9) | `loom service install\|uninstall\|status`: launchd user agent / systemd user unit, opt-out in config | Story | 7 | Agent |
| 10 | [#10](https://github.com/gsuquet/loom/issues/10) | Update decision logic (pure): channel, current version, releases, policy -> action | Story | 6 | Agent |
| 11 | [#11](https://github.com/gsuquet/loom/issues/11) | `loom upgrade [--channel] [--from <path>]`, `loom rollback`: download, verify, atomic swap, keep previous binary | Story | 4, 10 | Agent |
| 12 | [#12](https://github.com/gsuquet/loom/issues/12) | Auto-upgrade in loomd: periodic check, apply, restart through the service manager, health check, automatic rollback | Story | 9, 11 | Agent |
| 13 | [#13](https://github.com/gsuquet/loom/issues/13) | otel: `tracing` + configurable OTLP exporter (off by default), spans for CLI commands and daemon operations, `mise` task for a local backend | Story | 7 | Agent |
| 14 | [#14](https://github.com/gsuquet/loom/issues/14) | E2E in CI: fake release server -> install -> upgrade -> failed upgrade -> rollback, macOS and Linux | Story | 5, 12 | Agent |
| 15 | [#22](https://github.com/gsuquet/loom/issues/22) | CLI conventions ([ADR-0010](adr/0010-cli-conventions.md)): global flags, confirmations, broken pipe, exit codes, CLI reference generated from `clap` | Story | 3 | Agent |

Parallel tracks once 2 and 6 are merged: {3, 15, 4, 5}, {7, 8, 9, 13}, {10}. 11, 12 and 14 join them.

## v0.2 tasks

Each task is a GitHub issue in the `v0.2` milestone, written with the [agent task template](../.github/ISSUE_TEMPLATE/agent-task.md). Design: [Repositories](design/repos.md), [ADR-0012](adr/0012-repo-index.md).

| # | Issue | Task | Kind | Depends on | Owner |
| --- | --- | --- | --- | --- | --- |
| 1 | – | ADR-0012 (repo index, contexts, automatic updates) and `docs/design/repos.md` | Research | – | Me + agent |
| 2 | [#36](https://github.com/gsuquet/loom/issues/36) | `loom-repo` (pure): contexts, repo identity, `[contexts]` / `[repos]` / `[forges]` config | Story | 1 | Agent |
| 3 | [#37](https://github.com/gsuquet/loom/issues/37) | Repo scanner: roots, depth, exclusions, worktrees, removals, no git process | Story | 2 | Agent |
| 4 | [#38](https://github.com/gsuquet/loom/issues/38) | `loom-store`: SQLite index (`index.db`, FTS5), repo API endpoints in loomd | Story | 3 | Agent |
| 5 | [#39](https://github.com/gsuquet/loom/issues/39) | Repo state with `gix`: head, dirty, operation, ahead/behind, default branch, last commit, last fetch | Story | 4 | Agent |
| 6 | [#40](https://github.com/gsuquet/loom/issues/40) | `loom repo ls\|show\|scan\|register\|forget\|doctor`, repo resolution, registration on use | Story | 5 | Agent |
| 7 | [#41](https://github.com/gsuquet/loom/issues/41) | `loom search` for repos, ranking, < 100 ms | Story | 6 | Agent |
| 8 | [#42](https://github.com/gsuquet/loom/issues/42) | Automatic updates in loomd: fetch, ff-only on clean trees, scheduling, backoff, `loom repo sync` | Story | 5 | Agent |
| 9 | [#43](https://github.com/gsuquet/loom/issues/43) | `loom shell-init zsh\|bash`: registration on `cd` | Story | 6 | Agent |
| 10 | [#44](https://github.com/gsuquet/loom/issues/44) | Repo safety suite: every risky state, nothing lost, macOS + Linux | Chore | 8 | Agent |
| 11 | – | Rename the Python CLI to `loom-py`, update the dotfiles, a week of real use, delete the inventory scripts | Chore | 6, 8 | Me |

Parallel tracks once 5 is merged: {6, 7, 9} and {8, 10}.

## Open decisions

- One focus task at a time, or several.
- Presentation vs Content kind.
- Admin kind granularity (one kind, or Timesheet/Expense/Interview subtypes).
