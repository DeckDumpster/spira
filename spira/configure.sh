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
#                [--dolt-data PATH|""] [--no-repo-map]
#
# NON-INTERACTIVE — every prompt has an env-var path:
#   CONFIGURE_OUT             output file (default: ${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.conf)
#   CONFIGURE_PROD            SPIRA_PROD
#   CONFIGURE_MAX_AEONS       SPIRA_MAX_AEONS
#   CONFIGURE_MAX_LIVE_AEONS  SPIRA_MAX_LIVE_AEONS (empty string = no fleet ceiling)
#   CONFIGURE_LOOM_ADDR       SPIRA_LOOM_ADDR
#   CONFIGURE_DOLT_DATA       SPIRA_DOLT_DATA (empty string = do not manage the server)
#
# An unset env var triggers an interactive prompt if stdin is a TTY, or falls
# through to the derived default with a notice if not.
#
# WHAT IS WRITTEN
#   Trap keys — active KEY = value lines, one per trap.
#   Derivable keys — commented-out # KEY = derived_value lines for every other
#   settable key, so the operator can see what is available.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

# ---------------------------------------------------------------------------
# Argument parsing.  Every trap key tracks "was it given?" separately so that
# an empty string is distinguishable from "not provided yet".
# ---------------------------------------------------------------------------
_out="${CONFIGURE_OUT:-}"
_no_repo_map=

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
        --no-repo-map)      _no_repo_map=1; shift ;;
        *) printf 'configure: unknown argument: %s\n' "$1" >&2; exit 1 ;;
    esac
done

# Default output path.
if [ -z "$_out" ]; then
    _out="${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.conf"
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
# Derive defaults for every configurable key by sourcing conf.sh in a clean
# subprocess.  The result is a set of KEY=value lines we read back below.
# ---------------------------------------------------------------------------
_defaults="$(
    env -i \
        PATH="$PATH" \
        "HOME=${HOME:-}" \
        "XDG_DATA_HOME=${XDG_DATA_HOME:-}" \
        "XDG_CONFIG_HOME=${XDG_CONFIG_HOME:-}" \
        SPIRA_CONF=/nonexistent \
        CONF_HERE="$HERE" \
        bash -c '
            . "$CONF_HERE/conf.sh" 2>/dev/null
            # Print every key in the allowlist with its derived value.
            for _k in $SPIRA_CONF_KEYS; do
                [ -z "$_k" ] && continue
                printf "%s=%s\n" "$_k" "${!_k:-}"
            done
        ' 2>/dev/null
)"

# Return the derived value for KEY.
_def() { printf '%s\n' "$_defaults" | grep "^$1=" | head -1 | cut -d= -f2-; }

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
  The default resolves to SPIRA_RELEASES/current, the symlink activate.sh
  swaps on each deployment. No release is activated on a fresh install;
  run activate.sh before re-running install.sh." \
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
        "SPIRA_MAX_LIVE_AEONS — whole-fleet ceiling (pool + lane fayths combined).
  Empty means no ceiling (the shipped default). Set it when your API account
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
# Write the config file.
# ---------------------------------------------------------------------------
mkdir -p "$(dirname "$_out")"

{
    cat <<HEADER
# spira.conf — written by configure.sh
#
# The first file found among these paths wins:
#   <harness checkout>/spira.conf        (beside conf.sh; do NOT push)
#   \${XDG_CONFIG_HOME:-\$HOME/.config}/spira/spira.conf  (here)
#   /etc/spira/spira.conf
#
# Remove this file and re-run configure.sh to regenerate it.
# Point \$SPIRA_CONF at a different path to use another file.
#
# FORMAT: KEY = value, one per line, # comments, blank lines ignored.
# Values are NOT shell — ~ and \$HOME expand and nothing else does.
# An unrecognised key is printed to stderr and ignored; a typo is not a
# setting the operator believes is in force.

# ============================================================
# TRAP KEYS — explicitly set because the derived default is
# plausibly wrong for a fresh clone.
# ============================================================

HEADER

    # SPIRA_PROD
    cat <<PROD_COMMENT
# SPIRA_PROD: the activated release directory. systemd executes every unit's
# ExecStart from this path. The default is SPIRA_RELEASES/current; install.sh
# refuses until a release is activated with activate.sh.
PROD_COMMENT
    printf 'SPIRA_PROD = %s\n\n' "$_prod"

    # SPIRA_MAX_AEONS
    cat <<AEONS_COMMENT
# SPIRA_MAX_AEONS: maximum aeons that may run at once (the task-pool ceiling,
# not counting lane fayths). Default 4 is a guess about the operator's cores.
AEONS_COMMENT
    printf 'SPIRA_MAX_AEONS = %s\n\n' "${_maxaeons:-4}"

    # SPIRA_MAX_LIVE_AEONS
    cat <<LIVE_COMMENT
# SPIRA_MAX_LIVE_AEONS: whole-fleet ceiling (pool + lane fayths). Empty means
# no ceiling — correct for a core-constrained box, wrong for a constrained API
# account. Set it when the account limit matters more than the core count.
LIVE_COMMENT
    printf 'SPIRA_MAX_LIVE_AEONS = %s\n\n' "$_maxlive"

    # SPIRA_LOOM_ADDR
    cat <<LOOM_COMMENT
# SPIRA_LOOM_ADDR: where Loom (the live-graph server) listens. Loopback by
# design — Loom has no authentication, so changing this to 0.0.0.0 exposes
# your beads database to every host on the LAN.
LOOM_COMMENT
    printf 'SPIRA_LOOM_ADDR = %s\n\n' "${_loom:-127.0.0.1:8788}"

    # SPIRA_DOLT_DATA
    cat <<DOLT_COMMENT
# SPIRA_DOLT_DATA: the Dolt server's own data directory. install.sh creates
# this directory, writes dolt-server.yaml, and starts dolt-beads.service.
# Leave empty only if you run the Dolt server yourself.
DOLT_COMMENT
    printf 'SPIRA_DOLT_DATA = %s\n' "$_dolt"

    cat <<DERIVABLE

# ============================================================
# DERIVABLE KEYS — commented out; values shown are what
# conf.sh would compute from where the harness is installed.
# Uncomment and edit any of these to override the default.
# ============================================================

DERIVABLE

    # Trap keys are already written above; skip them in the comment section.
    _trap_keys=" SPIRA_PROD SPIRA_MAX_AEONS SPIRA_MAX_LIVE_AEONS SPIRA_LOOM_ADDR SPIRA_DOLT_DATA "
    while IFS= read -r _kv; do
        [ -z "$_kv" ] && continue
        _k="${_kv%%=*}"
        _v="${_kv#*=}"
        # Skip trap keys and blank-name entries.
        [ -z "$_k" ] && continue
        case " $_trap_keys " in *" $_k "*) continue ;; esac
        printf '# %s = %s\n' "$_k" "$_v"
    done <<< "$_defaults"
} > "$_out"

# ---------------------------------------------------------------------------
# Round-trip validation: source conf.sh with the generated file and check
# that no "unknown key" warnings appear.  An unknown-key warning would mean
# this script wrote a key conf.sh does not recognise, which is a bug here.
# ---------------------------------------------------------------------------
_rt_warn="$(SPIRA_CONF="$_out" SPIRA_DB="/tmp/configure-nodb-$$" \
    bash -c ". '$HERE/conf.sh'" 2>&1 1>/dev/null || true)"
if printf '%s\n' "$_rt_warn" | grep -q 'unknown key'; then
    printf 'configure: ERROR: generated config contains a key conf.sh does not recognise:\n' >&2
    printf '%s\n' "$_rt_warn" | grep 'unknown key' >&2
    rm -f "$_out"
    exit 1
fi

# ---------------------------------------------------------------------------
# Seed repo-map from the example unless disabled or one already exists.
# ---------------------------------------------------------------------------
if [ -z "$_no_repo_map" ]; then
    _conf_dir="$(dirname "$_out")"
    _map="$_conf_dir/repo-map"
    if [ -f "$_map" ]; then
        printf 'configure: repo-map already exists: %s\n' "$_map"
    else
        cp "$HERE/repo-map.example" "$_map"
        printf 'configure: seeded repo-map from repo-map.example — edit %s\n' "$_map"
    fi
fi

printf 'configure: wrote %s\n' "$_out"
printf 'configure: next steps:\n'
printf '  1. Edit %s (at minimum: update SPIRA_PROD)\n' "$_out"
printf '  2. Run: %s/doctor.sh\n' "$HERE"
