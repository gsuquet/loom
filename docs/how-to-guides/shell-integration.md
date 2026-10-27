# Configure the shell integration

> Make loomd register each repository that you `cd` into, also the repositories outside the scan roots. The prompt does not become slower. This guide applies to zsh and to bash (3.2 or later).

Before you start, make sure that:

- `loom` is on your `PATH`.
- loomd runs. To check, run `loom status`. You can also [run loomd as a service](run-loomd-as-a-service.md).

The shell never starts loomd. If loomd does not run, nothing is registered.

## Configure the integration

1. Add one line to the end of the shell's startup file:

   - zsh, in `~/.zshrc`:

     ```zsh
     eval "$(loom shell-init zsh)"
     ```

   - bash, in `~/.bashrc`:

     ```bash
     eval "$(loom shell-init bash)"
     ```

   If you use the same startup file on machines without `loom`, add a guard to the line:

   ```bash
   command -v loom >/dev/null && eval "$(loom shell-init bash)"
   ```

2. Open a new terminal, or reload the file in the current one (`source ~/.zshrc`, `source ~/.bashrc`).

After this step, the shell runs `loom repo register --on-use --background` in the background. It runs this command for the start directory and for each directory that it enters. It runs it one time for each directory in a shell session. If the directory is in a git repository that is not in the index, loomd registers the repository. In a git repository, loomd also records the repository as used. The hook prints nothing and adds about one millisecond to each directory change.

The integration also defines a `loom` shell function. When you run `loom go <id>`, or when you press `g` in the TUI, the shell changes directory to the repository linked to the task. If the task has no linked repository, the shell stays in the current directory.

## Check that it works

1. `cd` into a repository outside the scan roots.

2. List the index:

   ```bash
   loom repo ls
   ```

   The list shows the repository. `loom repo show <name>` shows `registered: yes` and its `last_used`.

If the repository is not in the list, do these checks:

- Make sure that loomd runs: `loom status`.
- Make sure that the startup file has the `eval` line: `type _loom_chpwd` in zsh, `type __loom_prompt` in bash.

## Add the context and the focus task to the prompt

`loom prompt` prints the context of the current directory and its focus task, for example `work k3 Fix the login timeout`. It does not need the `eval` line, but it needs loomd to show the focus task. It waits for loomd at most 100 ms, and it never prints an error.

1. Add the segment to the prompt, in the shell's startup file:

   - zsh, in `~/.zshrc`. Use single quotes, so that zsh runs the command at each prompt:

     ```zsh
     setopt PROMPT_SUBST
     PROMPT='$(loom prompt --shell zsh) %~ %# '
     ```

   - bash, in `~/.bashrc`. Use single quotes, so that bash runs the command at each prompt:

     ```bash
     PS1='$(loom prompt) \w \$ '
     ```

2. Open a new terminal, or reload the file in the current one.

3. To make the segment shorter, add `--max-title <n>`. For example, `--max-title 20` cuts the title after 20 characters. `--max-title 0` shows the context and the task prefix only.

If the segment is empty, do these checks:

- Run `loom prompt` in the same directory. If the output is empty, no rule gives a context: see [configure contexts](configure-contexts.md).
- In a git repository, loomd must have the repository in its index. Make sure that loomd runs (`loom status`) and that the `eval` line of the shell integration is in the startup file.
- If the segment has no task, the context has no focus task. To set one, run `loom go <id>`.

## Disable the integration

1. Remove the `eval "$(loom shell-init …)"` line from the startup file.

2. Open a new terminal. To stop the hook in a shell that is already open, run this command instead:

   - zsh:

     ```zsh
     add-zsh-hook -d chpwd _loom_chpwd
     ```

   - bash:

     ```bash
     PROMPT_COMMAND=${PROMPT_COMMAND//__loom_prompt/:}
     ```

The repositories that are already registered stay in the index. To remove one, run `loom repo forget <repo>`.

## Reference

- What the snippet does and how `--on-use --background` behaves: [CLI conventions](../reference/cli/conventions.md#loom-shell-init).
- Commands and flags: [loom CLI reference](../reference/cli/loom.md#loom-shell-init).
- What `loom prompt` prints and how long it waits: [CLI conventions](../reference/cli/conventions.md#loom-prompt).
