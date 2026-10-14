# Back up context stores

> Push the task store of a context to a private git repository, check that the backup works, restore it on another machine, and fix a store that diverged.

Before you start: `loom` and `loomd` are installed, the context is defined in `config.toml` ([configure contexts](configure-contexts.md)), and git has your identity (`git config --global user.name` and `user.email`): loomd commits with it. Store commits are never signed, whatever your git configuration says.

The backup host holds your private notes. For a client context, use a host the client allows; one repository per client context, so that purging the context at the end of an engagement does not need a history rewrite.

## Set up the backup

1. Create an empty private repository on the backup host, such as `git@github.com:me/work-store.git`. Do not add a README: the remote must be empty or hold a store backed up earlier.

2. Check that loomd's git can reach it without a prompt: the SSH key is in the agent loomd sees, or a credential helper holds an HTTPS token.

   ```bash
   git ls-remote git@github.com:me/work-store.git
   ```

3. Add the remote to the context in `config.toml`:

   ```toml
   [contexts.work]
   store.backup.remote = "git@github.com:me/work-store.git"
   ```

   Each context needs its own repository.

4. Optional: change how often loomd syncs (default 15 minutes, at least 1 minute):

   ```toml
   [tasks]
   sync_interval = "30m"
   ```

5. Restart loomd: it reads the configuration only when it starts, then syncs every store with a remote.

   - Service on macOS:

     ```bash
     launchctl kickstart -k gui/$(id -u)/io.github.gsuquet.loomd
     ```

   - Service on Linux:

     ```bash
     systemctl --user restart loomd.service
     ```

6. Sync now and read the result:

   ```bash
   loom sync -c work
   ```

   ```text
   work: pushed 12 commits
   ```

## Check the backup

```bash
loom repo doctor
```

Each store held on the machine has a `backup <context>` line. `ok` means the last sync left the store and its remote equal. A `warn` or `fail` line comes with its fix.

## Restore a store on another machine

1. Install loom and copy `config.toml` (the same file on every machine).

2. Optional: name the machine. loomd names it after the host at its first start, in `~/.loom/machine.toml`; edit `id` to choose another name, then restart loomd:

   ```toml
   id = "tower"
   ```

3. Optional: keep a context off machines that must not hold it. In `config.toml`, list the machines that hold it:

   ```toml
   [contexts.work]
   machines = ["laptop"]
   ```

   On any other machine, loomd does not load that store, and `loom repo doctor` lists it under `machine`.

4. Start loomd with an empty store directory (`~/.loom/contexts/<name>/` or `store.path`), then sync:

   ```bash
   loom sync -c work
   ```

   ```text
   work: pulled 12 commits
   ```

Both machines can sync the same store, but loom does not yet handle edits made on both between two syncs: the store then diverges.

## Fix a diverged store

loom never merges a store. When the store and its remote both have commits the other lacks, `loom sync` and `loom repo doctor` report `diverged` and the sync of that store stops; the other stores are still synced.

1. Stop loomd, so that it does not write the store while you fix it (`launchctl bootout gui/$(id -u)/io.github.gsuquet.loomd` on macOS, `systemctl --user stop loomd.service` on Linux).

2. In the store directory, fetch the remote and look at both sides:

   ```bash
   cd ~/.loom/contexts/work
   git fetch backup
   git log --oneline --graph HEAD backup/main
   ```

3. Bring the remote's commits in by hand, for example with a merge. `events.jsonl` files merge by keeping both sides' lines (`merge=union` in `.gitattributes`); a `task.md` changed on both sides is a conflict you resolve.

   ```bash
   git merge backup/main
   ```

4. Start loomd again (the service starts it, or `loom status`), then sync:

   ```bash
   loom sync -c work
   ```

   ```text
   work: pushed 2 commits
   ```

## Fix a refused push

`push rejected, commits kept local` means the remote refused the commits: no write access, a protected `main` branch, or a server hook. The commits stay in the store. Fix the access on the backup host, then run `loom sync -c <context>` again.
