# ADR-0012: Repo index, contexts and automatic updates

- Status: accepted
- Date: 2026-10-12

## Context

v0.2 replaces the inventory scripts of the dotfiles. Their behaviour, inventoried before this decision:

- Repos are found by walking fixed roots to depth 4 without starting git, reading `.git/config` and `HEAD` directly.
- A repo's context is the first directory under `$HOME`, mapped through a hard-coded table; an unknown directory becomes a context of its own. Two other scripts (`git-review`, `git-pr`) carry their own, different rules.
- The result is written to JSON files read directly by agent instructions. Repos deleted from disk are never purged.
- Nothing fetches or updates repos.

The design docs placed contexts and SQLite in v0.3 and left the repo card undefined.

## Decision

1. **Contexts are configured in v0.2**: `[contexts.<name>]` with `match.hosts` and `match.paths`, plus `default_context`. A repo's context is, first match wins: per-repo override, hosts, paths, default context, otherwise `unassigned`. No context is ever inferred from a directory name. These rules are the single source of truth; other tools ask loom.
2. **SQLite index from v0.2**: `~/.loom/index.db`, machine-local, rebuildable from a scan, FTS5 for search, written only by `loomd`. The `loom-store` crate is introduced in v0.2 with the repo tables only (`rusqlite`, bundled SQLite) and grows in v0.3.
3. **Repo card** = computed state in the index (read with `gix`, ADR-0008) + editable overrides in `config.toml` (`[repos."<path>"]`). Card files in context stores wait for v0.3.
4. **Automatic updates** by `loomd`, policy `ff | fetch | off` per context and per repo, default `ff`. A fast-forward happens only on a clean tree, with no operation in progress, on a branch not ahead of its upstream, through `git merge --ff-only`; the local default branch may be moved with `git fetch <remote> <b>:<b>` when not checked out. Nothing is ever rebased, reset, stashed or deleted. git runs non-interactively (`GIT_TERMINAL_PROMPT=0`, SSH `BatchMode` unless the user configured an SSH command) and honours the user's config and hooks.
5. **No compatibility files**: `~/.cache/git/*.json` are not written. Scripts and agent instructions use `loom repo ls --json` / `loom repo show --json`.
6. **Registration on use** through any `loom` command run inside a repo and a shell hook (`loom shell-init`), brought forward from v0.3.

## Consequences

- One definition of contexts for repos now and tasks later; the v0.3 domain work reuses it.
- SQLite and FTS5 arrive one version early; `loom-store` starts small.
- Automatic fast-forwards rely on git's own refusals (`--ff-only`, untracked files) on top of loom's checks: two independent guards against lost work.
- `post-merge` hooks run on automatic fast-forwards, as on a manual pull.
- On Linux, automatic fetches over SSH need the agent socket visible to the systemd user unit; `loom repo doctor` reports it.
- The dotfiles must update their agent instructions and allowlists, and the former Python CLI is renamed `loom-py` so that `~/.local/bin/loom` is the Rust binary.
