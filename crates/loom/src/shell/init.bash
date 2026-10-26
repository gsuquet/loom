# shellcheck shell=bash
# loom shell integration for bash. In ~/.bashrc: eval "$(loom shell-init bash)"
# Each directory the shell enters, once per session, is sent to loomd in the background:
# loomd registers the repository it is in. Nothing happens when loom is not installed.
# Bash 3.2 compatible (macOS /bin/bash): no associative array.
if command -v loom >/dev/null 2>&1; then
  loom() {
    local is_go=0
    local is_tui=1
    local skip_next=0
    local arg
    for arg in "$@"; do
      if [ "$skip_next" = 1 ]; then
        skip_next=0
        continue
      fi
      case "$arg" in
        -c|--context|--color)
          skip_next=1
          ;;
        -c*|--context=*|--color=*)
          ;;
        --)
          ;;
        -h|--help|-V|--version)
          is_tui=0
          break
          ;;
        -*)
          ;;
        go)
          is_go=1
          is_tui=0
          break
          ;;
        *)
          is_tui=0
          break
          ;;
      esac
    done
    if [ "$is_go" = 1 ] || [ "$is_tui" = 1 ]; then
      local cd_file
      cd_file=$(mktemp "${TMPDIR:-/tmp}/loom-cd.XXXXXX") || { command loom "$@"; return $?; }
      LOOM_CD_FILE="$cd_file" command loom "$@"
      local rc=$?
      if [ -s "$cd_file" ]; then
        local target
        target=$(cat "$cd_file")
        rm -f "$cd_file"
        if [ -d "$target" ]; then
          cd "$target" || return $?
        fi
      else
        rm -f "$cd_file"
      fi
      return "$rc"
    fi
    command loom "$@"
  }
  __loom_seen=$'\n'
  __loom_pwd=
  __loom_prompt() {
    local rc=$?
    if [[ "$PWD" != "$__loom_pwd" ]]; then
      __loom_pwd=$PWD
      case $__loom_seen in
        *$'\n'"$PWD"$'\n'*) ;;
        *)
          __loom_seen+="$PWD"$'\n'
          # Detached (subshell: no job, no notice); every error and output discarded.
          (command loom repo register --on-use --background </dev/null >/dev/null 2>&1 &) 2>/dev/null
          ;;
      esac
    fi
    # Keeps `$?` for the prompt commands that follow.
    return "$rc"
  }
  case ${PROMPT_COMMAND-} in
    *__loom_prompt*) ;;
    '') PROMPT_COMMAND=__loom_prompt ;;
    # Appended on a line of its own: an existing value ending with `;` stays valid.
    *) PROMPT_COMMAND+=$'\n__loom_prompt' ;;
  esac
fi
