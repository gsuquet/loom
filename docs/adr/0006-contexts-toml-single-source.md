# 6. `contexts.toml` is the single context source; git includeIf is generated

- Status: Accepted
- Date: 2026-11-09

## Context

Context ("who am I working for here") was inferred by at least three divergent tables
(path segments in two scripts, a third schema in the Rust loom) and separately encoded in
hand-written git `includeIf` blocks for identity routing. They disagreed on edge cases and
silently fell back to a default context.

## Decision

- `~/.config/loom/contexts.toml` (`$LOOM_CONTEXTS`, `--config`) defines contexts with
  `match.paths`, `match.hosts`, `kb`, `git_include`, `secrets`, `kube_context`, plus
  `[kb.<name>]` and `[git].protected_branches`. Validation is strict (unknown keys,
  relative paths and duplicate matchers are errors).
- Resolution: `--context` → `$LOOM_CONTEXT` → longest cwd path prefix (symlinks resolved) →
  git remote host prefix (insteadOf applied) → error. No default context.
- Contexts are user-defined; examples and tests use `context1`, `context2`, `context3`.
- `loom context gitconfig` generates `includeIf` blocks (`hasconfig:remote.*.url:` per host,
  `gitdir:` per path) ordered so git's last-include-wins mirrors loom's resolution; a
  dotfiles task installs them.

## Consequences

- Identity routing and loom's own behaviour cannot drift apart.
- Working outside a configured tree requires an explicit `-c`; this is intended.
- `hasconfig:remote.*.url` requires git ≥ 2.36.
