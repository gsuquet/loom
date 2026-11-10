# 15. Skills for judgment, loom for mechanics

- Status: Accepted
- Date: 2026-11-09

## Context

Large skill catalogues are expensive to maintain and drift; deterministic work done by agents
is slow and inconsistent. Both supported coding agents read `SKILL.md` skills.

## Decision

- loom (no AI, offline) owns everything deterministic: hooks, note creation and property changes, validation,
  view compilation, lint, sync, accept, consolidation, measurement.
- Ten agent-agnostic skills own the judgment work: `recall`, `record`, `task`, `decide`, `research`,
  `investigate`, `pr-review`, `interview`, `distill`, `memory-review`. Each ≤~150 lines, kept in the dotfiles, pointing to the
  stores' own rules rather than duplicating them.
- Until loom implements a mechanic, the skill performs it with plain tools (`rg`, `jq`, git)
  and switches to the loom command when it exists.

## Consequences

- Skills stay small and stable; changes to them go through the improvement loop and approval.
- Work can start before loom slice 1b ships.
