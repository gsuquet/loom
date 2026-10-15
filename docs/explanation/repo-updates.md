# How loomd keeps repositories up to date

> This page tells when `loomd` fetches each repository of the index, and when it fast-forwards a branch. It tells about the two guards that keep local work safe. It also tells what to do when `loomd` skips a repository. Keys: [configuration](../reference/configuration.md#contexts-and-repositories). API and result format: [`POST /v1/repos/sync`](../reference/api.md#updates), [`LastUpdate`](../reference/api.md#lastupdate). Decisions: [ADR-0012](../adr/0012-repo-index.md), [ADR-0008](../adr/0008-git-cli-for-writes.md).

## The problem

If nobody updates a repository for a week, it is a week behind. Then you must run `git pull` before every piece of work. You find merge conflicts late, and you compare branches with stale remote refs. A fetch is safe. But if a branch moves in a working tree where someone works, that work can be lost.

`loomd` fetches every repository. It moves a branch only when the move cannot affect local work. When it cannot be sure, it does nothing and tells why.

## The schedule

- After its start, `loomd` updates every repository of the index that was never updated. Then it updates each repository every `[repos] fetch_interval` (1 h by default) after its previous update. A repository updated before a restart waits for the rest of its interval.
- Each delay changes by up to ±10 % at random, so that repositories found together are not fetched together forever.
- At most `[repos] fetch_concurrency` updates (4 by default) run at the same time.
- A failed fetch (host unreachable, VPN down, authentication refused, timeout) delays the next update of its repository. The delay is 2, then 4, then 8 times the interval, never more.
- The host of the primary remote gets the same backoff, so its other repositories also wait. This occurs only when no fetch of that host succeeded in the meantime. If the host answers for other repositories, it is up, and the failure is specific to the repository. Repositories of other hosts are never delayed. The next successful fetch of the host clears its backoff.
- Sometimes another git process works in a repository (an automatic fetch of an editor, a command that you run). Then `loomd` skips the repository as `busy` and tries it again one interval later, without any backoff. The lock tells nothing about the remote.

`loom repo sync` runs the same update now, for the repositories that it names or for all of them. It ignores the schedule and the backoff.

## What an update does

1. **Policy.** `update = "ff"` (the default), `"fetch"` or `"off"`, from `[repos."<path>"]`, otherwise from the context of the repository. `off`: nothing runs. A repository without a context (`unassigned`) gets `fetch` by default, because no rule says that it is yours to move. `[repos."<path>"] update` still sets its policy, `ff` included.
2. **Fetch.** `git fetch --no-prune --no-prune-tags --no-tags <primary remote>`. No ref is ever deleted, even when the git configuration of the user asks to prune. No tag is fetched: automatic updates move only branches, and tags are the business of the user.

   If the update fetched tags, one tag that moved upstream would make the whole fetch fail, each time. As soon as tags are fetched, git refuses to overwrite a local tag that exists (`! [rejected] 1.0.0 -> 1.0.0 (would clobber existing tag)`). Two settings make git fetch tags. `fetch.pruneTags` adds `refs/tags/*:refs/tags/*` even with `--no-prune`, and only `--no-prune-tags` removes it. `remote.<name>.tagOpt = --tags` also fetches tags, and `--no-tags` overrides it. `git fetch --tags` brings them when you want them.

   A repository without a remote, or one that cannot be opened, is skipped, and git does not run.
3. **State.** The update reads the state again, after the fetch and right before it decides. The working tree possibly changed after the start of the update.
4. **Branch `HEAD` is on.** The update fast-forwards it with `git merge --ff-only --no-autostash <commit of its upstream>` only when every condition holds. The first condition that fails is the reason of the skip:
   - no operation in progress (rebase, merge, cherry-pick, revert, bisect, am).
   - no tracked change, staged or not, and no conflicted file.
   - `HEAD` is on a branch that has commits (not detached).
   - the branch has an upstream whose ref exists.
   - the branch has no commit that its upstream lacks (not ahead, not diverged).
   - the upstream has commits that the branch lacks.
   - every one of these is known. A field that the state could not read in its time budget means no move. A file that needs a filter driver (git-lfs…) for the comparison also means no move.

   When the upstream has nothing new, nothing has to move and nothing is reported as skipped.
5. **Default branch.** The local default branch comes from `refs/remotes/<primary>/HEAD`, otherwise it is `main`, otherwise `master`, if it exists locally or on the primary remote. The update moves it only when all these conditions hold:

   - no working tree of the repository has it checked out, not even a linked one.
   - no rebase or bisect is in progress on it.
   - it has an upstream on the primary remote.
   - it is strictly behind its upstream.

   The update moves it with `git fetch --no-prune --no-prune-tags --no-tags <primary> <its branch on the remote>:refs/heads/<branch>`. If it is ahead or diverged, it stays, and the update tells so. If a working tree has it checked out, the update of that working tree moves it, under the conditions of step 4.
6. **Result.** The update reads the state once more and stores it with the result of the update, its [`last_update`](../reference/api.md#lastupdate). The result is `fetched`, `fast_forwarded`, `default_branch_moved`, `skipped` with a reason, or `failed` with the step and the end of the git error output.

Nothing else ever occurs: no rebase, reset, stash, checkout, branch deletion or prune, no other branch moved, no conflict resolved.

## Two independent guards

The decision of loom is the first guard: it reads the state with `gix` and refuses when anything is doubtful. git is the second guard. `git merge --ff-only` refuses anything but a fast-forward, and refuses to overwrite an untracked file that the new commits would create. `git fetch <remote> <a>:<b>` refuses to move a branch other than forward, and refuses a branch checked out in any working tree. A branch moves only when the two guards agree, so a gap in one guard is caught by the other.

When git refuses, the update reports it as skipped (`git refused the fast-forward: …`), and does not retry before the next interval. The typical case is an untracked file with the same name as a file that the remote added.

## When another git process holds a lock

git protects a repository with lock files: `index.lock` while it writes the index, `<ref>.lock` while it moves a ref. An editor that fetches in the background, or a command that you run, holds them for a moment. If an update meets a lock, it fails in git (`fatal: Unable to create '…/index.lock': File exists`, `error: cannot lock ref 'refs/remotes/origin/…': is at … but expected …`). loomd reads the git error output and reports the step as skipped, `repository busy`, not as failed.

loomd never removes a lock. From outside, the lock of a process that runs and the lock of a process that crashed look the same. If loomd removed the first lock, the repository would become corrupt. The next update, one interval later, tries again. If a lock is still there then, it is probably stale, and `git status` in the repository tells what to do.

Only an error output made of lock messages counts. Any other output, even next to a lock message, stays a failure.

An automated suite in CI, on Linux and macOS, covers both guards end to end. It runs `loomd` on repositories in every risky state:

- changes tracked, staged, conflicted or untracked.
- ahead, diverged, detached, without upstream.
- rebase, merge, cherry-pick, bisect or `git am` in progress.
- the default branch checked out in a linked worktree.
- a dirty submodule.
- a filter driver.
- a `post-merge` hook that fails.
- a moved local tag.
- a tag moved upstream under `fetch.pruneTags` and `tagOpt = --tags`.
- an `index.lock` held by another process.
- a stash.
- policies `fetch` and `off`.

The suite fails on any change to refs, index, files, stash, operation or configuration, other than a fetch and the fast-forward of a clean branch.

## Hooks and configuration

The commands run through the `git` CLI, in the working tree of the repository. They use the git configuration, credential helpers and hooks of the user, exactly as a manual `git pull --ff-only` does. In particular, a `post-merge` hook runs after each automatic fast-forward. The time that a hook takes counts in the 10 min timeout of the fast-forward.

git never asks anything. `GIT_TERMINAL_PROMPT=0` disables password prompts. SSH runs with `BatchMode=yes` (no passphrase or host key question), unless the user configured an SSH command (`core.sshCommand`, `GIT_SSH_COMMAND` or `GIT_SSH`). git is pinned to the repository: `loomd` removes the variables that it inherits, such as `GIT_DIR`. git never looks above the working tree for another repository.

Each command has a timeout: 120 s for a fetch, 10 min for a fast-forward. On timeout, the whole process group of the command (git, ssh, hooks, credential helpers) receives `SIGTERM`, so that git can remove its lock files. 5 s later, the process group receives `SIGKILL`.

A fast-forward is local work of limited size. If its checkout stops before the end, the working tree is only half updated. Thus, its long timeout never interrupts it before its checkout completes, unless the whole command takes more than 10 min. In practice, only a `post-merge` hook that hangs takes this long. If this occurs, `git status` shows the state, and `git merge --ff-only @{upstream}` completes the job.

When `loomd` stops, it does not interrupt an update that runs. The git command of the update finishes on its own, and `loomd` does not record its result.

## SSH from a service

Under `loom service`, `loomd` does not start from a terminal. Thus, its fetches reach SSH remotes only through an SSH agent that it can see. On macOS, the launchd user agent inherits `SSH_AUTH_SOCK` from the user session (the system `ssh-agent`, with keychain passphrases). On Linux, the systemd user unit must see the agent socket. Without an agent, SSH fetches fail with `Permission denied (publickey)`, and their host gets a backoff. HTTPS remotes with a credential helper are not affected.

`loom repo doctor` checks each remote host as an update reaches it. loomd itself runs `git ls-remote` in one repository of the host, with its own environment and the git configuration of the user. A check from the shell would see another environment (another agent, another `PATH`) and would miss the git configuration. For example, `core.sshCommand` can give a key, possibly for some remotes only, through `includeIf "hasconfig:remote.*.url:…"`. This key applies to git, not to an `ssh -T` that you write by hand. The doctor also groups by reason the repositories whose last update failed or was skipped, with the fix for each reason.

## When a repository is skipped

`loom repo sync -n` shows what an update would do now. `last_update` in `GET /v1/repos/{context}/{name}` shows what the last update did. Each reason is a state of the working tree that only you can change:

| Reason | What to do |
| --- | --- |
| `N changed tracked paths` | Commit, stash or discard the changes. Then run `loom repo sync <repo>`. |
| `<operation> in progress` | Finish or abort it (`git rebase --continue` or `--abort`, and so on). |
| `HEAD detached` | Check out a branch. |
| `branch <b> has no upstream` | `git branch --set-upstream-to <remote>/<b>`, or leave it: a local branch is never moved. |
| `<b> diverged from its upstream` | Rebase or merge by hand. |
| `git refused the fast-forward: …` | Usually an untracked file that the remote now tracks. Move it to a different location. |
| `repository busy: another git process holds a lock (<step>)` | Nothing: the next update tries again. If it continues, make sure that no git process runs in the repository. A process that crashed leaves its lock file. |
| `<field> unknown` | The state could not be read in time, or a file needs a filter driver for the comparison. Check `error` in the state, and run `git status`. |
| `update policy is off` | Set `update` in `config.toml` if the repository should be updated. |
| `repository unreadable: …` | The repository cannot be opened: check its `.git`. |
