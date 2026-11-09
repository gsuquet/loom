# loom — design

- Status: Proposed (2026-11-09), replaces the bronze/silver/gold knowledge-base design of the
  same day before any of it was implemented
- Decisions: see [Decisions](#decisions) and [`docs/adr/`](adr/)

## 1. Purpose

Humans and AI agents work at different speeds and need different things from notes. One day of
use showed it: agents produced dozens of records and notes in hours, and routing all of it
through human-curated notes meant either drowning in review or notes that were no longer the
human's. loom therefore manages **two stores**:

1. **Knowledge** — the human's own notes: Zettelkasten-style, curated, read and written in
   Obsidian. Slow, durable, generic (no isolated-context data).
2. **Context** — the agents' working memory: an append-only event log with generated views,
   optimised for retrieval, concurrent writes and token budgets, not for human reading. Fast,
   situational, one repo per confidentiality boundary.

loom is the deterministic machinery around them (hooks, view compilation, lint, sync, the
improvement loop), plus the git workflow commands (`loom git inventory|pr|review`).

### Non-goals

- No daemon, no database of record (indexes are disposable caches), no GUI/TUI/web UI.
- No storage of session transcripts. They are read transiently at session end for extraction
  (§6) and never written to a repo.
- No reimplementation of what Obsidian already does for the knowledge repo (graph, backlinks,
  views, search UI).
- Not a sync engine beyond `git pull --rebase` / `push`.

### Constraints

- Go, single static binary, installed with mise, macOS + Linux, no cgo.
- Everything in loom works offline and without AI. AI is used only by agent skills and by the
  optional extraction/learning steps of the improvement loop (§6), which degrade to "not run".
- Size budget: ~4.5k LOC non-test, enforced by `mise run loc` in CI.
- Secrets only via environment variables; loom never stores tokens. Extraction redacts secrets
  before writing events.
- Strict context isolation: events of a context are only ever written to that context's repo;
  knowledge never holds isolated-context data.
- The knowledge repo depends on the Obsidian app and CLI (closed source, free). This is a
  deliberate exception for the human UI ([ADR 0003](adr/0003-knowledge-zettelkasten-in-obsidian.md));
  the files stay plain markdown and agents fall back to `rg` when the app is not running.

## 2. Contexts

The single definition of "who am I working for here". Read by every command; also used to
generate git `includeIf` blocks.

`~/.config/loom/contexts.toml` (overridable with `$LOOM_CONTEXTS` or `--config`; honours
`$XDG_CONFIG_HOME`). Typically kept in dotfiles and templated per machine. Example (all
values are examples):

```toml
[contexts.context1]
match.paths  = ["~/context1"]              # cwd prefix (longest match wins)
match.hosts  = ["github.com/your-user"]    # remote URL prefix (host[/owner])
store        = "~/kb/context-context1"     # this context's context repo
git_include  = "~/.config/git/context1.gitconfig"
secrets      = "pass-cli"

[contexts.context2]
match.paths  = ["~/context2"]
match.hosts  = ["gitlab.example.com"]
store        = "~/kb/context-context2"
git_include  = "~/.config/git/context2.gitconfig"
secrets      = "pass-cli"

[contexts.context3]
match.paths  = ["~/context3"]
match.hosts  = ["gitlab.com/example-group"]
store        = "~/kb/context-context3"
git_include  = "~/.config/git/context3.gitconfig"
secrets      = "bw"
kube_context = "example-cluster"           # consumed by mise per-dir setup, not loom

[knowledge]
path = "~/kb/knowledge"                    # one per human, shared by all contexts (read-only for agents)

[git]
protected_branches = ["main", "master", "trunk", "production"]   # default if omitted
```

- Validation is strict: unknown keys, relative paths, hosts with a scheme or user, duplicate
  matchers across contexts are errors.
- Forge kind is derived from the host (`github.com` → `gh`, anything else → `glab`).
- Resolution order: `--context/-c` → `$LOOM_CONTEXT` → cwd path match (longest prefix, `~`
  expanded, symlinks resolved) → git remote host match (insteadOf applied; remotes matching
  different contexts are an error) → error. There is no default.
- `loom context [--json]` prints the resolved context and why; `loom context gitconfig` prints
  the `includeIf` blocks, ordered so git's last-include-wins mirrors the resolution order.

> The `kb` key and `[kb.<name>]` tables implemented in slice 1a are replaced by `store` and
> `[knowledge]` in slice 1b.

## 3. Knowledge (the human's store)

One private git repo, Zettelkasten-style, opened as an Obsidian vault
([ADR 0003](adr/0003-knowledge-zettelkasten-in-obsidian.md)).

```
inbox/        fleeting notes and captures (phone capture lands here)
sources/      literature notes: my notes on one source, in my words, with the reference
notes/        permanent notes: one idea, my words, flat
maps/         structure notes (maps of content), entry points to clusters
outputs/      articles, talks, briefs written for a reader
attachments/  binaries
_meta/        templates, tags.md, schema.md
```

- Plain markdown, YAML properties, `[[wikilinks]]`; Obsidian core plugins and Bases only, no
  community plugins. Every link in a permanent note sits in a sentence saying why.
- Agents read it (Obsidian CLI when the app runs, `rg` otherwise) and never write to it
  directly. They write **drafts** into the context repo; `loom accept <draft>` moves a draft
  the human has rewritten into `notes/` or `sources/`.
- Holds no isolated-context data. Context repos may link knowledge notes; knowledge never links
  context.
- Sync: git (`loom sync`), no Obsidian Sync.

## 4. Context (the agents' store)

One private git repo per confidentiality boundary (`context-<ctx>`), written mostly by agents,
pushed directly to `main` ([ADR 0010](adr/0010-context-event-log-and-views.md)).

```
schema.json                       event types (JSON Schema), validated on write; versioned
log/YYYY/MM/DD-<session>.jsonl    append-only, one file per agent session: no merge conflicts
blobs/<sha256>.<ext>              plans, reports, artifacts, drafts; referenced by events
views/                            generated, never edited; deleting them is safe
  hot.md                          L0: active projects, open decisions/questions, recent changes, drafts waiting
  repo/<repo>.md                  L1: facts, commands, gotchas, procedures, prefs for one code repo
  project/<project>.md            L1: goal, state, open decisions and questions
```

### Events

One JSON object per line, short keys. Common fields: `id` (ULID), `t` (type), `scope`
(`global`, `repo:<name>`, `project:<name>`), `s` (statement), `src` (event id, blob, URL or
`repo@sha`), `at` (date), `conf` (0–1), `ttl` (optional), `sup` (id of the event it replaces),
`by` (`human` or `agent/<name>`).

| type | meaning | expiry |
|---|---|---|
| `fact` | something observed about a repo, system or tool | default 90 days unless used |
| `pref` | a human preference or correction, with its reason | never; only replaced |
| `decision` | a choice with options and consequences | never; only replaced |
| `plan` | an approved plan (blob) | with its outcome |
| `outcome` | a finished unit of work: what was done, commits, PR/MR, files, mutating commands (name + target) | rolled up after 90 days |
| `question` | open, waiting on the human (also contradictions and proposals) | when answered |
| `entity` | a system, service or person | never |
| `link` | pointer to a knowledge note | never |
| `procedure` | a learned how-to (when, steps, pitfalls, verification); may become a skill | by use |
| `signal` | an observation about an outcome or artifact: correction, revert, CI failure, review comment, use | aggregated |

### Reading: tiers and budgets

| tier | content | when | budget |
|---|---|---|---|
| L0 | `views/hot.md` | injected at session start (hook) | ≤100 lines |
| L1 | `views/repo/<repo>.md` + its `views/project/<project>.md` | injected at session start | ≤240 lines |
| L2 | `loom ctx query` (decisions, plans, questions, recent outcomes for this scope) | when planning | on demand |
| L3 | blobs, full history, knowledge notes | when the task needs them | on demand |

Start context is loaded once per session and never re-injected mid-session (keeps the prompt
cache stable); later reads go through queries.

### Compilation

`loom ctx build` folds the log into views: applies `sup` replacements and expiry, deduplicates,
ranks by scope match, confidence, use count and recency, and cuts to each view's budget. Views
are caches: always rebuildable from the log.

## 5. Agent integration

Hooks of each supported coding agent call `loom agent hook <event>`:

| event | loom does |
|---|---|
| session start | resolve context and repo → inject L0 + L1 |
| plan approved | store the plan blob, append a `plan` event |
| PR/MR opened or merged | append/extend the `outcome` event from git and `gh`/`glab` |
| session end | run extraction (§6), then `loom ctx build`, `loom sync` |

**Skills** carry only the judgment work; mechanics live in loom
([ADR 0015](adr/0015-skills-for-judgment-loom-for-mechanics.md)). One `SKILL.md` each, agent-agnostic,
≤~150 lines, kept in dotfiles:

| skill | role |
|---|---|
| `recall` | before planning: L2/L3 queries + knowledge search → short brief with citations and gaps |
| `record` | end of task: outcome, new facts, procedures, decisions → events |
| `decide` | capture a decision; supersede earlier ones |
| `research` | investigations: sources, findings, dated vs durable facts, drafts |
| `interview` | build knowledge: question tree, numbered rounds with recommended answers → drafts |
| `distill` | sources/research/interviews → Zettelkasten drafts in the human's voice |
| `review` | weekly with the human: drafts, questions, proposals, stale facts → accept/reject |

## 6. Continuous improvement loop

([ADR 0014](adr/0014-continuous-improvement-loop.md)), informed by self-improving agent
harnesses, with the two gaps they leave closed: outcomes are measured, and anything that
changes behaviour needs human approval.

| step | when | what | AI |
|---|---|---|---|
| capture | session end | read the session transcript transiently, redact secrets, append candidate `fact`, `pref`, `decision`, `outcome` events; the transcript is never stored | optional |
| consolidate | nightly | dedupe, apply replacements, turn contradictions into `question`s, expire by age and non-use, rebuild views | no |
| learn | weekly | the same gotcha or fix in ≥3 outcomes, or the same correction twice → draft a `procedure` or a skill patch, filed as a `question` | yes |
| approve | weekly review | the human accepts or rejects; facts flow in automatically, **procedures, skills and structure never change without approval** | — |
| apply | on approval | procedures become skills through a dotfiles PR; structural changes through a loom or repo PR | no |
| measure | always | `signal` events per skill version, procedure and fact: corrections, reverts, CI failures after agent changes, review comments on agent PRs, use counts; regressions are flagged for rollback | no |

**The structure is in scope too.** The loop may propose changes to the event schema, view
layouts and budgets, expiry rules, skills, hooks and loom itself, backed by `signal` evidence
(e.g. a view section never used, a budget always exceeded, a recurring correction). Such
proposals are `question` events and land as PRs; `schema.json` is versioned and loom ships
migrations (`loom ctx migrate`).

## 7. Command surface

| command | slice | purpose |
|---|---|---|
| `loom context [gitconfig]` | 1a | resolve / generate |
| `loom git inventory [-c ctx]` | 1a | list repos per context (cache) |
| `loom git pr` / `loom git review` | 1a | replace the `git-pr` / `git-review` scripts |
| `loom ctx add <type> …` | 1b | validate and append an event |
| `loom ctx query` | 1b | filter events by scope/type/date (`--json`) |
| `loom ctx build` | 1b | compile views |
| `loom ctx lint` | 1b | schema, budgets, stale facts, links into knowledge, no secrets, isolation |
| `loom accept <draft>` | 1b | move a human-rewritten draft into knowledge |
| `loom sync` | 1b | pull --rebase / commit / push for knowledge and context repos |
| `loom agent hook <event>` | 2 | hook entry point (§5) |
| `loom ctx extract` | 2 | session-end extraction (optional AI) |
| `loom ctx consolidate` | 2 | nightly consolidation |
| `loom ctx signals` / `loom ctx learn` | 3 | measurement, weekly learning |
| `loom ctx migrate` | as needed | schema migrations |
| `loom daily` | 4 | activity digest as `outcome`/`signal` summaries into the hot view |

## 8. Architecture

```
cmd/loom/main.go            # wiring only
internal/cli/               # subcommand definitions
internal/contexts/          # contexts.toml load + resolve
internal/forge/             # gh / glab adapters
internal/gitx/              # git exec helpers
internal/gitcmd/            # inventory, pr, review
internal/ctxstore/          # event schema, log append/read, blobs, query
internal/views/             # view compilation and budgets
internal/hooks/             # agent hook handlers, extraction driver
internal/knowledge/         # read-only access, accept, lint of links into knowledge
```

- Command packages (`gitcmd`, `ctxstore`, `views`, `hooks`, `knowledge`) never import each
  other except `views` → `ctxstore` and `hooks` → `ctxstore`/`views`; shared code lives in
  `contexts`, `forge`, `gitx`. Enforced by `architecture_test.go` (plus the dependency
  allowlist).
- Dependencies: stdlib `flag`, `BurntSushi/toml`, `gopkg.in/yaml.v3` (knowledge properties); JSON via stdlib. Schema validation is a
  small in-tree subset (types, required keys, enums), not a full JSON Schema engine.

## 9. Delivery

| slice | content | done when |
|---|---|---|
| 1a | skeleton, contexts, `loom git *`, design + ADRs | parity with the legacy scripts (golden tests) |
| 1b | context store (`ctx add/query/build/lint`), `accept`, `sync`; knowledge and context repos created; skills written against plain files | a week of daily agent use |
| 2 | hooks, session-end extraction, consolidation | start context injected in every session |
| 3 | signals, weekly learning, proposals | first approved procedure → skill |
| 4 | daily digest, capture (phone → knowledge inbox), integrations | per slice design |

Testing: table tests and golden files; git commands against temporary repos; forge calls behind
an interface with recorded fixtures; event logs and view compilation with fixture logs; no
network in tests.

## Decisions

- D1 Two stores: human knowledge, agent context. ([ADR 0001](adr/0001-two-stores-knowledge-and-context.md))
- D2 One knowledge repo (no isolated-context data); one context repo per confidentiality
  boundary, agents push to `main`. ([ADR 0002](adr/0002-one-knowledge-repo-context-repos-per-boundary.md))
- D3 Knowledge is Zettelkasten-style in Obsidian (app + CLI, core plugins + Bases, git sync).
  ([ADR 0003](adr/0003-knowledge-zettelkasten-in-obsidian.md))
- D4 Context is an append-only JSONL event log with generated, budgeted views; one log file per
  session; facts expire by age and non-use, prefs and decisions never.
  ([ADR 0010](adr/0010-context-event-log-and-views.md))
- D5 Start context ≈ 3k tokens (L0 + L1), injected once per session.
- D6 Continuous improvement loop with measurement and human approval for behaviour and
  structure changes. ([ADR 0014](adr/0014-continuous-improvement-loop.md))
- D7 Seven skills for judgment; loom for mechanics. ([ADR 0015](adr/0015-skills-for-judgment-loom-for-mechanics.md))
- Earlier decisions kept: forges via `gh`/`glab` ([ADR 0007](adr/0007-forges-via-gh-glab.md)),
  stdlib-first Go ([ADR 0004](adr/0004-go-single-binary-stdlib-first.md)), contexts.toml
  ([ADR 0006](adr/0006-contexts-toml-single-source.md)), `loom git` changes G1–G10
  ([ADR 0012](adr/0012-loom-git-behavior-changes.md)), MIT and public repo, sync timer owned by
  the dotfiles ([ADR 0013](adr/0013-dotfiles-own-the-sync-timer.md)).
