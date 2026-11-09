# 13. The dotfiles own the `loom sync` timer (`dot:loom:sync-timer`)

- Status: Accepted
- Date: 2026-11-09

## Context

`loom sync` should run every 15 minutes. The scheduler differs per OS (launchd on macOS,
systemd user timers on Linux). loom has no daemon (ADR 0004) and should not install
services into the user's system.

## Decision

The timer is defined and installed by the user's global dotfiles as the mise task
`dot:loom:sync-timer` (global dotfiles tasks are namespaced `dot:`). It only invokes
`loom sync`. loom ships no unit files and no `install-timer` command.

## Consequences

- loom stays a plain CLI; scheduling is visible and editable where other services live.
- Machines without the dotfiles get no automatic sync until a timer is set up by hand.
- Timer cadence changes do not need a loom release.
