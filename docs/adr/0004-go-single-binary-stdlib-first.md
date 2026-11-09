# 4. Go single binary, no daemon or database, stdlib-first dependencies

- Status: Accepted
- Date: 2026-11-09

## Context

The tool runs on macOS and Linux, is installed with mise and is called from shells, git
aliases and agent hooks. Startup time, install friction and long-term maintenance by one
person matter more than framework convenience.

## Decision

- Go, one static binary, `CGO_ENABLED=0`; no daemon, no background service, no database.
- Subcommands use the stdlib `flag` package (no cobra).
- Third-party dependencies: `github.com/BurntSushi/toml` and `gopkg.in/yaml.v3` only. Any
  other dependency requires a new ADR; `architecture_test.go` enforces the allowlist.
- Size budget ~4.5k non-test LOC, reported per package by `mise run loc` and enforced in CI.

## Consequences

- Trivial cross-compilation and install (`mise use github:gsuquet/loom`).
- Some boilerplate for flag parsing and help output that a framework would provide.
- The LOC budget forces scope discussions early instead of after the code exists.
