# loom shell integration for zsh. In ~/.zshrc: eval "$(loom shell-init zsh)"
# Each directory the shell enters, once per session, is sent to loomd in the background:
# loomd registers the repository it is in. Nothing happens when loom is not installed.
if command -v loom >/dev/null 2>&1; then
  loom() {
    local is_go=0
    local is_tui=1
    local skip_next=0
    local arg
    for arg in "$@"; do
      if [[ "$skip_next" == 1 ]]; then
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
    if [[ "$is_go" == 1 || "$is_tui" == 1 ]]; then
      local cd_file
      cd_file=$(mktemp "${TMPDIR:-/tmp}/loom-cd.XXXXXX") || { command loom "$@"; return $?; }
      LOOM_CD_FILE="$cd_file" command loom "$@"
      local rc=$?
      if [[ -s "$cd_file" ]]; then
        local target
        target=$(cat "$cd_file")
        rm -f "$cd_file"
        if [[ -d "$target" ]]; then
          cd "$target" || return $?
        fi
      else
        rm -f "$cd_file"
      fi
      return $rc
    fi
    command loom "$@"
  }
  typeset -gA _loom_seen
  _loom_seen=()
  _loom_chpwd() {
    [[ -n ${_loom_seen[$PWD]-} ]] && return 0
    _loom_seen[$PWD]=1
    # Detached (`&!`: no job, no notice); every error and output discarded.
    { command loom repo register --on-use --background </dev/null >/dev/null 2>&1 &! } 2>/dev/null
    return 0
  }
  autoload -Uz add-zsh-hook
  add-zsh-hook chpwd _loom_chpwd
  _loom_chpwd
fi
