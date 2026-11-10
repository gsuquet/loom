# 12. `loom git` behaviour changes vs the legacy scripts

- Status: Accepted
- Date: 2026-11-09

## Context

`loom git inventory|pr|review` replace three scripts whose behaviour was specified from
the code. Several behaviours were unsafe defaults, dead flags or bugs. Parity is the goal
except where listed here.

## Decision

- G1 `review post` creates a draft/pending review by default; `--publish` posts it.
- G2 `pr` opens draft PRs/MRs by default; `--ready` opens them ready for review.
- G3 `inventory -c <ctx>` reads the cache filtered by context; `--refresh` rescans.
- G4 Scans cover every configured root and prune deleted repos; `--root`, `--max-depth`
  and `--output` are dropped.
- G5 The base branch is taken from the full remote HEAD ref (`release/2.0` stays intact);
  `view` and `diff` are allowed on protected branches.
- G6 The editor template is cut at a scissors line instead of dropping every `#` line, so
  markdown headings survive.
- G7 Review comments without a `challenger` verdict are skipped with a warning.
- G8 Lint/test detection uses exact mise task names (`lint`, `test`), not substrings.
- G9 JSON output is plain UTF-8; parity tests compare structure, not bytes.
- G10 The clipboard is written only on an interactive TTY and never with `--json`.

## Consequences

- Nothing is published or marked ready without an explicit flag.
- Agent instructions that pass `--draft` keep working; those relying on immediate
  publication must add `--publish`/`--ready`.
- Golden tests encode these deviations explicitly.
