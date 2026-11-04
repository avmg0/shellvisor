# Shellvisor shell integration for bash (works with macOS's bash 3.2 and bash 4/5).
# Loaded via `bash --rcfile <this> -i`. Sources your normal startup files first,
# then reports cwd / command start / exit code to Shellvisor with OSC escapes.

if [ -n "$SHELLVISOR_LOGIN" ]; then
  [ -r /etc/profile ] && . /etc/profile
  if   [ -r "$HOME/.bash_profile" ]; then . "$HOME/.bash_profile"
  elif [ -r "$HOME/.bash_login" ];   then . "$HOME/.bash_login"
  elif [ -r "$HOME/.profile" ];      then . "$HOME/.profile"
  fi
else
  [ -r "$HOME/.bashrc" ] && . "$HOME/.bashrc"
fi
unset SHELLVISOR_LOGIN

__sv_osc() { printf '\033]%s\007' "$1"; }
__sv_b64() { printf '%s' "$1" | base64 | tr -d '\n'; }
__sv_at_prompt=0
__sv_running=0

# Shellvisor's own escapes (OSC 7770) rather than the common OSC 133, so
# prompt frameworks and other terminals' integrations (VTE, kitty, iTerm2)
# that also print 133 can't confuse it.
__sv_preexec() {
  [ "$__sv_at_prompt" = 1 ] || return 0
  case "$BASH_COMMAND" in __sv_*) return 0 ;; esac
  [ -n "$COMP_LINE" ] && return 0
  __sv_at_prompt=0
  __sv_running=1
  printf '\033[0m'  # the typed-command colour ends where output begins
  local cmd
  cmd=$(HISTTIMEFORMAT= builtin history 1 | sed 's/^ *[0-9]*[* ]*//')
  [ -n "$cmd" ] || cmd=$BASH_COMMAND
  __sv_osc "7770;cmd;$(__sv_b64 "$cmd")"
  __sv_osc "7770;start"
}

# first in PROMPT_COMMAND: report how the command ended, and the folder
__sv_precmd() {
  local ec=$?
  __sv_at_prompt=0
  printf '\033[0m'  # an empty line or ctrl+c: no colour left over
  if [ "$__sv_running" = 1 ]; then __sv_osc "7770;end;$ec"; fi
  __sv_running=0
  __sv_osc "7;file://${HOSTNAME}${PWD}"
  return $ec
}

# last in PROMPT_COMMAND: only from here on can a command start (anything
# else in PROMPT_COMMAND also trips the DEBUG trap)
__sv_ready() {
  local ec=$?
  __sv_style
  __sv_osc "7770;prompt"
  __sv_at_prompt=1
  return $ec
}

# ---- Shellvisor settings (settings screen / `sv settings`), read at each
# prompt from $SHELLVISOR_UI: the folder's colour in the prompt, the colour of
# the commands you type, `ls` as a table
__sv_palette="37 39 111 35 178 208 170 167 245"
__sv_ps1_last= ; __sv_ps1_base= ; __sv_lt=1
__sv_style() {
  local c=- b=0 tc=- i p o e
  if [ -r "$SHELLVISOR_UI" ]; then { read -r c; read -r b; read -r __sv_lt; read -r tc; } < "$SHELLVISOR_UI"; fi
  # the prompt changed under us (a theme, conda…): that is the new base
  [ "$PS1" != "$__sv_ps1_last" ] && __sv_ps1_base=$PS1
  p=$__sv_ps1_base
  p=${p%'\[\e[0;38;5;'*'m\]'}
  for i in $__sv_palette; do
    p=${p//"\[\e[1;38;5;${i}m\]"/}
    p=${p//"\[\e[38;5;${i}m\]"/}
  done
  p=${p//'\[\e[22;39m\]'/}
  __sv_ps1_base=$p
  if [ "$c" != - ] && [ -n "$c" ]; then
    o="\[\e[38;5;${c}m\]"; e='\[\e[22;39m\]'
    [ "$b" = 1 ] && o="\[\e[1;38;5;${c}m\]"
    p=${p//'\w'/$o'\w'$e}
    p=${p//'\W'/$o'\W'$e}
  fi
  # typed commands: the colour starts at the end of the prompt; the hooks
  # reset it before the command runs
  if [ "$tc" != - ] && [ -n "$tc" ]; then p="$p\[\e[0;38;5;${tc}m\]"; fi
  PS1=$p
  __sv_ps1_last=$p
}

# `ls` prints a table (sv ls) at the terminal; with other flags, in a pipe,
# or with the setting off it is the real ls
function ls {
  if [ "${__sv_lt:-1}" = 1 ] && [ -t 1 ] && command -v sv >/dev/null 2>&1; then
    local a
    for a in "$@"; do
      case $a in -a|-A|--all|-G|-F|-h|--color|--color=*) ;; -*) command ls "$@"; return ;; esac
    done
    command sv ls "$@"
    return
  fi
  command ls "$@"
}

if [ "${BASH_VERSINFO[0]}" -gt 5 ] || { [ "${BASH_VERSINFO[0]}" -eq 5 ] && [ "${BASH_VERSINFO[1]}" -ge 1 ]; }; then
  # bash 5.1+: PROMPT_COMMAND may be an array; keep every entry, in order
  case " ${PROMPT_COMMAND[*]} " in
    *__sv_precmd*) ;;
    *) PROMPT_COMMAND=(__sv_precmd "${PROMPT_COMMAND[@]}" __sv_ready) ;;
  esac
else
  case "$PROMPT_COMMAND" in
    *__sv_precmd*) ;;
    *) PROMPT_COMMAND="__sv_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}; __sv_ready" ;;
  esac
fi
trap '__sv_preexec' DEBUG
