# Agent orchestration

> Delegation, typed contracts, autonomy, gates, runtimes, the factory, context quality and external data access.

### Entities

| Entity | Contents |
| --- | --- |
| **AgentTemplate** | `id@version`, role, applicable kinds, prompt, allowed tools (built-in + loom MCP), input schema, output schema, gates, budget (USD, turns, time), **required runtime capabilities** |
| **Specialist** | The template of a role, as plain files in the store, editable in any editor: reviewer, implementer, investigator, researcher, knowledge distiller, meeting distiller, ticket creation, grilling, refining, pruning. Each has its own skills, model and provider. All are new, not reused from existing Claude Code skills |
| **Pipeline** | Stages defined in a text file (e.g. implement -> review -> fix, capped). Stage = template + gates; typed output of a stage feeds the next. Stages are runs on the same task, not child tasks. No visual editor |
| **DelegationRequest** | task, template@version, `requested_by: operator \| rule:<id>` |
| **Run** | request, stage, runtime + model, runner, context pack, worktrees, lease (`machine_id` + expiry), status, frozen skills (source and commit), stream, gate results, outputs, cost |
| **ContextPack** | Exactly what the agent receives, computed from the template input schema, persisted in the run |

Schemas are generated from Rust types (`schemars`): one source for CLI `--json`, MCP tools and agent contracts.

### Rules

- **Delegation is triggered by me.** Automatic starts are opt-in per rule, emitting the same `DelegationRequest` with `requested_by: rule:<id>`. A rule starts an agent; its result stays a proposal.
- **Two ways to run:** in the background, asking questions through loom, or as a prepared session in my terminal, which writes its result to a file that loom picks up on close. I mark that one done. Both run in a clean environment. There is no embedded terminal.
- **Worktrees:** loom creates them per task, one per repo, and cleans up.
- **Accounts are per context.** A wrong account for a context is blocked, not only warned about.
- **Budgets:** warn, or warn and stop. Usage billing and seats are both supported, and spend is visible.
- **Loops have a hard cap.** A loop that reaches it stops and asks me.
- **Choosing context:** loom picks a default from the task, its parents, the context and related knowledge. The specialist has a fixed set. I can add, edit and remove.
- **Reviewer isolation:** a reviewer receives only the diff and the spec, which overrides the pack layers below. Loom enforces this when it builds the pack.
- **Evidence:** an implementer attaches a throwaway journey through the running system to its proposal. At most three curated journeys per project are kept.
- **Silence:** a run with no activity for a set time warns me.
- **Interrupted is not Failed:** infrastructure death is not a bad result. The run keeps its worktrees, with resume or take over offered, and `loomd` reconciles on restart. Nothing restarts automatically.
- **Agents hold no credentials.** External effects (push, draft MR, comment, ticket reply) are **proposed actions** in the typed output, executed by loom after gates, according to the autonomy policy below.
- **Minimal context, enforced** for background runs, not for prepared terminal sessions (Claude Code adapter): `claude -p` launched with `--bare`, `--system-prompt-file`, `--tools`/`--disallowedTools`, `--strict-mcp-config --mcp-config <loom only>`, `--json-schema`, `--max-budget-usd`, `--max-turns`, `--permission-prompts none`, `--output-format stream-json`. All lookups beyond the worktree go through loom MCP, logged and scoped to the run's context.
- **Runtime**: `AgentRuntime` port in Rust, see [Runtimes and providers](#runtimes-and-providers). No second language.

### Autonomy policy

Everything from an agent is a proposal I approve. Acting without me may come later, per specialist version and per context, once autonomy is proven and only after my go-ahead. Metrics alone never grant it: they are evidence for my decision, and they can revoke it. This overrides the automatic grant in [ADR-0007](../adr/0007-autonomy-policy.md).

| Class | Examples | Approval now | Possible later, after my go-ahead |
| --- | --- | --- | --- |
| Internal, reversible | Push to a `loom/*` branch, open a draft MR, create child tasks | Me | Automatic |
| Visible to the team | Mark MR ready, comment on an MR, Jira status transition | Me | Configurable per context |
| Visible to a client or requester | Ticket reply, public Jira comment | Me | Always me |

The evidence per specialist version is its gate pass rate, my rework rate and the number of messages I send after launch.

### Gates

1. Pre-run: brief valid against input schema, budget available, worktree clean.
2. Output contract: JSON validated against output schema; N retries with the validation error.
3. Deterministic: build, tests, lint, types (commands declared per repo), diff size, allowed/forbidden paths, secret scan.
4. Adversarial review by another template (probabilistic, complements 3).
5. Approval of proposed actions according to the autonomy policy.

Each template version is measured (gate pass rate, my rework rate, messages after launch, cost per run). Versions can be compared on the same tasks: this is the continuous improvement loop for agents.

### Runtimes and providers

Claude Code comes first, on both accounts, and Antigravity later, behind the runtime port. Configurable per context. Two families behind the same `AgentRuntime` port:

| Family | Examples | Used for |
| --- | --- | --- |
| **Harness** | Claude Code, Codex CLI, Antigravity, … (headless) | Stages that edit files and run commands: implement, fix, preflight |
| **Direct API** | Anthropic, OpenAI, Gemini, and the client's cloud tenant (Bedrock, Vertex, Azure) | Stages whose only tools are loom MCP tools: triage, plan, review of a provided diff, summaries, learnings extraction |

- Loom's own agent loop (direct API) never gets file edit or shell tools. Coding goes through a harness. That keeps loom from becoming a harness itself.
- **Capabilities**: each adapter declares what it can enforce (tool restriction, MCP-only, structured output, budget cap, streaming, mid-run messages). A template declares what it requires. Loom refuses to run a template on a runtime that cannot enforce its requirements.
- **Loom-side enforcement comes first**, whatever the runtime: output validated against the schema by loom, budget and time enforced by killing the process, environment scrubbed of credentials, file and network isolation at OS or container level. Harness flags are an extra layer.
- **Role -> runtime + model binding per context** (e.g. reviewer -> Claude Opus via Claude Code, triage -> a small model via direct API).
- **Allowed runtimes per context**: this answers "which account runs agents on client code". Agent runs are blocked in a context with no allowed runtime.
- Cost: reported by the harness when available, otherwise computed from token usage and a price table in config (never hardcoded).
- Codex CLI and Antigravity capabilities are to be checked when their adapters are written.

```toml
[contexts.company-a.agents]
allowed_runtimes = ["claude-code", "bedrock"]

[contexts.company-a.agents.runtimes.claude-code]
family = "harness"
binary = "claude"
token_cmd = "op read op://…"

[contexts.company-a.agents.runtimes.bedrock]
family = "api"
provider = "bedrock"
region = "eu-west-3"
credentials_cmd = "…"

[contexts.company-a.agents.roles]
reviewer = { runtime = "claude-code", model = "claude-opus-5-5" }
triage = { runtime = "bedrock", model = "…" }
```

### Factory

Pipelines started by rules. Results stay proposals until I give the go-ahead described in [Autonomy policy](#autonomy-policy).

- **Intake rules**: a schedule plus a guard script, one firing per run, skipping overlaps. A new rule starts in a test mode. They emit `DelegationRequest`s with `requested_by: rule:<id>`.
- **Scheduler**: parallel run cap per runner and per context, budget per day and per context, pickup by priority, kill switch per context, circuit breaker per template (failure rate spike stops its runs), a task failing N times returns to the inbox with the diagnosis.
- **Runners**: any loomd can be a runner for the contexts it is allowed to serve.
  - Local runner (my machines): worktrees, capped parallelism, power assertion to prevent sleep while runs are active, pauses cleanly otherwise.
  - Central runner (VM / container), a later option with no date: same binary, for long runs and night shifts (scheduling windows per runner, e.g. 22:00–07:00), one container per run.
  - Placement per run: `local | central | any`, with rules (expected duration, night window). Leases prevent double execution.
  - Allowed runners per context: a client may forbid hosting its code or credentials on a personal VM.
- **Template evaluation**: archived tasks with their accepted outcome form a replay set. A new template version is replayed before promotion. An agent may propose template versions, evaluated the same way. The ledger becomes the eval dataset.

### Context quality

Minimal context only works if it is the right context. Garbage in, guesses out.

**1. Readiness gate (definition of ready per kind).** A task is delegable only when the template's input schema can be filled:

| Kind | Required before delegation |
| --- | --- |
| Story | Acceptance criteria, repo, target branch |
| Bug | Repro steps, expected vs actual, environment |
| Review | MR, head SHA, linked issue or stated intent |
| Investigation | Signal (alert or report), time window, service |
| Research | Question, decision it feeds, time box |

Missing fields block delegation. A cheap **brief critic** (direct API) lists ambiguities and questions; I answer in the app, or loom proposes a comment to the ticket author.

**2. Layered context pack with a token budget per layer.**

| Layer | Content | Mode |
| --- | --- | --- |
| Brief | Typed task input (from the template schema) | Always inlined |
| Repo card | Languages, build/test/lint commands, structure, conventions, `AGENTS.md`/`CLAUDE.md` of the repo | Inlined, cached per repo SHA by the repo index |
| Linked artifacts | Issue mirror + comments, MR diff, alert snapshot | Inlined if under budget, else spilled to file + summary |
| History | Similar archived tasks (same files touched, same component, same monitor, then FTS), their findings and decisions; knowledge pages and the decisions of the parent conception | Summaries inlined, details by reference |
| Pointers | IDs and paths the agent can fetch through loom MCP | References only |

Structural links (same files, same labels, same monitor) are ranked above text search: they are more precise. Embeddings only if measurement shows FTS + structure is not enough.

**3. Freshness.** The pack is built at run start from a fresh pull; versions are recorded (head SHA, issue `updated_at`). An upstream change during the run is flagged in the run.

**4. Asking is a valid outcome.** Every output schema has `assumptions[]` and `missing_context[]`, and the status `needs_clarification { questions }`. Questions land in the inbox; answers are appended to the brief and the run resumes. A guess presented as a fact counts as a failure in the metrics, a question does not.

**5. Pipeline handoff.** The typed output of a stage is the brief of the next one (the plan stage builds the implement stage's context). Transcripts are never passed between stages.

**6. Measured.** Per run: pack composition and size, loom MCP lookups (a lookup = something missing from the pack), declared assumptions, gate results, my rework. Lookups repeated across runs become pack rules. Missing knowledge that is repeatedly looked up in the same repo becomes a Chore: document it in the repo's `AGENTS.md` (context debt).

**7. Visible.** The panel showing what an agent receives opens the first time for each specialist, and can be skipped afterwards. I can add, edit and remove what it holds. Text in a pack or prompt must name the plausible mistake it prevents ([Keeping knowledge useful](knowledge.md#keeping-knowledge-useful)). Every run keeps the exact pack it received.

### External data for agents

Example: Datadog logs, metrics, monitors, dashboards.

- **Transport: MCP.** Loom MCP server is the only MCP server an agent sees. It keeps credentials inside loomd, gives typed parameters, logs every call as an event, and needs no Bash access. Claude Code loads MCP tool definitions on demand, so the upfront schema cost stays small.
- **Credentials**: per context, a fine-grained Datadog PAT with read scopes only. Datadog enforces read-only; loom's allowlist per template is the second layer. The run's context selects the credential: an agent in context A cannot reach context B's Datadog.
- **Loom-owned tools, not upstream pass-through**: a handful of task-shaped tools (`logs_search`, `logs_patterns`, `metric_summary`, `monitor_status`, `dashboard_summary`, `trace_get`) implemented on the direct Datadog API client loom already needs for alert ingestion. Loom controls the result shape end to end.
- **Token efficiency comes from result shaping**:
  - aggregate first: log patterns with counts, metric summaries (min/max/avg/p95, anomalies, change points), raw rows only on request;
  - field projection (`fields`) and a default `limit` on every tool;
  - tabular results as TSV with a header row instead of JSON (no repeated keys);
  - spill to file: a result above the per-call budget is written to `runs/<id>/data/<call>.tsv`; the agent receives a summary, the row count and the path, and reads slices with Read/Grep (run data dir passed with `--add-dir`).
- Agent tools come in this order: Datadog, then read-only Kubernetes. Implementers run `terraform plan` and deployments only through CI on their MR.
- Tokens per call are recorded; formats and tool designs are compared with the template metrics.
- Rust only: `rmcp` (official MCP Rust SDK).
- Proxying an upstream MCP server (Datadog, Sentry, GitLab, Jira) stays possible for long-tail tools, with results passed through the same shaping and spill rules.
