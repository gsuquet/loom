---
name: Agent task
about: A task brief an agent can implement in one PR
labels: agent-ready
---

## Goal
<one sentence, the observable result>

## Context
- docs/design/<doc>.md (section)
- docs/adr/<n>-<title>.md

Read only these; do not load the whole `docs/` tree.

## Scope
**In:** <crates, files, commands>

**Out:** <what not to touch>

## Interfaces
<types, commands, endpoints to expose>

## Acceptance criteria
- [ ] <testable behaviour>
- [ ] <error case handled>

## Documentation
<Diátaxis docs to write or update, describing shipped behavior: reference (CLI, config, formats), how-to (new procedure), explanation (mechanism). "None" if nothing is user-visible.>

## Required tests
<unit / integration / e2e, nominal and failure paths>

## Constraints
`mise run check` green. No new dependency without a one-line justification in the PR. Draft PR, Conventional Commits, assumptions listed in the PR description. If the brief is insufficient or contradicts the docs, stop and ask in this issue instead of guessing.

## Depends on
#<n>
