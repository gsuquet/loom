# ADR-0008: git CLI for writes, gix for reads

- Status: accepted
- Date: 2026-10-12

## Context

Loom fetches, fast-forwards, creates worktrees and checks out MR refs in my repositories, and scans many repositories for the index. Writes must behave exactly like my own git usage.

## Decision

Every git write goes through the `git` CLI. Read-only, high-volume inspection uses `gix`. Loom never resolves conflicts and never touches a dirty working tree.

## Consequences

- Writes honour my git config, credential helpers, hooks and signing.
- Process spawning cost on writes is acceptable (writes are rare compared to reads).
- Two git implementations to keep consistent on reads vs writes.
