//! Shell integration printed by `loom shell-init`: static scripts embedded in the binary, one
//! per shell, under `src/shell/`.

use crate::cli::Shell;

/// Command the hook runs, detached, for each directory the shell enters.
pub const HOOK_COMMAND: &str = "loom repo register --on-use --background";

/// The script to `eval` in the startup file of `shell`.
#[must_use]
pub const fn init_script(shell: Shell) -> &'static str {
    match shell {
        Shell::Zsh => include_str!("shell/init.zsh"),
        Shell::Bash => include_str!("shell/init.bash"),
    }
}

#[cfg(test)]
mod tests {
    use super::{HOOK_COMMAND, init_script};
    use crate::cli::Shell;

    #[test]
    fn every_script_is_guarded_and_runs_the_hook_detached_and_silent() {
        for shell in [Shell::Zsh, Shell::Bash] {
            let script = init_script(shell);
            // Guarded once: nothing is defined when `loom` is not on `PATH`.
            assert!(
                script.contains("if command -v loom >/dev/null 2>&1; then"),
                "{shell:?}"
            );
            let hook = script
                .lines()
                .find(|line| line.contains(HOOK_COMMAND))
                .unwrap_or_else(|| panic!("{shell:?}: no hook command"));
            assert!(hook.contains(&format!("command {HOOK_COMMAND} ")), "{hook}");
            assert!(
                hook.contains("</dev/null >/dev/null 2>&1"),
                "output discarded: {hook}"
            );
            // Never `git`: loom walks up to the repository itself.
            assert!(!script.contains("git "), "{shell:?}");
            // `return` at the top level of an `eval` in a startup file would end the file.
            assert!(
                script
                    .lines()
                    .filter(|line| line.trim_start().starts_with("return"))
                    .all(|line| line.starts_with("    ")),
                "{shell:?}: top-level return"
            );
            assert!(script.ends_with("fi\n"), "{shell:?}");
        }
    }

    #[test]
    fn zsh_uses_a_chpwd_hook_and_a_detached_job() {
        let script = init_script(Shell::Zsh);
        assert!(script.contains("add-zsh-hook chpwd _loom_chpwd"));
        assert!(script.contains("typeset -gA _loom_seen"));
        assert!(script.contains(" &! "));
    }

    #[test]
    fn bash_keeps_prompt_command_and_the_exit_status() {
        let script = init_script(Shell::Bash);
        assert!(script.starts_with("# shellcheck shell=bash\n"));
        assert!(script.contains("PROMPT_COMMAND+=$'\\n__loom_prompt'"));
        assert!(script.contains("*__loom_prompt*) ;;"), "installed once");
        assert!(script.contains("local rc=$?") && script.contains("return \"$rc\""));
        assert!(script.contains(" &) 2>/dev/null"), "detached in a subshell");
        // Bash 3.2 (macOS) has no associative array.
        assert!(!script.contains("declare -A"));
    }
}
