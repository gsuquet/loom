# Configure contexts

> Give every repository on the machine a context (personal, a client…), check the assignments, and fix the repositories left `unassigned`.

Before you start: `loom` and `loomd` are installed. The configuration is `<loom home>/config.toml`: `~/.loom/config.toml`, or under `LOOM_HOME` when it is set.

## Define the contexts

1. Add one `[contexts.<name>]` table per context to `config.toml`, with the directories (`match.paths`) and the remote hosts (`match.hosts`) of its repositories:

   ```toml
   [contexts.personal]
   match.paths = ["~/perso"]
   match.hosts = ["github.com/gsuquet"]

   [contexts.company-a]
   match.paths = ["~/work/company-a"]
   match.hosts = ["gitlab.com/company-a", "gitlab.company-a.example"]
   ```

   A repository gets the first rule that matches: a `[repos."<path>"] context` override, then the `match.hosts` entry of its primary remote (the longest prefix wins), then the longest `match.paths` prefix, then `default_context`. Nothing else: it is `unassigned`.

2. Optional: give every repository that matches no rule a context:

   ```toml
   default_context = "personal"
   ```

3. Restart loomd: it reads the configuration only when it starts, then scans and assigns every repository again.

   - Service on macOS:

     ```bash
     launchctl kickstart -k gui/$(id -u)/io.github.gsuquet.loomd
     ```

   - Service on Linux:

     ```bash
     systemctl --user restart loomd.service
     ```

   - Without the service, stop loomd (its PID is in `run/loomd.lock`), then start it:

     ```bash
     kill "$(cat ~/.loom/run/loomd.lock)"
     loom status
     ```

   An invalid configuration stops loomd: `loom status` then prints the error and the end of `log/loomd.log`.

## Check the assignments

1. List the repositories, sorted by context:

   ```bash
   loom repo ls
   ```

   A `!` in the first column marks a repository without context.

2. See the rule that assigned a repository:

   ```bash
   loom repo show company-a/api
   ```

   The `context:` line names the context and the rule, such as `company-a (match.hosts "gitlab.com/company-a")`.

3. List the repositories of one context, or those without one:

   ```bash
   loom repo ls -c company-a
   loom repo ls -c unassigned --format paths
   ```

## Fix unassigned repositories

1. Run the checks:

   ```bash
   loom repo doctor
   ```

   The `unassigned` line gives their count, one of them, and a `[contexts.<name>]` table that would match it.

2. Pick the rule that fits, in `config.toml`:

   - All the repositories of a directory: add the directory to the `match.paths` of a context.
   - All the repositories of a forge group: add `host/group` to its `match.hosts`.
   - One repository: an override.

     ```toml
     [repos."~/src/tool"]
     context = "personal"
     ```

   - Everything else: `default_context`.

3. Restart loomd (see above), then run `loom repo doctor` again. The `contexts` line fails when `match.paths` entries of two contexts are the same directory once `~` is expanded (`~/work` and `/home/me/work`): keep each directory in one context.

## Reference

- Keys and their rules: [configuration](../reference/configuration.md#contexts-and-repositories).
- Output of `loom repo ls`, `show` and `doctor`: [CLI conventions](../reference/cli/conventions.md#loom-repo).
- Commands and flags: [loom CLI reference](../reference/cli/loom.md).
