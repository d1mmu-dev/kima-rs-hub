# Sourced by the dev tools: loads KEY=VALUE lines from the PRIVATE tools config (never in the repository). Path:
# $KIMA_RS_TOOLS_ENV, default ${XDG_CONFIG_HOME:-~/.config}/kima-rs/tools.env. Values are taken literally (no shell
# evaluation). Only these six keys are ever read; any other line (LD_PRELOAD, BASH_ENV, NODE_OPTIONS, GIT_DIR,
# npm_config_*, ...) is ignored. A key already present in the environment, even set to an empty value, wins over the
# config. tools/tools-env.mjs applies the same rules for the Node tools.
#   KIMA_HUB_REFERENCE KIMA_REFERENCE_COPY KIMA_CSS_DEPS KIMA_TOOLS_PATH VERTIGO FFMPEG
KIMA_RS_TOOLS_ENV="${KIMA_RS_TOOLS_ENV:-${XDG_CONFIG_HOME:-$HOME/.config}/kima-rs/tools.env}"
if [ -f "$KIMA_RS_TOOLS_ENV" ]; then
  while IFS= read -r _line || [ -n "$_line" ]; do
    _key="${_line%%=*}"
    case "$_line" in *=*) ;; *) continue ;; esac
    case "$_key" in
      KIMA_HUB_REFERENCE | KIMA_REFERENCE_COPY | KIMA_CSS_DEPS | KIMA_TOOLS_PATH | VERTIGO | FFMPEG) ;;
      *) continue ;;
    esac
    [ -n "${!_key+set}" ] || export "$_key=${_line#*=}"
  done <"$KIMA_RS_TOOLS_ENV"
  unset _line _key
fi
# KIMA_TOOLS_PATH: extra directories (node, npm, ffmpeg, ...) put in front of PATH.
if [ -n "${KIMA_TOOLS_PATH:-}" ]; then export PATH="$KIMA_TOOLS_PATH:$PATH"; fi
