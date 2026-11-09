# AGENTS.md

Rules for anyone (human or agent) changing this repository.

## Before committing

- Run `mise run fmt ::: lint ::: test ::: loc` and keep it green.
- Tests: table tests; golden files under `testdata/` (`go test ./... -update` to rewrite,
  then review the diff); git behaviour against temp repos (`t.TempDir()` + `git init`),
  with `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_NOSYSTEM` isolated; no network.

## Constraints

- LOC budget: ~4.5k non-test lines in total (`mise run loc`, enforced in CI). If a change
  needs more, discuss scope first; do not raise the budget silently.
- Package boundaries (enforced by `architecture_test.go`): command packages (`gitcmd`, `kb`,
  `render`, `agentlog`, ...) never import each other; they share only `contexts`, `forge`,
  `gitx`. Only `cmd/loom` imports `internal/cli`; `cmd/loom` is wiring only.
- Dependencies: stdlib, `github.com/BurntSushi/toml`, `gopkg.in/yaml.v3`. No new dependency
  without an ADR in `docs/adr/` (MADR-lite: Status, Context, Decision, Consequences).
- No cgo. No secrets in code, tests or fixtures; credentials come from env only.
- Public repo: use example values (`github.com/your-user`, `gitlab.example.com`) in docs
  and fixtures, never real hosts, paths, organizations or context names.
- Design changes go through `docs/design.md` + an ADR before code.
