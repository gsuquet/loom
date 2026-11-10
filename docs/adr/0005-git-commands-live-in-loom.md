# 5. Git workflow commands live in loom, with package boundaries

- Status: Accepted
- Date: 2026-11-09

## Context

Repository inventory, PR/MR creation and review workspaces were three separate scripts
with diverging context tables, forge detection and cache paths. They need the same
context resolution as the KB commands.

## Decision

`loom git inventory|pr|review` replace the scripts in the same binary (git aliases
`git pr`, `git mr`, `git review` point at them). To keep one binary from becoming one
tangle:

- command packages (`gitcmd`, `kb`, `render`, `agentlog`) never import each other;
- they share only `contexts`, `forge` and `gitx`;
- only `cmd/loom` imports `cli`, and `cmd/loom` imports nothing else internal.

`architecture_test.go` enforces these rules on every test run.

## Consequences

- One context source, one forge adapter, one release for all workflows.
- Shared behaviour must be pushed down into `contexts`/`forge`/`gitx` deliberately rather
  than imported sideways.
- Removing a command group is deleting a package plus its `cli` file.
