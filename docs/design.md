# loom — design

- Status: Accepted (2026-11-09)
- Decisions: see [Decisions](#decisions-2026-11-09) and [`docs/adr/`](adr/)

## 1. Purpose

One small CLI that:

1. manages a plain-markdown knowledge base (KB) on a bronze/silver/gold model,
2. records the work of AI agents into it (plans, what was done, changes, artifacts),
3. builds daily/weekly notes from real activity (git, forges, tickets, calendars),
4. hosts the git workflow commands (`loom git inventory|pr|review`) that replace a set of
   existing shell/Python scripts.

### Non-goals

- No daemon, no database of record, no GUI/TUI/web UI, no plugin system.
- No session/prompt/transcript tracking of agents.
- No proprietary formats: the KB must stay fully usable with `rg`, an editor and git alone.
- Not a sync engine beyond `git pull --rebase` / `push`.

### Constraints

- Go, single static binary, installed with mise. Works offline; AI features off by default.
- Size budget: ~4.5k LOC non-test (KB ~3k, git ~1.5k). `mise run loc` reports LOC per
  package and CI fails above the budget.
- macOS + Linux. No cgo (keeps cross-compilation trivial).
- Secrets only via environment variables, fetched by the caller (a password-manager CLI per
  context). loom never stores tokens.
- Strict context isolation: data from an isolated context is never written to the commons KB
  except via `loom promote`.

## 2. Contexts

The single definition of "who am I working for here". Read by every command; also used to
generate git `includeIf` blocks.

`~/.config/loom/contexts.toml` (overridable with `$LOOM_CONTEXTS` or `--config`; honours
`$XDG_CONFIG_HOME`). Typically kept in dotfiles and templated per machine. Example (all
values are examples):

```toml
[contexts.context1]
match.paths  = ["~/context1"]                 # cwd prefix (longest match wins)
match.hosts  = ["github.com/your-user"]    # remote URL prefix (host[/owner])
kb           = "commons"
git_include  = "~/.config/git/context1.gitconfig"
secrets      = "pass-cli"

[contexts.context2]
match.paths  = ["~/context2"]
match.hosts  = ["gitlab.example.com"]
kb           = "commons"
git_include  = "~/.config/git/context2.gitconfig"
secrets      = "pass-cli"

[contexts.context3]
match.paths  = ["~/context3"]
match.hosts  = ["gitlab.com/example-group"]
kb           = "vault"
git_include  = "~/.config/git/context3.gitconfig"
secrets      = "bw"
kube_context = "example-cluster"           # consumed by mise per-dir setup, not loom

[kb.commons]
path = "~/kb/commons"
[kb.vault]
path = "~/context3/knowledge-vault"

[git]
protected_branches = ["main", "master", "trunk", "production"]   # default if omitted
```

- The `match.*` schema is reused from the archived Rust loom (`~/.loom/config.toml`).
- Validation is strict: unknown keys, relative paths, hosts with a scheme or user, duplicate
  matchers across contexts and `kb` names without a `[kb.<name>]` table are errors.
- Forge kind is derived from the host (`github.com` → `gh`, anything else → `glab`).
- Resolution order: `--context/-c` flag → `$LOOM_CONTEXT` → cwd path match (longest prefix,
  `~` expanded, symlinks resolved) → git remote host match (insteadOf applied, longest
  prefix; remotes matching different contexts are an error) → error. There is no default.
- `loom context [--json]` prints the resolved context and why it was chosen.
- `loom context gitconfig` prints the `includeIf` blocks (`hasconfig:remote.*.url:` per host,
  `gitdir:` per path), ordered so git's last-include-wins mirrors the resolution order. A
  dotfiles mise task writes them into the global git config: git identity routing is
  generated from the same single source.

## 3. Knowledge base

Two git repos with identical layout: **commons** (private; context1 and context2) and **vault**
(separate private repo; isolated contexts such as `context3`).

```
AGENTS.md            # rules for agents writing into the KB
CLAUDE.md            # contains only: @AGENTS.md
bronze/
  inbox/             # captures not yet triaged (phone, CLI)
  clips/YYYY/        # web clips (ingested)
  papers/            # pdf + extracted md
  meetings/YYYY/     # transcripts, meeting notes
  daily/YYYY/        # YYYY-MM-DD.md, YYYY-Www.md
  agents/YYYY/<date>-<repo>-<branch>/   # plan.md report.md changes.md artifacts/
  files/             # binary attachments
silver/              # flat atomic notes, typed by frontmatter `type:`
gold/{articles,talks,adrs,briefs}/<slug>/<slug>.md (+ assets) → dist/ (gitignored)
_meta/{templates/, views/, schema.md}
```

### Note schema (enforced by `loom lint`)

| field | where | values |
|---|---|---|
| `type` | all | bronze: `capture clip paper meeting daily weekly agent-plan agent-report agent-changes`; silver: `concept howto decision person project reference`; gold: `article talk adr brief` |
| `created` | all | ISO date |
| `context` | all | context name |
| `tags` | all | list |
| `source` | bronze | URL / origin |
| `status` | bronze | `new triaged done` |
| `sources` | silver, gold | list of `[[wikilinks]]` |
| `author` | agent-written | `agent/<name>` |

Links: `[[wikilinks]]` only, no `![[embeds]]`, no plugin syntax. Bronze is append-only (lint
warns on edits of `status: done` notes).

## 4. Command surface

| command | slice | purpose |
|---|---|---|
| `loom context [gitconfig]` | 1a | resolve / generate |
| `loom git inventory [-c ctx]` | 1a | list repos per context (cache) |
| `loom git pr` / `loom git review` | 1a | replace the `git-pr` / `git-review` scripts (git aliases `git pr`, `git mr`, `git review`) |
| `loom init` | 1b | scaffold a KB repo (layout, AGENTS.md, templates, schema) |
| `loom capture [text\|-]` | 1b | write to `bronze/inbox` |
| `loom triage` | 1b | fzf over inbox: open / promote to silver / done / delete |
| `loom new <type> <title>` | 1b | silver/gold note from template |
| `loom promote <note>` | 1b | vault → commons copy with confirmation + lint |
| `loom lint` | 1b | schema, broken links, context leaks |
| `loom search <q>` | 1b | `rg` wrapper; optional FTS index later |
| `loom sync` | 1b | pull --rebase / commit / push; stop on conflict; 15-min timer owned by dotfiles |
| `loom ingest` | 2 | clean extraction of inbox URLs/PDFs into clips/papers |
| `loom daily` / `loom weekly` | 3 | activity notes + local timesheet draft |
| `loom render <gold>` | 1b/5 | pandoc+typst (pdf/html), marp (slides), ADF (Jira) |
| `loom agent hook <event>` | 4 | called by coding-agent hooks |
| `loom enrich` | later | optional AI, off by default |

## 5. Architecture

```
cmd/loom/main.go            # wiring only
internal/cli/               # subcommand definitions (one file per command group)
internal/contexts/          # contexts.toml load + resolve
internal/forge/             # GitHub (gh) / GitLab (glab) adapters, shared by git + daily
internal/gitx/              # git exec helpers
internal/kb/                # layout, notes, frontmatter, links, lint
internal/gitcmd/            # inventory, pr, review
internal/render/            # pandoc/marp/ADF
internal/agentlog/          # hook handlers (slice 4)
```

- Rule: command packages (`gitcmd`, `kb`, `render`, `agentlog`) never import each other; they
  share only `contexts`, `forge`, `gitx`. Only `cmd/loom` imports `cli`. Enforced by
  `architecture_test.go`, which also enforces the third-party dependency allowlist.
- Forge access by shelling out to `gh` / `glab` (already authenticated, already per-host
  config) rather than embedding API clients: less code, no token handling.
- External tools are optional at runtime: a command needing `pandoc`/`fzf`/`glab` checks for
  it and fails with a clear message.
- Dependencies: stdlib `flag`-based subcommands (no cobra), `BurntSushi/toml`,
  `gopkg.in/yaml.v3`. Nothing else in slice 1.

## 6. Agent recording (slice 4, summary)

Unit = `bronze/agents/YYYY/<date>-<repo>-<branch>/` in the KB of the repo's context.

- `plan.md`: a hook copies the agent's plan file(s) when written/approved.
- `report.md`: written by the agent at the end, per the KB's AGENTS.md rules.
- `changes.md`: commits + PR/MR links, files touched, mutating non-git commands (name +
  target only).
- `artifacts/`: files under `$LOOM_ARTIFACTS` + discovered outputs, 20 MB cap.

Hooks from each supported coding agent call `loom agent hook <event>`. Guardrails get their
own design.

## 7. Capture (slice 2, summary)

Android PWA (`share_target`) → Cloudflare Pages behind Access → Worker (R2 snapshot,
markdown conversion, GitHub Contents API commit to `bronze/inbox`) → local `loom ingest`.
No GitHub Actions. Offline queue in the PWA. A dedicated architecture doc will follow.

## 8. Daily / weekly (slice 3, summary)

Generated on the first terminal of the morning / Friday from 15:00 (a shell hook calls
`loom daily --if-missing`). Sources: git log across inventory repos, gh/glab activity,
ticketing systems, calendars (ICS), meeting transcripts. One commons daily note; isolated-context
details live in the vault daily, linked. Timesheet drafts go to `~/.local/state/loom/`,
never committed.

## 9. Delivery

| slice | content | done when |
|---|---|---|
| 0 | archive the Rust loom as `loom-legacy`; create this repo | repos exist |
| 1a | skeleton, contexts, `loom git *`, design doc + ADRs, mise install, retire the replaced scripts | output parity with the scripts (golden tests) |
| 1b | KB core (`init capture triage new promote lint search sync render`), commons KB repo | daily use for a week |
| 2–6 | capture, daily + integrations, agent recording + guardrails, ticketing/meeting integrations, vault migration | per slice design |

Testing: table tests + golden files; `git` commands tested against temporary repos; forge
calls behind an interface with recorded fixtures; no network in tests.

## Decisions (2026-11-09)

- A1 Type vocabularies as in §3. ([ADR 0003](adr/0003-medallion-layout-and-note-schema.md))
- A2 Forges via `gh`/`glab` shell-out. ([ADR 0007](adr/0007-forges-via-gh-glab.md))
- A3 Stdlib `flag` + `BurntSushi/toml` + `yaml.v3`. ([ADR 0004](adr/0004-go-single-binary-stdlib-first.md))
- A4 Public repo, MIT license; CI on GitHub Actions from day one.
- A5 Inventory cache at `~/.cache/loom/repos/<ctx>.json`; agent instructions updated in the
  same change.
- A6 Contexts are user-defined (examples use `context1`..`context3`); no fallback context; an unmatched
  directory is an error. ([ADR 0006](adr/0006-contexts-toml-single-source.md))
- A7 The `loom sync` timer is owned by the dotfiles as `dot:loom:sync-timer`.
  ([ADR 0013](adr/0013-dotfiles-own-the-sync-timer.md))
- G1–G10 `loom git` behaviour changes vs the legacy scripts.
  ([ADR 0012](adr/0012-loom-git-behavior-changes.md))

Global dotfiles mise tasks are namespaced `dot:`; repository-local tasks (this repo's
`build`, `test`, `lint`, `fmt`, `loc`) are not.
