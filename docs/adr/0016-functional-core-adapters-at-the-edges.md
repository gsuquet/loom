# 16. Functional core, adapters at the edges

- Status: Accepted
- Date: 2026-11-10
- Supersedes: the package-boundary part of [ADR 0005](0005-git-commands-live-in-loom.md)
  (its `kb`/`render`/`agentlog` packages no longer exist in the design)

## Context

loom is mostly written by agents, under a ~4.5k LOC budget, and its value is in deterministic
rules: context resolution, note conventions, claim semantics, view ranking and budgets. Those
rules are easy to test exhaustively when they are plain functions over values, and hard to
test when they shell out to `git`, `gh` or `glab` or touch the filesystem as they go. ADR 0005
fixed package boundaries for a package list that design.md has since replaced.

## Decision

- **Functional core**: `contexts` (resolution, gitconfig generation), and from slice 1b
  `memstore` (note rules: kinds, properties, tags, links, supersede and expiry semantics) and
  `views` (building and budgets). Core packages run no subprocess: they take facts as values or
  injected functions and return values or errors. Filesystem access stays in thin functions at
  the package edge (`contexts.Load`, symlink resolution in `Resolve`, note writes in 1b).
- **Imperative shell / adapters**: `cli` (flags, output, wiring), `gitx` (git), `forge`
  (`gh`/`glab`), `hooks` (agent harness events), and file writes. All subprocesses go through
  `gitx` or `forge`.
- **Value types at the edge**: kinds, statuses, tags and note stems are parsed once into
  validated types when read; the core never handles raw strings for them.
- **Claims are append-only** (event sourcing, light): a claim note is never rewritten, not even
  by a migration; it is replaced by a new note that `supersedes` it.
- **Views are projections** (CQRS, light): pure, deterministic, rebuildable from the notes,
  never edited by hand or by agents.
- **Anti-corruption layer**: `forge` maps `gh`/`glab` JSON into loom types at the boundary; no
  forge JSON shape leaks past it.
- **Interfaces** only where two real implementations exist: forge (GitHub, GitLab) and agent
  harness. Everything else is concrete types and function values.
- **Packages and allowed edges** (replaces ADR 0005's list):
  - `cmd/loom` imports only `cli`; only `cmd/loom` imports `cli`; `cli` may import any package.
  - Shared packages: `contexts`, `forge`, `gitx`. Command packages: `gitcmd`, `memstore`,
    `views`, `hooks`, `knowledge`. Command packages import only shared packages, plus the
    planned edges `views` → `memstore` and `hooks` → `memstore`/`views` (slice 1b).
  - Core packages (`contexts`, `memstore`, `views`) never import `os/exec`, `gitx` or `forge`
    in non-test code. Their tests may run real git.

## Consequences

- Core logic is tested with table tests over values; real git and fixtures are needed only
  for adapters and for the few end-to-end tests that check the wiring.
- Callers do a little more wiring (e.g. `cli` passes a git remote lister to
  `contexts.Resolve`).
- Migrations can add notes but cannot rewrite claims, which keeps history auditable at the cost
  of more files.

## Confirmation

`architecture_test.go`: `TestPackageBoundaries` (import edges, `cli` and `cmd/loom` rules),
`TestCoreRunsNoSubprocess` (core packages), `TestThirdPartyDeps` (ADR 0004). The planned
`views`/`hooks` edges are a TODO in that test until slice 1b. Value types, append-only claims
and pure views are checked by `memstore`/`views` tests when those packages land; until then,
review only.
