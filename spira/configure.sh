#!/usr/bin/env bash
# configure.sh — bootstrap ~/.config/spira/ for an operator who has just cloned.
#
# Asks only about keys whose derived default is a trap. Writes the rest as
# commented-out lines showing what conf.sh would derive, so the file teaches what
# is available without forcing the operator to read the full key list.
#
# WHAT "TRAP KEY" MEANS. A key whose derived default is plausibly wrong for a
# fresh clone. On this harness the four traps are:
#
#   SPIRA_PROD         derives to a path that typically does not exist on a new
#                      machine; install.sh refuses to write units whose ExecStart
#                      target is absent, so an unset SPIRA_PROD stops installation.
#
#   SPIRA_MAX_AEONS /  a guess about the operator's own cores and account limits.
#   SPIRA_MAX_LIVE_AEONS   Default (4 / empty) is wrong for a single-core box or
#                      a constrained API account and right for a sixteen-core one.
#
#   SPIRA_LOOM_ADDR    Loopback by design; Loom has no authentication in front of
#                      it, so binding to 0.0.0.0 exposes it to the LAN without any
#                      credential check. The default is safe, but it is the kind of
#                      value operators change without realising the consequence.
#
#   SPIRA_DOLT_DATA    Dolt server data directory. install.sh creates it, writes
#                      dolt-server.yaml, and starts dolt-beads.service. Leave empty
#                      only if you run the Dolt server yourself.
#
# USAGE
#   configure.sh [--out PATH] [--prod PATH] [--max-aeons N]
#                [--max-live-aeons N|""] [--loom-addr ADDR]
#                [--dolt-data PATH|""]
#
# NON-INTERACTIVE — every prompt has an env-var path:
#   CONFIGURE_OUT             output file (default: ${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.toml)
#   CONFIGURE_PROD            SPIRA_PROD
#   CONFIGURE_MAX_AEONS       SPIRA_MAX_AEONS
#   CONFIGURE_MAX_LIVE_AEONS  SPIRA_MAX_LIVE_AEONS (empty string = the registered default)
#   CONFIGURE_LOOM_ADDR       SPIRA_LOOM_ADDR
#   CONFIGURE_DOLT_DATA       SPIRA_DOLT_DATA (empty string = do not manage the server)
#
# An unset env var triggers an interactive prompt if stdin is a TTY, or falls
# through to the derived default with a notice if not.
#
# WHAT IS WRITTEN
#   `spira-config init` writes the answers and every other key at its registered
#   default, because the resolver refuses a key the file does not declare. Further
#   inputs: CONFIGURE_INSTANCE, CONFIGURE_ID_PREFIX, CONFIGURE_HOME_REPO,
#   CONFIGURE_HOME_REPO_PATH, CONFIGURE_DB, CONFIGURE_RUN, CONFIGURE_RELEASES,
#   CONFIGURE_OPERATED (0 = nobody reads escalations: doctor warns rather than fails).
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

# configure runs before install puts the release's bin/ on PATH: find spira-config in this tree's own release.
if ! command -v spira-config >/dev/null 2>&1 && [ -x "$HERE/../bin/spira-config" ]; then
    PATH="$(cd "$HERE/../bin" && pwd -P):$PATH"
fi

# ---------------------------------------------------------------------------
# Argument parsing.  Every trap key tracks "was it given?" separately so that
# an empty string is distinguishable from "not provided yet".
# ---------------------------------------------------------------------------
_out="${CONFIGURE_OUT:-}"

# Trap-key "given" flags — set if the var was exported by the caller.
_prod_given=;      [ -n "${CONFIGURE_PROD+x}"            ] && _prod_given=1
_maxaeons_given=;  [ -n "${CONFIGURE_MAX_AEONS+x}"       ] && _maxaeons_given=1
_maxlive_given=;   [ -n "${CONFIGURE_MAX_LIVE_AEONS+x}"  ] && _maxlive_given=1
_loom_given=;      [ -n "${CONFIGURE_LOOM_ADDR+x}"       ] && _loom_given=1
_dolt_given=;      [ -n "${CONFIGURE_DOLT_DATA+x}"       ] && _dolt_given=1

_prod="${CONFIGURE_PROD:-}"
_maxaeons="${CONFIGURE_MAX_AEONS:-}"
_maxlive="${CONFIGURE_MAX_LIVE_AEONS:-}"
_loom="${CONFIGURE_LOOM_ADDR:-}"
_dolt="${CONFIGURE_DOLT_DATA:-}"

while [ $# -gt 0 ]; do
    case "$1" in
        --out)              _out="$2";      shift 2 ;;
        --prod)             _prod="$2";    _prod_given=1;     shift 2 ;;
        --max-aeons)        _maxaeons="$2"; _maxaeons_given=1; shift 2 ;;
        --max-live-aeons)   _maxlive="$2"; _maxlive_given=1;  shift 2 ;;
        --loom-addr)        _loom="$2";    _loom_given=1;     shift 2 ;;
        --dolt-data)        _dolt="$2";    _dolt_given=1;     shift 2 ;;
        *) printf 'configure: unknown argument: %s\n' "$1" >&2; exit 1 ;;
    esac
done

# Default output path.
if [ -z "$_out" ]; then
    _out="${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.toml"
fi

# ---------------------------------------------------------------------------
# Guard: never overwrite an existing config.
# ---------------------------------------------------------------------------
if [ -f "$_out" ]; then
    printf 'configure: config file already exists: %s\n' "$_out"
    printf 'configure: remove it first if you want to regenerate it\n'
    exit 1
fi

# ---------------------------------------------------------------------------
# Defaults for the keys asked about and the paths a fresh box must name. Every other
# key is left to `spira-config init`, which writes it at its registered default.
# ---------------------------------------------------------------------------
_data="${XDG_DATA_HOME:-$HOME/.local/share}/spira"
_releases="${CONFIGURE_RELEASES:-$_data/releases}"
declare -A _defaults=(
    [SPIRA_PROD]="$_releases/current"
    [SPIRA_MAX_AEONS]=4
    [SPIRA_MAX_LIVE_AEONS]=
    [SPIRA_LOOM_ADDR]=127.0.0.1:8788
    [SPIRA_DOLT_DATA]="$_data/dolt"
)
_def() { printf '%s' "${_defaults[$1]-}"; }

# ---------------------------------------------------------------------------
# Interactive prompts for any trap key not already provided.
# Prints the prompt to stderr; reads from stdin; falls back to the derived
# default when stdin is not a TTY (so CI never hangs).
# ---------------------------------------------------------------------------
_ask() {
    local label="$1" key="$2" default="$3"
    if [ -t 0 ] && [ -t 2 ]; then
        printf '\n%s\n  default (from conf.sh): %s\n  value: ' "$label" "$default" >&2
        local ans
        read -r ans
        [ -z "$ans" ] && ans="$default"
        printf '%s' "$ans"
    else
        printf 'configure: no TTY; %s using derived default: %s\n' "$key" "$default" >&2
        printf '%s' "$default"
    fi
}

if [ -z "$_prod_given" ]; then
    _prod="$(_ask \
        "SPIRA_PROD — the activated release directory systemd executes from.
  The default resolves to SPIRA_RELEASES/current, the symlink release install-tarball
  swaps on each deployment. No release is activated on a fresh install;
  run release install-tarball before re-running install.sh." \
        SPIRA_PROD "$(_def SPIRA_PROD)")"
fi

if [ -z "$_maxaeons_given" ]; then
    _maxaeons="$(_ask \
        "SPIRA_MAX_AEONS — maximum aeons that may run at once (the task-pool ceiling).
  Four is the shipped default. Raise it on a many-core box; lower it on a
  single-core one or a constrained API account." \
        SPIRA_MAX_AEONS "$(_def SPIRA_MAX_AEONS)")"
fi

if [ -z "$_maxlive_given" ]; then
    _maxlive="$(_ask \
        "SPIRA_MAX_LIVE_AEONS — whole-fleet ceiling (pool + lane runes combined).
  Empty takes the registered default. Set it when your API account
  limits how many parallel sessions you can run across ALL personas." \
        SPIRA_MAX_LIVE_AEONS "$(_def SPIRA_MAX_LIVE_AEONS)")"
fi

if [ -z "$_loom_given" ]; then
    _loom="$(_ask \
        "SPIRA_LOOM_ADDR — where Loom listens (host:port).
  Loopback (127.0.0.1:8788) is the safe default: Loom has no authentication,
  so 0.0.0.0 exposes your beads database to anyone on the LAN." \
        SPIRA_LOOM_ADDR "$(_def SPIRA_LOOM_ADDR)")"
fi

if [ -z "$_dolt_given" ]; then
    _dolt="$(_ask \
        "SPIRA_DOLT_DATA — the Dolt server's data directory.
  install.sh creates this directory, writes dolt-server.yaml, and starts
  dolt-beads.service to manage the server. Leave empty only if you run
  the Dolt server yourself and handle the data directory manually." \
        SPIRA_DOLT_DATA "$(_def SPIRA_DOLT_DATA)")"
fi

# ---------------------------------------------------------------------------
# Write the config file through `spira-config init`: the answers below, and every other
# key at its registered default, so the resolver finds every key declared.
# ---------------------------------------------------------------------------
_repo_root="$(cd "$HERE/.." && pwd -P)"
_init_args=(
    --out "$_out"
    --instance "${CONFIGURE_INSTANCE:-prod}"
    --id_prefix "${CONFIGURE_ID_PREFIX:-sp}"
    --home_repo "${CONFIGURE_HOME_REPO:-$(basename "$_repo_root")}"
    --home_repo_path "${CONFIGURE_HOME_REPO_PATH:-$_repo_root}"
    --db "${CONFIGURE_DB:-$_data/db}"
    --run "${CONFIGURE_RUN:-$_data/run}"
    --releases "$_releases"
    --dolt_data "${_dolt:-$_data/dolt}"
    --prod "$_prod"
    --max_aeons "${_maxaeons:-4}"
    --loom_addr "${_loom:-127.0.0.1:8788}"
)
[ -n "$_maxlive" ] && _init_args+=(--max_live_aeons "$_maxlive")
[ -n "${CONFIGURE_OPERATED:-}" ] && _init_args+=(--operated "$CONFIGURE_OPERATED")
mkdir -p "$(dirname "$_out")"
SPIRA_HOME="$HERE" spira-config init "${_init_args[@]}" >/dev/null </dev/null \
    || { rm -f "$_out"; printf 'configure: ERROR: could not write %s\n' "$_out" >&2; exit 1; }
[ -n "$_dolt" ] || spira-config set spira.dolt_data "" "$_out" >/dev/null \
    || { rm -f "$_out"; printf 'configure: ERROR: could not write dolt_data\n' >&2; exit 1; }

# Round-trip: the generated file must pass spira-config's own validation.
if ! _rt_warn="$(spira-config validate "$_out" 2>&1)"; then
    printf 'configure: ERROR: generated config is invalid:\n%s\n' "$_rt_warn" >&2
    rm -f "$_out"
    exit 1
fi

printf 'configure: wrote %s\n' "$_out"
printf 'configure: next steps:\n'
printf '  1. Edit %s (at minimum: update SPIRA_PROD)\n' "$_out"
printf '  2. Run: doctor\n'
