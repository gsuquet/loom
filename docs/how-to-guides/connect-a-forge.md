# Connect a forge

> Give `loomd` a token for GitHub or GitLab, for each context. Then check that the token can read and publish reviews.

Before you start, make sure that `loom` and `loomd` are installed and that your contexts are defined ([configure contexts](configure-contexts.md)).

`loomd` never stores the token. It gets the token from a command (`token_cmd`) or from a variable of its environment (`token_env`), and keeps it in memory only. The keys are in the [configuration reference](../reference/configuration.md#forges).

## Get a token

Use one of these sources.

### GitHub CLI

If you use `gh`, its token is enough: it has the `repo` scope.

1. Sign in, if necessary:

   ```sh
   gh auth login --hostname github.com
   ```

2. Find the absolute path of `gh`. `loomd` runs as a service with a minimal `PATH`, so it does not find `gh` by its name:

   ```sh
   command -v gh
   ```

   For example, `/opt/homebrew/bin/gh`.

3. Use this command as `token_cmd`: `/opt/homebrew/bin/gh auth token --hostname github.com`.

### A GitHub token

1. On GitHub, open **Settings**, **Developer settings**, **Personal access tokens**.
2. Create one of these tokens:
   - A classic token with the `repo` scope.
   - A fine-grained token with **Pull requests: Read and write** and **Contents: Read**. A fine-grained token has one resource owner. To review in two organizations, use a classic token or two contexts.
3. If the organization uses SAML single sign-on, select **Configure SSO** on the token, then **Authorize** for the organization.
4. Keep the token in a password manager, then read it with its command line. For 1Password:

   ```sh
   op read op://personal/github/token
   ```

### A GitLab token

1. On GitLab, open **Preferences**, **Access tokens**.
2. Create a personal access token with the `api` scope. With `read_api` only, `loomd` can read, but cannot publish reviews.
3. Keep the token in a password manager, then read it with its command line, as for GitHub.

## Configure the forge

1. Add a `[forges."<host>"]` table to `config.toml` for each forge host. Give its kind and the source of the token:

   ```toml
   [forges."github.com"]
   kind = "github"
   token_cmd = "/opt/homebrew/bin/gh auth token --hostname github.com"

   [forges."gitlab.company-a.example"]
   kind = "gitlab"
   token_cmd = "/opt/homebrew/bin/op read op://company-a/gitlab/token"
   ```

   Every context uses the token of the host table.

2. For a context that uses another account on the same host, add a `[contexts.<name>.forges."<host>"]` table. It replaces the token of the host table for this context:

   ```toml
   [contexts.company-a.forges."github.com"]
   token_cmd = "/opt/homebrew/bin/op read op://company-a/github/token"
   ```

3. Restart `loomd` to read the configuration:

   ```sh
   loom service restart
   ```

A command that waits for an unlock or a prompt cannot run in `loomd`: `loomd` stops it after 10 s. Unlock the password manager first, or use a token source that needs no prompt, such as a 1Password service account.

## Check the credentials

1. Run the doctor:

   ```sh
   loom repo doctor
   ```

2. Read the `forge <context> <host>` lines:
   - `ok`: the token can read and publish reviews.
   - `warn` `cannot publish`: the token can only read. Create a token with the scope that the fix gives.
   - `warn` `scopes unknown`: the forge does not give the scopes of this token (a GitHub fine-grained token, the OAuth token of `glab`). Make sure that the token has the permissions above.
   - `fail`: the fix tells what to do. For example, when `loomd` does not find the command, the fix gives its absolute path.
