# Repositories

> The repo index: discovery, contexts, state, search and automatic updates (v0.2). Decisions in [ADR-0012](../adr/0012-repo-index.md).

Loom knows every git repository on the machine, which context it belongs to, and whether it is up to date. It replaces the inventory scripts of the dotfiles (`git-inventory`, the `repos` mise task, `loom repo` of the former Python CLI).

## Discovery

- **Roots**: `[repos] roots` in `config.toml`, default `["~/perso", "~/work"]` plus the `match.paths` of every context. Roots that do not exist are skipped.
- **Walk**: depth `[repos] max_depth` (default 4) below each root, symbolic links not followed, no git process started.
- **A repository** is a directory with a `.git` directory or a `.git` file (`gitdir:` pointer: worktrees, submodules checked out as separate repos). The walk does not descend into a repository: nested repositories are found only through their own root or by registration.
- **Excluded**: hidden directories, `node_modules`, `vendor`, `target`, `dist`, `build`, `.venv`, `venv`, `__pycache__`, plus `[repos] exclude` (glob patterns: without `/`, matched against directory names; with `/`, against the path relative to the root).
- **Registration on use**: a repo outside the roots is added when a `loom` command runs inside it, or when the shell hook (`loom shell-init`) sees the shell enter it. `loom repo register <path>` does it explicitly; `loom repo forget` removes it.
- **Removal**: a scan removes the repos that no longer exist on disk. A scan of some roots never removes repos found under other roots. A registered repo is read in place by every scan and kept while its `.git` exists, even deeper than `max_depth`, excluded, or outside the roots; `loom repo forget` removes it.

## Identity

- **Path**: absolute, as found (symbolic links kept). The path is the key; one checkout = one repo.
- **Name**: directory name. **Slug**: `<context>/<name>`, an identifier scripts and agents may store (`loom repo show <slug>`), so it is stable. A repo keeps its slug, persisted in the index, while its context and name are unchanged; a new repo never renumbers another. A new repo, or one whose context or name changed, gets `<context>/<name>` when free, otherwise the lowest free `<context>/<name>-N` (N ≥ 2). A removed repo frees its slug. `loom repo ls` shows the suffix.
- **Remotes**: every `[remote "x"] url` of the repo's config (followed through `gitdir:` and `commondir` for worktrees). The **primary remote** is `origin`, otherwise the first one.
- **Forge**: GitHub or GitLab, from the primary remote's host, SSH host aliases resolved with `ssh -G <alias>` (cached per alias), plus `[forges]` overrides for self-hosted hosts.

## Contexts

Contexts are defined in `config.toml` (domain model: [Context](domain-model.md#context)). v0.2 uses only their matching rules; their stores arrive in v0.3.

```toml
default_context = "personal"

[contexts.personal]
match.paths = ["~/perso"]
match.hosts = ["github.com/gsuquet"]

[contexts.company-a]
match.paths = ["~/work/company-a"]
match.hosts = ["gitlab.com/company-a", "gitlab.company-a.example"]
update = "fetch"
```

A repo's context, first match wins:

1. Override in `[repos."<path>"] context`.
2. `match.hosts`: the primary remote's host and path prefix (after SSH alias resolution).
3. `match.paths`: the longest matching path prefix.
4. `default_context`, when set.
5. `unassigned`: listed and reported by `loom repo ls`, never guessed from a directory name.

Prefixes match whole segments: `gitlab.com/company-a` matches `gitlab.com/company-a/x/y`, not `gitlab.com/company-ab`; `~/work` matches `~/work/x`, not `~/workshop`. The longest `match.hosts` prefix wins as well (path segments compared without ASCII case). The same entry in two contexts is a configuration error, since neither would be longer. Context names follow the rule of `-c`; `unassigned` is reserved.

The same rules are the only source of truth for every tool: scripts ask `loom repo show --json` instead of reimplementing them.

## State (repo card)

Read with `gix`, no git process (ADR-0008), refreshed after a scan, after a fetch, on registration, periodically, and when `loom repo show` asks for it. The status walk, the untracked count and the ahead/behind counts share a time budget per repository; a field not computed in budget is reported unknown, never a failure of the whole state. Filter drivers (git-lfs…) are never run: a changed file that needs one to be compared makes `dirty` unknown, which blocks a fast-forward.

| Field | Content |
| --- | --- |
| `head` | Branch name (unborn or not), or short SHA when detached |
| `dirty` | Number of modified, staged or conflicted tracked paths (untracked files reported separately) |
| `untracked` | Count of untracked files, each file counted, capped at 1,000 |
| `operation` | `rebase`, `merge`, `cherry-pick`, `revert`, `bisect`, `am` in progress, or none (`am` blocks a fast-forward like the others) |
| `upstream` | Upstream of `head`, ahead and behind counts; none when its ref is missing |
| `default_branch` | `refs/remotes/<primary>/HEAD`, otherwise `main`, otherwise `master` (local or on the primary remote) |
| `last_commit` | Date of `HEAD`'s commit |
| `last_fetch` | Modification time of `FETCH_HEAD` (the latest of a linked worktree's and the main one's) |
| `last_update` | Result of the last automatic update (see below) |

Editable metadata lives in `config.toml` (`[repos."<path>"]`: `context`, `update`); repo card files in the context stores come with v0.3.

## Index and search

- SQLite at `~/.loom/index.db`, machine-local and rebuildable (`loom repo scan --rebuild`), written only by `loomd` (single writer). The CLI reads through the API.
- `loom search <query>` searches repos in v0.2 (name, slug, path, remotes, context, branch) with SQLite FTS5 and a ranking that favours exact name matches, then recently used repos. Tasks and notes join the same command in v0.3. Budget: under 100 ms end to end on 1,000 repos.
- `loom repo ls [-c <context>] [--json | --format paths]`: the list for scripts and pickers (`--format paths` feeds `fzf`).

## Automatic updates

`loomd` keeps repos up to date without ever risking local work.

- **Policy** per context and per repo (`update = "ff" | "fetch" | "off"`), repo override first, then context, default `ff`. An `unassigned` repo defaults to `fetch`; an explicit repo override wins (ADR-0012 clarification).
- **Fetch**: `git fetch --no-prune` of the primary remote (tags by git's automatic tag following, never clobbering a local tag), on a schedule (`[repos] fetch_interval`, default 1 h, jittered ±10 %), at most `[repos] fetch_concurrency` (default 4) at a time, each with a timeout (fetch 120 s, fast-forward 10 min so that a checkout is never interrupted; the process group is stopped on timeout). The state is read again after the fetch, right before deciding.
- **Fast-forward**, only when every condition holds (pure decision, exhaustively tested):
  - policy `ff`;
  - no operation in progress, no conflicted file, no tracked change (staged or not);
  - `head` is a branch with an upstream, `head` is not ahead of it, and the upstream is ahead;
  - then `git merge --ff-only <upstream>`, which refuses on its own if an untracked file would be overwritten.
  - The local default branch, when it is not checked out in any worktree and is strictly behind its upstream, is moved with `git fetch <remote> <branch>:<branch>` (refuses any non fast-forward).
  - No other branch is touched; nothing is ever rebased, reset, stashed or deleted.
- **Non-interactive git**: `GIT_TERMINAL_PROMPT=0`; when the user has no `core.sshCommand` / `GIT_SSH_COMMAND`, `GIT_SSH_COMMAND="ssh -o BatchMode=yes"`; the user's git config, credential helpers and hooks are honoured (ADR-0008), so `post-merge` hooks run.
- **SSH from a service**: on macOS the launchd user agent inherits `SSH_AUTH_SOCK` from the user session (system `ssh-agent`, keychain passphrases). On Linux the systemd user unit must see the agent socket; `loom repo doctor` checks it and prints the fix.
- **Failures**: a failing host (VPN down, auth) backs off per host (doubling, up to the fetch interval × 8) without blocking other hosts; a failure while another fetch of the same host succeeds backs off only its repo. Each repo keeps its `last_update` (time, policy, outcomes: `fetched`, `fast_forwarded`, `default_branch_moved`, `skipped` with a reason, `failed` with the step and git's stderr tail), stored apart from the state.
- **Manual**: `loom repo sync [<repo>…] [-n]` runs the same logic now (`POST /v1/repos/sync`), ignoring the schedule and the backoff; `-n` prints the decisions from the current state (`would fetch, then decide` when they depend on the fetch).

## Out of scope for v0.2

Repo card files in context stores, events in the ledger (v0.3), MR refs and forge metadata (v0.4), `loom go` (v0.3), the tmux sessionizer itself (stays in the dotfiles, fed by `loom repo ls --format paths`).
