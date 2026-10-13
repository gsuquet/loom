# How loomd keeps repositories up to date

> When `loomd` fetches each repository of the index, when it fast-forwards a branch, the two guards that keep local work safe, and what to do when a repository is skipped. Keys: [configuration](../reference/configuration.md#contexts-and-repositories); API and result format: [`POST /v1/repos/sync`](../reference/api.md#updates), [`LastUpdate`](../reference/api.md#lastupdate); decisions: [ADR-0012](../adr/0012-repo-index.md), [ADR-0008](../adr/0008-git-cli-for-writes.md).

## The problem

A repository left alone for a week is a week behind: `git pull` before every piece of work, merge conflicts found late, branches compared against stale remote refs. Fetching is harmless, but moving a branch in a working tree where someone is working can lose that work. `loomd` fetches every repository, and moves a branch only when nothing local can be affected; whenever it cannot be sure, it does nothing and says why.

## The schedule

- After its start, `loomd` updates every repository of the index that was never updated, then each one every `[repos] fetch_interval` (1 h by default) after its previous update. A repository updated before a restart waits for the rest of its interval.
- Each delay moves by up to ±10 % at random, so that repositories found together are not fetched together forever.
- At most `[repos] fetch_concurrency` updates (4 by default) run at the same time.
- A failed fetch (host unreachable, VPN down, authentication refused, timeout) delays the next update of its repository: 2, then 4, then 8 times the interval, never more. The host of the primary remote backs off the same way, so its other repositories wait too, but only when no fetch of that host succeeded in the meantime: a host that answers for other repositories is up, and the failure is the repository's own. Repositories of other hosts are never delayed. The next successful fetch of the host clears its backoff.

`loom repo sync` runs the same update now, for the repositories it names or for all of them, regardless of the schedule and of the backoff.

## What an update does

1. **Policy.** `update = "ff"` (the default), `"fetch"` or `"off"`, from `[repos."<path>"]`, otherwise from the repository's context. `off`: nothing runs. A repository without a context (`unassigned`) gets `fetch` by default, since no rule says it is yours to move; `[repos."<path>"] update` still sets its policy, `ff` included.
2. **Fetch.** `git fetch --no-prune <primary remote>`. No ref is ever deleted, even when the user's configuration asks for pruning. Tags come with git's automatic tag following (and `remote.<name>.tagOpt`): a local tag that differs from the remote one is kept, and does not fail the fetch as an explicit `--tags` would. A repository without a remote, or one that cannot be opened, is skipped without running git.
3. **State.** The state is read again, after the fetch and right before deciding: the working tree may have changed since the update started.
4. **Branch `HEAD` is on.** It is fast-forwarded with `git merge --ff-only --no-autostash <commit of its upstream>` only when every condition holds; the first that fails is the reason of the skip:
   - no operation in progress (rebase, merge, cherry-pick, revert, bisect, am);
   - no tracked change, staged or not, and no conflicted file;
   - `HEAD` is on a branch that has commits (not detached);
   - the branch has an upstream whose ref exists;
   - the branch has no commit its upstream lacks (not ahead, not diverged);
   - the upstream has commits the branch lacks;
   - every one of these is known: a field the state could not read in its time budget, or a file that needs a filter driver (git-lfs…) to be compared, means no move.

   When the upstream has nothing new, nothing has to move and nothing is reported as skipped.
5. **Default branch.** When the local default branch (`refs/remotes/<primary>/HEAD`, otherwise `main`, otherwise `master`) is checked out in no working tree of the repository, not even a linked one, nor being rebased or bisected, has an upstream on the primary remote, and is strictly behind it, it is moved with `git fetch --no-prune <primary> <its branch on the remote>:refs/heads/<branch>`. Ahead or diverged, it stays, and the update says so. Checked out somewhere, it is left to the update of that working tree, under the conditions above.
6. **Result.** The state is read once more and stored with the result of the update, its [`last_update`](../reference/api.md#lastupdate): `fetched`, `fast_forwarded`, `default_branch_moved`, `skipped` with a reason, or `failed` with the step and the end of git's error output.

Nothing else ever happens: no rebase, reset, stash, checkout, branch deletion or pruning, no other branch moved, no conflict resolved.

## Two independent guards

loom's own decision is the first guard: it reads the state with `gix` and refuses on anything doubtful. git is the second: `git merge --ff-only` refuses anything but a fast-forward, and refuses to overwrite an untracked file the incoming commits would create; `git fetch <remote> <a>:<b>` refuses to move a branch other than forward, and refuses a branch checked out in any working tree. Both must agree for a branch to move, so a gap in one is caught by the other.

When git refuses, the update reports it as skipped (`git refused the fast-forward: …`), and does not retry before the next interval. The typical case is an untracked file with the same name as a file the remote added.

## Hooks and configuration

The commands run through the `git` CLI, in the repository's working tree, with the user's git configuration, credential helpers and hooks, exactly like a manual `git pull --ff-only`. In particular, a `post-merge` hook runs after each automatic fast-forward. A hook that takes long counts in the 10 min timeout of the fast-forward.

git never asks anything: `GIT_TERMINAL_PROMPT=0` turns off password prompts, and SSH runs with `BatchMode=yes` (no passphrase or host key question) unless the user configured an SSH command (`core.sshCommand`, `GIT_SSH_COMMAND` or `GIT_SSH`). git is pinned to the repository: variables such as `GIT_DIR` inherited by `loomd` are removed, and git never looks above the working tree for another repository.

Each command has a timeout: 120 s for a fetch, 10 min for a fast-forward. On timeout, the whole process group of the command (git, ssh, hooks, credential helpers) receives `SIGTERM`, which lets git remove its lock files, then `SIGKILL` 5 s later. A fast-forward is local, bounded work, and interrupting its checkout would leave a half-updated working tree: its long timeout means it is never interrupted before its checkout completes unless the whole command exceeds 10 min, which in practice only a hung `post-merge` hook does. If that happens, `git status` shows the state, and `git merge --ff-only @{upstream}` finishes the job.

When `loomd` stops, an update already running is not interrupted: its git command finishes on its own, and its result is not recorded.

## SSH from a service

Under `loom service`, `loomd` is not started from a terminal, so its fetches only reach SSH remotes through an SSH agent it can see. On macOS, the launchd user agent inherits `SSH_AUTH_SOCK` from the user session (the system `ssh-agent`, with keychain passphrases). On Linux, the systemd user unit must see the agent socket. Without one, SSH fetches fail with `Permission denied (publickey)` and their host backs off; HTTPS remotes with a credential helper are not affected.

## When a repository is skipped

`loom repo sync -n` shows what an update would do now, and `last_update` in `GET /v1/repos/{context}/{name}` what the last one did. Each reason is a state of the working tree that only you can change:

| Reason | What to do |
| --- | --- |
| `N changed tracked paths` | Commit, stash or discard the changes, then `loom repo sync <repo>`. |
| `<operation> in progress` | Finish or abort it (`git rebase --continue` or `--abort`, and so on). |
| `HEAD detached` | Check out a branch. |
| `branch <b> has no upstream` | `git branch --set-upstream-to <remote>/<b>`, or leave it: a local branch is never moved. |
| `<b> diverged from its upstream` | Rebase or merge by hand. |
| `git refused the fast-forward: …` | Usually an untracked file the remote now tracks: move it aside. |
| `<field> unknown` | The state could not be read in time, or a file needs a filter driver to be compared; check `error` in the state, and run `git status`. |
| `update policy is off` | Set `update` in `config.toml` if the repository should be updated. |
| `repository unreadable: …` | The repository cannot be opened: check its `.git`. |
