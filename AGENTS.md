# AGENTS.md

Instructions for coding agents working on this repository.

## What loom is

A personal work engine: a prioritized list of everything I have to do across contexts (personal, clients), synced with forges and trackers, recorded as an auditable ledger, and delegable to specialist agents under typed contracts and gates. Rust, single binary family (`loom`, `loomd`).

## Status

v0.2.0 is released (repo index); v0.3 (tasks core and TUI) is in progress. The crates are listed in `docs/design/architecture.md`. Anything describing a "multi-agent lifecycle engine" with 13 crates (old commits, old branches) is the **previous** loom and is superseded: do not use it as a reference.

## Read only what your task needs

Your task brief links the sections you need. Do not load the whole `docs/` tree.

| Topic | Document (`docs/design/` = target behavior) |
| --- | --- |
| Vision, non-goals | [docs/design/vision.md](docs/design/vision.md) |
| Processes, interfaces, crates, git, language | [docs/design/architecture.md](docs/design/architecture.md) |
| Tasks, events, notes, kinds, status, priority, contexts | [docs/design/domain-model.md](docs/design/domain-model.md) |
| Store layout, ownership, IDs, multi-machine | [docs/design/storage.md](docs/design/storage.md) |
| Repo index, contexts of repos, automatic updates | [docs/design/repos.md](docs/design/repos.md) |
| Issue and MR sync | [docs/design/tracker-sync.md](docs/design/tracker-sync.md) |
| MR review: discovery, mirror, refs, publish, forge credentials | [docs/design/reviews.md](docs/design/reviews.md) |
| Events, learnings | [docs/design/capture-and-learnings.md](docs/design/capture-and-learnings.md) |
| Agents, gates, runtimes, factory | [docs/design/agents.md](docs/design/agents.md) |
| TUI, gestures | [docs/design/user-workflow.md](docs/design/user-workflow.md) |
| Colors, spacing, badges, panes, modals, toasts (TUI and web) | [docs/design/ui-style.md](docs/design/ui-style.md) |
| Settled decisions | [docs/adr/](docs/adr/README.md) |
| Versions and acceptance criteria | [docs/roadmap.md](docs/roadmap.md) |
| Code, docs and git style | [docs/reference/style-guide.md](docs/reference/style-guide.md) |

## Toolchain

Pinned with [mise](https://mise.jdx.dev) in `.mise/config.toml`. Run `mise install` once.

The quality gate is `mise run check`; CI runs it on Linux and macOS. Pull requests that only touch docs (`docs/`, top-level `*.md`, `LICENSE`, except the generated references) run spelling and the secret scan only. The required check is `ci`; pull requests must be up to date with `main` before merging. `mise run fmt` formats the workspace; `mise tasks` lists every task.

## Working on a task

1. Your task is a GitHub issue written with `.github/ISSUE_TEMPLATE/agent-task.md`: goal, context links, scope, interfaces, acceptance criteria, required tests, dependencies. Stay inside its scope.
2. If the brief is insufficient or contradicts the docs, **stop and ask** instead of guessing. A question is a valid outcome; a guess presented as a fact is not.
3. List every assumption you made in the PR description.
4. One task = one PR, small enough to review in 20 minutes. Open it as a **draft**.

## Code rules

- Strict TDD for decision logic: write the failing test first.
- Decision logic is written as **pure functions** (no filesystem, network, clock or process), with I/O in the binaries. Inject time and randomness.
- `loom-api` and `loom-core` have no I/O and no async runtime.
- Errors: `thiserror` in library crates, `miette` in binaries. No `unwrap()`/`expect()` outside tests.
- Closed sets are enums; match exhaustively, no wildcard arm on domain enums.
- Public types that cross a boundary (API, CLI `--json`, MCP, agent contracts) derive `serde` and `schemars::JsonSchema`.
- Git writes go through the `git` CLI, never a library (ADR-0008). Never touch a dirty working tree, never resolve conflicts automatically.
- No new crate without updating the crate table in `docs/design/architecture.md`.
- No new dependency without a one-line justification in the PR description.
- Default to private visibility; public items get `///` docs.
- Doc comments that feed a generated reference (clap help, `///` on types that derive `JsonSchema`) follow the ASD-STE100 rules of the [style guide](docs/reference/style-guide.md#22-simplified-technical-english-asd-ste100).

## Tests

- Unit tests next to the code; integration tests in `crates/<crate>/tests/`.
- No real network or real forge in tests. Use local fake servers or fixtures.
- Test nominal and failure paths. Tests that touch the filesystem use temporary directories.

## Git

- Branch: `feat/<slug>`, `fix/<slug>`, `docs/<slug>`, `chore/<slug>`. Never commit on `main`.
- [Conventional Commits](https://www.conventionalcommits.org): `<type>(<scope>): <description>`.
- Never force-push, never rewrite published history.
- Every commit is signed (`commit.gpgsign=true`). Never use `--no-gpg-sign`; if signing fails, stop and report.
- PRs are merged by squash only (rebase merge is disabled), so `main` only receives commits signed by GitHub.
- To bring a branch up to date, merge `main` into it with a signed merge commit (or GitHub's "Update with merge commit"). Never use GitHub's "Update with rebase": it replays the commits unsigned and the ruleset then blocks the merge.
- Reviews use [Conventional Comments](https://conventionalcomments.org).

## Docs

Two kinds of documentation, never mixed:

- `docs/design/`: **target behavior** (design specs). Input for your task. If your implementation has to diverge, update the design doc in the same PR and say why.
- [Diátaxis](https://diataxis.fr/) folders: **shipped behavior** only, describing what the code does now, never what is planned.
  - `docs/reference/`: CLI commands and flags, config keys, file formats, API and JSON schemas, release assets. Dry, complete, no instructions.
  - `docs/how-to-guides/`: one goal per guide, steps for someone who knows the basics (install, upgrade, roll back…).
  - `docs/explanation/`: how and why a shipped mechanism works (e.g. auto-upgrade and rollback).
  - `docs/tutorials/`: learning by doing, starting with v0.3.

Rules:

- The Diátaxis folders and `README.md` follow the ASD-STE100 writing rules of the [style guide](docs/reference/style-guide.md#22-simplified-technical-english-asd-ste100).
- Any user-visible change updates the matching reference in the same PR.
- A new user procedure gets a how-to.
- The "Documentation" section of your issue lists what to write.
- A new structural decision needs a new ADR in `docs/adr/` (next number, status `proposed`).

## Security

- No secrets in code, tests, fixtures or logs. `gitleaks` must pass.
- Credentials are resolved at runtime through `*_cmd` / `*_env` configuration, never stored in plain text.
