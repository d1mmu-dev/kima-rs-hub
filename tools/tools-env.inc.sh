# Sourced by the dev tools: loads KEY=VALUE lines from the PRIVATE tools config (never in the repository) for
# every key the environment does not already set. Path: $KIMA_RS_TOOLS_ENV, default
# ${XDG_CONFIG_HOME:-~/.config}/kima-rs/tools.env. Values are taken literally (no shell evaluation).
KIMA_RS_TOOLS_ENV="${KIMA_RS_TOOLS_ENV:-${XDG_CONFIG_HOME:-$HOME/.config}/kima-rs/tools.env}"
if [ -f "$KIMA_RS_TOOLS_ENV" ]; then
  while IFS= read -r _line || [ -n "$_line" ]; do
    case "$_line" in '' | '#'*) continue ;; esac
    _key="${_line%%=*}"
    case "$_key" in *[!A-Za-z0-9_]* | '') continue ;; esac
    [ -n "${!_key:-}" ] || export "$_key=${_line#*=}"
  done <"$KIMA_RS_TOOLS_ENV"
  unset _line _key
fi
# KIMA_TOOLS_PATH: extra directories (node, npm, ffmpeg, ...) put in front of PATH.
if [ -n "${KIMA_TOOLS_PATH:-}" ]; then export PATH="$KIMA_TOOLS_PATH:$PATH"; fi
