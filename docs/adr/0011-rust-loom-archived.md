# 11. The Rust loom is archived as `loom-legacy`

- Status: Accepted
- Date: 2026-11-09

## Context

A previous Rust implementation named loom grew a broader scope (work-unit lifecycle,
orchestration) and a config at `~/.loom/config.toml`. The new design narrows the scope and
changes language, storage and command surface; incremental migration would carry the old
shape along.

## Decision

The Rust repository is renamed `loom-legacy` and archived. This repository starts from an
empty history. The only thing carried over is the `match.paths` / `match.hosts` schema,
reused in `contexts.toml`.

## Consequences

- No compatibility layer or migration code for the Rust loom's data.
- The name `loom` and the `github:gsuquet/loom` mise backend point at this implementation.
- Users of the old binary must switch explicitly; the archived repo remains readable.
