# 10. Agent recording scope: plans, report, changes, artifacts — no sessions

- Status: Accepted
- Date: 2026-11-09

## Context

AI agents do a growing share of the work. What matters later is what was planned, what
was done and what changed, not the conversation. Transcripts are large, noisy, may contain
secrets and are tool-specific.

## Decision

Each unit of agent work is recorded under
`bronze/agents/YYYY/<date>-<repo>-<branch>/` in the KB of the repo's context:

- `plan.md`: copied by a hook when the agent's plan is written/approved;
- `report.md`: written by the agent at the end, per the KB's AGENTS.md;
- `changes.md`: commits, PR/MR links, files touched, mutating non-git commands (name and
  target only);
- `artifacts/`: declared outputs (`$LOOM_ARTIFACTS`) and discovered files, 20 MB cap.

Coding-agent hooks call `loom agent hook <event>`. Sessions, prompts and transcripts are
never recorded.

## Consequences

- Records are small, reviewable markdown and respect context isolation.
- The "why" depends on agents writing a decent report; AGENTS.md rules carry that.
- Guardrails (what agents may do) are a separate design.
