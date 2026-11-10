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
2. **Memory** — the agents' working memory: one Obsidian vault per confidentiality boundary
   (`memory-<ctx>`), one Markdown note per item (tasks, specs, projects, repos, reviews, facts,
   decisions…), written deterministically by scripts, queried by tags, with generated, budgeted
   views for agents and Bases dashboards for the human. Fast, situational.

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
  before writing notes.
- Strict context isolation: notes of a context are only ever written to that context's memory vault;
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
memory       = "~/kb/memory-context1"      # this context's memory vault
git_include  = "~/.config/git/context1.gitconfig"
secrets      = "pass-cli"

[contexts.context2]
match.paths  = ["~/context2"]
match.hosts  = ["gitlab.example.com"]
memory       = "~/kb/memory-context2"
git_include  = "~/.config/git/context2.gitconfig"
secrets      = "pass-cli"

[contexts.context3]
match.paths  = ["~/context3"]
match.hosts  = ["gitlab.com/example-group"]
memory       = "~/kb/memory-context3"
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
  The blocks cover symlinked `match.paths` (both spellings), URLs with user and/or port, and
  any letter case (`[xX]` globs), so git and loom agree. One case cannot be expressed: git's
  `hasconfig:remote.*.url` sees remote URLs as written, before `url.<base>.insteadOf`
  rewriting, while loom matches the rewritten URL. When a context was picked only through
  such a rewrite, `loom context` keeps the match but warns (stderr and `warning` in `--json`)
  that git will not apply its `git_include`; the generated file says the same. The routing
  test runs real git over the same fixture as the resolution test.

> The `kb` key and `[kb.<name>]` tables implemented in slice 1a are replaced by `memory` and
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
  directly. They write **drafts** into their memory vault (`drafts/`); `loom accept <draft>` moves a draft
  the human has rewritten into `notes/` or `sources/`.
- Holds no isolated-context data. Memory vaults may reference knowledge notes; knowledge never
  references memory.
- Sync: git (`loom sync`), no Obsidian Sync.

## 4. Memory (the agents' store)

One private git repo per confidentiality boundary (`memory-<ctx>`), opened as an Obsidian vault, written
mostly by agents, pushed directly to `main` ([ADR 0010](adr/0010-memory-vaults-one-note-per-item.md)).

```
tasks/ specs/ projects/ repos/ reviews/ research/ investigations/ support/ questions/ drafts/   living notes
facts/ decisions/ prefs/ procedures/                    claims: never edited, replaced via `supersedes`
entities/ signals/                                      records
attachments/                                            binaries, large verbatim files
views/hot.md  views/repo/<repo>.md  views/project/<project>.md   generated for agents, budgeted
bases/*.base                                            dashboards for the human (task board, review queue, health)
_meta/  tags.md kinds.md templates/ new.sh set.sh q.sh check.sh build-views.sh
.local/                                                 gitignored sensitive scratch
```

- **One note per item**, file stem = id (`project-<slug>`, `repo-<slug>`, `entity-<slug>`, otherwise
  `<YYYY-MM-DD>-<slug>`), links `[[stem]]`. Each new item is a new file, so concurrent agents rarely conflict.
- **Deterministic writing**: notes are created with `new.sh` (templates per kind) and their properties changed with
  `set.sh`; agents edit only the bodies of living notes. `check.sh` enforces names, a property allowlist, status
  vocabularies, tags and links. Per-kind schemas start minimal and are refined through the improvement loop.
- **Properties** for single-valued fields (`created`, `by`, `status`, `happened`, `confidence`, `ttl`, `supersedes`,
  `sources`, `depends_on`, `attachments`); **tags** for every query facet: `type/<kind>`, `project/`, `repo/`,
  `area/`, `tool/`, `domain/`, `lang/`, `kind/`. Nested tags match by prefix in Obsidian search and Bases.
- **Kinds kept from earlier agent tooling**: task (lifecycle `todo → planned → active → challenge → fix → preflight →
  pr → done`, the challenge by a separate agent), spec (requirements → tasks with `depends_on`), project and repo hubs,
  peer review of others' PRs/MRs (Conventional Comments, draft by default), research, investigation (hypotheses →
  root cause → postmortem), support (diagnosis → reply draft), question, draft (for the knowledge vault).
- **Claims**: `fact` (default ttl 90 days unless used), `decision`, `pref` (never expire, only superseded), `procedure`.

### Reading: tiers and budgets

| tier | content | when | budget |
|---|---|---|---|
| L0 | `views/hot.md` | injected at session start (hook) | ≤100 lines |
| L1 | `views/repo/<repo>.md` + its `views/project/<project>.md`, and the task note when the branch maps to a task | injected at session start | ≤240 lines + task |
| L2 | `q.sh` / `loom mem query` by kind, tag, status; Obsidian CLI (`search`, `tags`, `backlinks`, `base:query`) when the app runs | when planning | on demand |
| L3 | full notes, attachments, knowledge notes | when the task needs them | on demand |

Start context is loaded once per session and never re-injected mid-session. Queries apply the semantics (superseded
claims and expired facts hidden) and return one compact line per note. The Obsidian app is never required: the
scripts (and later loom) read the files directly.

### Views

`build-views.sh` (later `loom mem build`) compiles `views/` from the notes: superseded and expired items dropped,
ranked by tag match, confidence, use and recency, cut to budgets. Views are caches: always rebuildable.

## 5. Agent integration

Hooks of each supported coding agent call `loom agent hook <event>`:

| event | loom does |
|---|---|
| session start | resolve context, repo and task (from the branch) → inject L0 + L1 |
| plan approved | write the plan into the task note (`Plan` section), status `planned` |
| PR/MR opened or merged | fill the task's `Outcome` section from git and `gh`/`glab`, set status |
| session end | run extraction (§6), then `loom mem build`, `loom sync` |

**Skills** carry only the judgment work; mechanics live in loom
([ADR 0015](adr/0015-skills-for-judgment-loom-for-mechanics.md)). One `SKILL.md` each, agent-agnostic,
≤~150 lines, kept in dotfiles:

| skill | role |
|---|---|
| `recall` | before planning: queries + knowledge search → short brief with cited notes and gaps |
| `record` | end of task: outcome, new facts, procedures, decisions, prefs/corrections → notes |
| `task` | create a task with a brief, run its lifecycle (challenge by a separate agent, preflight, draft PR), close it |
| `decide` | capture a decision; supersede earlier ones |
| `research` | investigations of options: sources, findings, dated vs durable facts, drafts |
| `investigate` | incidents, bugs, support requests: hypotheses, evidence, root cause, postmortem or reply |
| `pr-review` | peer review of others' PRs/MRs: Conventional Comments, draft-by-default posting |
| `interview` | explore first, then numbered question rounds with recommended answers → memory notes and knowledge drafts |
| `distill` | sources/research/interviews → Zettelkasten drafts in the human's voice |
| `memory-review` | weekly with the human: drafts, questions, proposals, stale facts → accept/reject |

## 6. Continuous improvement loop

([ADR 0014](adr/0014-continuous-improvement-loop.md)), informed by self-improving agent
harnesses, with the two gaps they leave closed: outcomes are measured, and anything that
changes behaviour needs human approval.

| step | when | what | AI |
|---|---|---|---|
| capture | session end | read the session transcript transiently, redact secrets, write candidate `fact`, `pref`, `decision` notes and the task outcome; the transcript is never stored | optional |
| consolidate | nightly | dedupe, apply replacements, turn contradictions into `question`s, expire by age and non-use, rebuild views | no |
| learn | weekly | the same gotcha or fix in ≥3 outcomes, or the same correction twice → draft a `procedure` or a skill patch, filed as a `question` | yes |
| approve | weekly review | the human accepts or rejects; facts flow in automatically, **procedures, skills and structure never change without approval** | — |
| apply | on approval | procedures become skills through a dotfiles PR; structural changes through a loom or repo PR | no |
| measure | always | `signal` notes per skill version, procedure and fact: corrections, reverts, CI failures after agent changes, review comments on agent PRs, use counts; regressions are flagged for rollback | no |

**The structure is in scope too.** The loop may propose changes to the event schema, view
layouts and budgets, expiry rules, skills, hooks and loom itself, backed by `signal` evidence
(e.g. a view section never used, a budget always exceeded, a recurring correction). Such
proposals are `question` notes tagged `kind/proposal` and land as PRs; the note conventions
(`_meta/kinds.md`) are versioned and loom ships migrations (`loom mem migrate`).

## 7. Command surface

| command | slice | purpose |
|---|---|---|
| `loom context [gitconfig]` | 1a | resolve / generate |
| `loom git inventory [-c ctx]` | 1a | list repos per context (cache) |
| `loom git pr` / `loom git review` | 1a | replace the `git-pr` / `git-review` scripts |
| `loom mem new <kind> …` / `loom mem set` | 1b | create a note / change a property (replaces `new.sh` / `set.sh`) |
| `loom mem query` | 1b | filter notes by kind/tag/status/date (`--json`), semantics applied |
| `loom mem build` | 1b | compile views |
| `loom mem lint` | 1b | conventions, tags, links, budgets, stale facts, no secrets, isolation |
| `loom accept <draft>` | 1b | move a human-rewritten draft into knowledge |
| `loom sync` | 1b | pull --rebase / commit / push for the knowledge and memory vaults |
| `loom agent hook <event>` | 2 | hook entry point (§5) |
| `loom mem extract` | 2 | session-end extraction (optional AI) |
| `loom mem consolidate` | 2 | nightly consolidation |
| `loom mem signals` / `loom mem learn` | 3 | measurement, weekly learning |
| `loom mem migrate` | as needed | schema migrations |
| `loom daily` | 4 | activity digest as `outcome`/`signal` summaries into the hot view |

## 8. Architecture

```
cmd/loom/main.go            # wiring only
internal/cli/               # subcommand definitions
internal/contexts/          # contexts.toml load + resolve
internal/forge/             # gh / glab adapters
internal/gitx/              # git exec helpers
internal/gitcmd/            # inventory, pr, review
internal/memstore/          # notes: frontmatter, kinds, tags, links, query
internal/views/             # view compilation and budgets
internal/hooks/             # agent hook handlers, extraction driver
internal/knowledge/         # read-only access, accept, lint of links into knowledge
```

- Command packages (`gitcmd`, `memstore`, `views`, `hooks`, `knowledge`) never import each
  other except `views` → `memstore` and `hooks` → `memstore`/`views`; shared code lives in
  `contexts`, `forge`, `gitx`. Enforced by `architecture_test.go` (plus the dependency
  allowlist).
- Dependencies: stdlib `flag`, `BurntSushi/toml`, `gopkg.in/yaml.v3` (note properties). Validation is in-tree
  (allowlist, enums, tags, links), not a schema engine.

## 9. Delivery

| slice | content | done when |
|---|---|---|
| 1a | skeleton, contexts, `loom git *`, design + ADRs | parity with the legacy scripts (golden tests) |
| 1b | memory store (`mem new/set/query/build/lint`), `accept`, `sync`; vault scripts replaced; skills switch to loom commands | a week of daily agent use |
| 2 | hooks, session-end extraction, consolidation | start context injected in every session |
| 3 | signals, weekly learning, proposals | first approved procedure → skill |
| 4 | daily digest, capture (phone → knowledge inbox), integrations | per slice design |

Testing: table tests and golden files; git commands against temporary repos; forge calls behind
an interface with recorded fixtures; note parsing and view compilation with fixture vaults; no
network in tests.

## Decisions

- D1 Two stores: human knowledge, agent context. ([ADR 0001](adr/0001-two-stores-knowledge-and-memory.md))
- D2 One knowledge repo (no isolated-context data); one memory vault per confidentiality
  boundary (`memory-<ctx>`), agents push to `main`. ([ADR 0002](adr/0002-one-knowledge-repo-memory-vaults-per-boundary.md))
- D3 Knowledge is Zettelkasten-style in Obsidian (app + CLI, core plugins + Bases, git sync).
  ([ADR 0003](adr/0003-knowledge-zettelkasten-in-obsidian.md))
- D4 Memory is an Obsidian vault with one note per item, written deterministically, queried by
  tags, with generated, budgeted views; claims are superseded, never edited; facts expire by age and
  non-use. ([ADR 0010](adr/0010-memory-vaults-one-note-per-item.md))
- D5 Start context ≈ 3k tokens (L0 + L1), injected once per session.
- D6 Continuous improvement loop with measurement and human approval for behaviour and
  structure changes. ([ADR 0014](adr/0014-continuous-improvement-loop.md))
- D7 Ten skills for judgment; loom for mechanics. ([ADR 0015](adr/0015-skills-for-judgment-loom-for-mechanics.md))
- Earlier decisions kept: forges via `gh`/`glab` ([ADR 0007](adr/0007-forges-via-gh-glab.md)),
  stdlib-first Go ([ADR 0004](adr/0004-go-single-binary-stdlib-first.md)), contexts.toml
  ([ADR 0006](adr/0006-contexts-toml-single-source.md)), `loom git` changes G1–G10
  ([ADR 0012](adr/0012-loom-git-behavior-changes.md)), MIT and public repo, sync timer owned by
  the dotfiles ([ADR 0013](adr/0013-dotfiles-own-the-sync-timer.md)).
