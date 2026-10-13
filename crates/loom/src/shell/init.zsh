# loom shell integration for zsh. In ~/.zshrc: eval "$(loom shell-init zsh)"
# Each directory the shell enters, once per session, is sent to loomd in the background:
# loomd registers the repository it is in. Nothing happens when loom is not installed.
if command -v loom >/dev/null 2>&1; then
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
