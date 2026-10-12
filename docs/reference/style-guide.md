# Loom Documentation & Code Style Guide

> Guidelines for writing code, documentation, and commit messages for Project Loom.

---

## 1. Rust Code Style Standards

- **Formatting:** Enforce via `cargo fmt`. Never check in unformatted code.
- **Linter:** Strict `clippy` checks (`pedantic` + `nursery`). Part of `mise run check`.
- **Zero Panics:** Library crates (`loom-core`) forbid `unwrap()` and `expect()`. Use `Result<T, DomainError>`.
- **Visibility:** Default to private. Expose public APIs thoughtfully with complete Rustdoc comments (`///`).
- **Error Types:** Use `thiserror` in libraries and `miette` in the CLI binary.

---

## 2. Documentation Style (The Good Docs Project)

- **Voice & Tone:** Direct, active voice, empathetic to developer workflow challenges.
- **Structure:** Clear hierarchy, task-based instructions, and concrete code examples.
- **Headings:** Sentence case or Title Case consistently.
- **Code Blocks:** Always annotate language (e.g. ````rust````, ````bash````, ````toml````).

---

## 3. Git & Review Standards

- **Commit Messages:** Strict adherence to [Conventional Commits](https://www.conventionalcommits.org):
  - Format: `<type>(<scope>): <short description>`
  - Types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`.
- **Code Reviews:** Strict adherence to [Conventional Comments](https://conventionalcomments.org):
  - Labels: `issue`, `suggestion`, `nitpick`, `todo`, `thought`, `praise`.
  - Decorations: `(blocking)`, `(non-blocking)`, `(if-minor)`.
