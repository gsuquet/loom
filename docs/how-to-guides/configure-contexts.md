# Configure contexts

> Give a context (personal, a client…) to each repository on the machine. Then check the assignments and fix the repositories that stay `unassigned`.

Before you start, make sure that `loom` and `loomd` are installed.

The configuration file is `<loom home>/config.toml`. This is `~/.loom/config.toml`, or the file under `LOOM_HOME` if `LOOM_HOME` is set.

## Define the contexts

1. For each context, add one `[contexts.<name>]` table to `config.toml`. In the table, give the directories (`match.paths`) and the remote hosts (`match.hosts`) of its repositories:

   ```toml
   [contexts.personal]
   match.paths = ["~/perso"]
   match.hosts = ["github.com/gsuquet"]

   [contexts.company-a]
   match.paths = ["~/work/company-a"]
   match.hosts = ["gitlab.com/company-a", "gitlab.company-a.example"]
   ```

   A repository gets the first rule that matches, in this order:

   1. A `[repos."<path>"] context` override.
   2. The `match.hosts` entry of its primary remote. The longest prefix wins.
   3. The longest `match.paths` prefix.
   4. `default_context`.

   If no rule matches, the repository is `unassigned`.

2. Optional: give a context to each repository that matches no rule:

   ```toml
   default_context = "personal"
   ```

3. Restart loomd. loomd reads the configuration only when it starts. Then it scans and assigns each repository again.

   ```bash
   loom service restart
   ```

   The command first checks `config.toml`. If the file is not valid, the command prints the error. loomd then continues to run with the previous configuration. If the service is installed, the service manager restarts loomd. Without the service, loomd is stopped and started again.

   A configuration that is not valid stops loomd. Then `loom status` prints the error and the end of `log/loomd.log`.

## Check the assignments

1. List the repositories, sorted by context:

   ```bash
   loom repo ls
   ```

   A `!` in the first column marks a repository without a context.

2. Show the rule that assigned a repository:

   ```bash
   loom repo show company-a/api
   ```

   The `context:` line gives the context and the rule, for example `company-a (match.hosts "gitlab.com/company-a")`.

3. List the repositories of one context, or the repositories without a context:

   ```bash
   loom repo ls -c company-a
   loom repo ls -c unassigned --format paths
   ```

## Fix unassigned repositories

1. Run the checks:

   ```bash
   loom repo doctor
   ```

   The `unassigned` line gives the number of unassigned repositories and one of them. It also gives a `[contexts.<name>]` table that matches this repository.

2. In `config.toml`, select the applicable rule:

   - All the repositories of a directory: add the directory to the `match.paths` of a context.
   - All the repositories of a forge group: add `host/group` to the `match.hosts` of a context.
   - One repository: add an override.

     ```toml
     [repos."~/src/tool"]
     context = "personal"
     ```

   - All other repositories: set `default_context`.

3. Restart loomd (see above).

4. Run `loom repo doctor` again.

   The `contexts` line fails if `match.paths` entries of two contexts are the same directory after `~` expansion. For example, `~/work` and `/home/me/work` are the same directory. Keep each directory in one context only.

## Reference

- Keys and their rules: [configuration](../reference/configuration.md#contexts-and-repositories).
- Output of `loom repo ls`, `show` and `doctor`: [CLI conventions](../reference/cli/conventions.md#loom-repo).
- Commands and flags: [loom CLI reference](../reference/cli/loom.md).
