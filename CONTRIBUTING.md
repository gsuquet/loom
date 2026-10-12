# Contributing to Project Loom

Thank you for your interest in contributing to Loom! Loom is a personal work engine written in Rust: one prioritized list across contexts, synced with forges and trackers, delegable to agents. See the [README](README.md).

---

## Code of Conduct

All contributors and maintainers are expected to abide by our [Code of Conduct](CODE_OF_CONDUCT.md). Please report any violations to the project maintainers.

---

## Core Engineering Principles

Before contributing, please note our non-negotiable architectural standards:
1. **Design-First:** Features follow the design in `docs/design/` and the settled decisions in `docs/adr/`. A new structural decision needs a new ADR. Work is split into small tasks described in GitHub issues (see [AGENTS.md](AGENTS.md)).
2. **Strict Test-Driven Development (TDD):** Red-Green-Refactor is strictly enforced. Write failing unit/port tests before writing production code.
3. **Pure Core:** `loom-api` and `loom-core` have no I/O and no async runtime. Decision logic is written as pure functions; binaries own I/O (see [architecture](docs/design/architecture.md)).
4. **Conventional Commits & Comments:** Commit messages must follow [Conventional Commits](https://www.conventionalcommits.org) (e.g. `feat:`, `fix:`, `docs:`, `refactor:`, `test:`). Code review feedback must adhere to [Conventional Comments](https://conventionalcomments.org).

---

## Development Environment Setup

Loom uses [`mise`](https://mise.jdx.dev) to manage its toolchains deterministically:

```bash
# Clone repository
git clone https://github.com/gsuquet/loom.git
cd loom

# Install pinned Rust 1.99.0 and developer utilities
mise install

# Run the quality gate
mise run check
```

---

## Contribution Workflow

1. **Fork & Branch:** Create a feature branch matching our convention: `feat/<slice-or-feature-name>` or `fix/<issue-name>`.
2. **Implement with TDD:**
   - Add unit tests in `crates/<crate-name>/src/` or integration tests in `crates/<crate-name>/tests/`.
   - Verify tests fail (Red).
   - Implement minimal domain/adapter code (Green).
   - Refactor and run the quality gate: `mise run check`.
3. **Pre-flight Quality Gate:** Ensure all checks pass before pushing:
   ```bash
   mise run check
   ```
4. **Submit a Draft PR:** Open a Pull Request marked as **Draft** for human review and CI verification.
