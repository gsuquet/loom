# 14. Continuous improvement loop

- Status: Accepted
- Date: 2026-11-09

## Context

Self-improving agent harnesses create and patch their own skills from experience and curate them
by usage. They typically apply changes without approval and do not measure whether a change
improved outcomes. We want the learning, with measurement and a human gate, and we want the
system's own structure (schema, views, budgets, skills, tooling) to be improvable the same way.

## Decision

A loop over the context store:

1. **capture** (session end): extract candidate `fact`, `pref`, `decision`, `outcome` events from
   the transient transcript, secrets redacted; optional AI.
2. **consolidate** (nightly): dedupe, apply replacements, contradictions → `question`, expiry by
   age and non-use, rebuild views; no AI.
3. **learn** (weekly): recurring gotchas/fixes (≥3 outcomes) or repeated corrections (≥2) →
   draft `procedure` or skill patch as a `question`; optional AI.
4. **approve** (weekly review with the human): facts flow in automatically; procedures, skills
   and structural changes never change without approval.
5. **apply**: approved changes land as PRs (dotfiles for skills, loom or store repos for
   structure); versioned and revertable.
6. **measure**: `signal` events (corrections, reverts, CI failures after agent changes, review
   comments on agent PRs, use counts) per skill version, procedure and fact; regressions are
   flagged for rollback.

The structure itself is in scope: the loop may propose changes to the event schema, view
layouts and budgets, expiry rules, hooks, skills and loom, backed by signal evidence.

## Consequences

- Behaviour changes are always reviewable and revertable; no silent self-modification.
- Learning needs history: build order is schema + capture, then consolidation, then measurement,
  then learning.
- Without AI the loop still consolidates and measures; capture and learning simply do not run.
