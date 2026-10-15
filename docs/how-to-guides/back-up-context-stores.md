# Make a backup of the context stores

> Push the task store of a context to a private git repository. Then check the backup, restore the store on a different machine, and fix a store that diverged.

Before you start, make sure that:

- `loom` and `loomd` are installed.
- The context is defined in `config.toml` ([configure contexts](configure-contexts.md)).
- git has your identity (`git config --global user.name` and `user.email`). loomd makes its commits with this identity.

loomd never signs the store commits, whatever your git configuration is.

The backup host keeps your private notes. For a client context, use a host that the client allows. Use one repository for each client context. Then, at the end of an engagement, you can purge the context without a history rewrite.

## Configure the backup

1. Create an empty private repository on the backup host, for example `git@github.com:me/work-store.git`. Do not add a README. The remote must be empty or contain a store from an earlier backup.

2. Make sure that the git of loomd can reach the repository without a prompt. For this, the SSH key is in the agent that loomd sees, or a credential helper holds an HTTPS token.

   ```bash
   git ls-remote git@github.com:me/work-store.git
   ```

3. Add the remote to the context in `config.toml`:

   ```toml
   [contexts.work]
   store.backup.remote = "git@github.com:me/work-store.git"
   ```

   Each context needs its own repository.

4. Optional: change how often loomd syncs (default 15 minutes, minimum 1 minute):

   ```toml
   [tasks]
   sync_interval = "30m"
   ```

5. Restart loomd. loomd reads the configuration only when it starts. Then it syncs each store that has a remote.

   - Service on macOS:

     ```bash
     launchctl kickstart -k gui/$(id -u)/io.github.gsuquet.loomd
     ```

   - Service on Linux:

     ```bash
     systemctl --user restart loomd.service
     ```

6. Sync the store now. Then read the result:

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

Each store on the machine has a `backup <context>` line. `ok` means that the store and its remote were equal when they were last synced. A `warn` or `fail` line also gives its fix.

## Restore a store on a different machine

1. Install loom.

2. Copy `config.toml` to the machine. Use the same file on each machine.

3. Optional: give a name to the machine. At its first start, loomd gives the machine the name of the host, in `~/.loom/machine.toml`. To use a different name, edit `id`:

   ```toml
   id = "tower"
   ```

   Then restart loomd.

4. Optional: keep a context only on the machines that can hold it. In `config.toml`, list these machines:

   ```toml
   [contexts.work]
   machines = ["laptop"]
   ```

   On all other machines, loomd does not load that store. `loom repo doctor` lists the store under `machine`.

5. Make sure that the store directory is empty (`~/.loom/contexts/<name>/` or `store.path`). Start loomd. Then sync the store:

   ```bash
   loom sync -c work
   ```

   ```text
   work: pulled 12 commits
   ```

The two machines can sync the same store. If you edit the store on the two machines and do not sync between the edits, the store diverges. loom does not handle this case yet.

## Fix a diverged store

loom never merges a store. The store diverges if the store and its remote each have commits that the other does not have. Then `loom sync` and `loom repo doctor` report `diverged`. The sync of that store stops. The sync of the other stores continues.

1. Stop loomd (`launchctl bootout gui/$(id -u)/io.github.gsuquet.loomd` on macOS, `systemctl --user stop loomd.service` on Linux). If loomd runs, it can write the store while you fix it.

2. In the store directory, fetch the remote. Then compare the two sides:

   ```bash
   cd ~/.loom/contexts/work
   git fetch backup
   git log --oneline --graph HEAD backup/main
   ```

3. Add the commits of the remote by hand, for example with a merge. git merges the `events.jsonl` files and keeps the lines of the two sides (`merge=union` in `.gitattributes`). If a `task.md` changed on the two sides, it is a conflict. Resolve it yourself.

   ```bash
   git merge backup/main
   ```

4. Start loomd again. The service starts it, or `loom status` starts it. Then sync the store:

   ```bash
   loom sync -c work
   ```

   ```text
   work: pushed 2 commits
   ```

## Fix a refused push

`push rejected, commits kept local` means that the remote refused the commits. The cause can be no write access, a protected `main` branch, or a server hook. The commits stay in the store.

1. Fix the access on the backup host.
2. Run `loom sync -c <context>` again.
