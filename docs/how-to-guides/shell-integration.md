# Set up the shell integration

> Make loomd index every repository you `cd` into, including those outside the scan roots, without slowing the prompt. zsh and bash (3.2 or later).

Before you start: `loom` is on your `PATH`. loomd runs (`loom status`, or [as a service](run-loomd-as-a-service.md)): the shell never starts it, and while it is down nothing is registered.

## Set it up

1. Add one line to the end of the shell's startup file:

   - zsh, in `~/.zshrc`:

     ```zsh
     eval "$(loom shell-init zsh)"
     ```

   - bash, in `~/.bashrc`:

     ```bash
     eval "$(loom shell-init bash)"
     ```

   To keep the startup file working on machines without `loom`, guard the line:

   ```bash
   command -v loom >/dev/null && eval "$(loom shell-init bash)"
   ```

2. Open a new terminal, or reload the file in the current one (`source ~/.zshrc`, `source ~/.bashrc`).

From then on, the directory the shell starts in and each directory it enters run `loom repo register --on-use --background` in the background, once per directory per shell session. Inside a git repository, loomd registers it when its index does not have it, and records it as used. The hook prints nothing and costs about a millisecond per directory change.

## Check that it works

1. `cd` into a repository outside the scan roots.

2. List the index:

   ```bash
   loom repo ls
   ```

   The repository is listed. `loom repo show <name>` shows `registered: yes` and its `last_used`.

If it is missing, check that loomd runs (`loom status`) and that the startup file has the `eval` line (`type _loom_chpwd` in zsh, `type __loom_prompt` in bash).

## Turn it off

1. Remove the `eval "$(loom shell-init …)"` line from the startup file.

2. Open a new terminal. To stop it in a running shell instead:

   - zsh:

     ```zsh
     add-zsh-hook -d chpwd _loom_chpwd
     ```

   - bash:

     ```bash
     PROMPT_COMMAND=${PROMPT_COMMAND//__loom_prompt/:}
     ```

Repositories already registered stay in the index: `loom repo forget <repo>` removes one.

## Reference

- What the snippet does and how `--on-use --background` behaves: [CLI conventions](../reference/cli/conventions.md#loom-shell-init).
- Commands and flags: [loom CLI reference](../reference/cli/loom.md#loom-shell-init).
