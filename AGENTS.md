# AGENTS.md

Rules for anyone (human or agent) changing this repository.

## Before committing

- Run `mise run fmt ::: lint ::: test ::: loc` and keep it green. CI also runs
  `mise run test:race` (race detector) and `mise run vuln` (govulncheck, needs network).
- Tests: table tests; golden files under `testdata/` (`mise run golden` to rewrite, then
  review the diff; add new golden packages to that task); git behaviour against temp repos
  (`t.TempDir()` + `git init`); no network.
- Every test that runs git isolates `GIT_CONFIG_GLOBAL`, `GIT_CONFIG_NOSYSTEM`,
  `GIT_CEILING_DIRECTORIES`, `HOME` and `LC_ALL=C` (one `isolateGit` helper per package).
- Bug fixes and agent tasks start with a failing test; show it fails without the change.

## Code rules

- Subprocesses only through `internal/gitx` (later `internal/forge`): `ctx` first,
  `exec.CommandContext`, argv only (never `sh -c`), `WaitDelay` set, stderr in the error,
  clean git env (repo-locating vars dropped, `LC_ALL=C`, `GIT_TERMINAL_PROMPT=0`).
  Never classify a failure by matching a localized message.
- `ctx context.Context` is the first parameter of anything that runs a subprocess or blocks.
- Never print or store credentials: redact URLs (`gitx.Redact`) before output, errors or notes.
- Anything loom writes in another format (gitconfig, YAML, Markdown) is validated or escaped
  for that format and tested with the real tool reading it.
- No exported mutable package-level vars; no `init` side effects.
- Interfaces only at the consumer and only for 2+ real implementations (forge, agent harness).

## Constraints

- LOC budget: ~4.5k non-test lines in total (`mise run loc`, enforced in CI). If a change
  needs more, discuss scope first; do not raise the budget silently.
- Package boundaries ([ADR 0016](docs/adr/0016-functional-core-adapters-at-the-edges.md),
  enforced by `architecture_test.go`): command packages (`gitcmd`, `memstore`, `views`,
  `hooks`, `knowledge`) never import each other, except `views` → `memstore` and `hooks` →
  `memstore`/`views` (these two edges are not in the test yet; added with slice 1b); shared code lives in `contexts`, `forge`, `gitx`. Only
  `cmd/loom` imports `internal/cli`; `cmd/loom` is wiring only.
- Functional core: `contexts`, `memstore`, `views` never import `os/exec`, `gitx` or `forge`
  (non-test code); `cli` injects what they need from git or the forge.
- Dependencies: stdlib, `github.com/BurntSushi/toml`, `gopkg.in/yaml.v3`. No new dependency
  without an ADR in `docs/adr/` (MADR-lite: Status, Context, Decision, Consequences).
- No cgo in the binary (`test:race` enables it for the test build only). No secrets in code,
  tests or fixtures; credentials come from env only.
- Public repo: use example values (`github.com/your-user`, `gitlab.example.com`) in docs
  and fixtures, never real hosts, paths, organizations or context names.
- Design changes go through `docs/design.md` + an ADR before code.

## Guardrails

- Do not edit golden files, `architecture_test.go`, `.golangci.yml`, `scripts/loc.sh`, CI or
  budgets in the same commit as the code they check unless that is the task; say so in the PR.
- ADRs are append-only: change course with a new ADR that supersedes the old one (only the old
  status line changes). Each new ADR has a Confirmation section naming its enforcement
  (test, CI or none).
- Verify every new dependency, API and flag against its real docs before using it.
- One concern per PR; Conventional Commit titles.
