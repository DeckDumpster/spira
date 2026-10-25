#!/usr/bin/env bash
#
# warm-full.sh — says when the aeon warm-slot tmpfs passes 85%, with each slot's size, as a watchd daemon.
#
#   warm-full.sh pass                              one check
#   warm-full.sh watch [--interval S] [--ticks N]  loop forever (or N times)
#   warm-full.sh health                            non-zero when the loop stopped polling
#
# The crossing into full is said once; falling back under clears it. Silent while
# SPIRA_AEON_WARM_BASE is unset or absent.
#
# covers: spira/warm-full.sh spira/watchers
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

HEALTH_FILE="$SPIRA_RUN/watchd/warm-full.health"
SAID="$SPIRA_RUN/watchd/warm-full.said"

_wf_say() {
    printf '%s %s\n' "$(date -u +%FT%TZ)" "$1"
    { mkdir -p "$(dirname "$SPIRA_CONCIERGE_INBOX")" \
        && printf '%s [watch:warm-full] %s\n' "$(date -u +%FT%TZ)" "$1" >> "$SPIRA_CONCIERGE_INBOX"; } 2>/dev/null
    return 0
}

cmd_pass() {
    local base="${SPIRA_AEON_WARM_BASE:-}" lim=85 pct sizes
    [ -n "$base" ] && [ -d "$base" ] || return 0
    pct="$(df --output=pcent "$base" 2>/dev/null | tail -n 1 | tr -dc '0-9')"
    [ -n "$pct" ] || return 0
    if [ "$pct" -lt "$lim" ]; then rm -f "$SAID"; return 0; fi
    [ -e "$SAID" ] && return 0
    sizes="$(du -sm "$base"/slot.[0-9]* 2>/dev/null | awk '{n=$2; sub(".*/","",n); printf "%s=%sMiB ", n, $1}')"
    _wf_say "aeon warm tmpfs $base is ${pct}% full (limit ${lim}%): ${sizes:-no slots}; idle slots are emptied at the next aeon acquire"
    mkdir -p "$(dirname "$SAID")" && : > "$SAID"
}

cmd_health() {
    [ -r "$HEALTH_FILE" ] || { echo "warm-full: never polled ($HEALTH_FILE absent)" >&2; return 1; }
    local st t iv age
    read -r st t iv < "$HEALTH_FILE"
    case "$t" in ''|*[!0-9]*) t=0 ;; esac
    case "$iv" in ''|*[!0-9]*) iv=120 ;; esac
    age=$(( $(date +%s) - t ))
    [ "$age" -le $(( 3 * iv )) ] || { echo "warm-full: last poll ${age}s ago (interval ${iv}s) — hung or dead" >&2; return 1; }
}

cmd_watch() {
    local interval=120 ticks="" tick=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --interval) interval="${2:?--interval needs a value}"; shift 2 ;;
            --ticks)    ticks="${2:?--ticks needs a value}"; shift 2 ;;
            *) echo "warm-full.sh: watch: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    while :; do
        cmd_pass
        mkdir -p "$(dirname "$HEALTH_FILE")"
        printf 'ok %s %s\n' "$(date +%s)" "$interval" > "$HEALTH_FILE.tmp" && mv "$HEALTH_FILE.tmp" "$HEALTH_FILE"
        tick=$((tick + 1))
        [ -n "$ticks" ] && [ "$tick" -ge "$ticks" ] && return 0
        sleep "$interval"
    done
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        pass)   cmd_pass ;;
        watch)  shift; cmd_watch "$@" ;;
        health) cmd_health; exit $? ;;
        *) echo "usage: warm-full.sh pass | watch [--interval S] [--ticks N] | health" >&2; exit 2 ;;
    esac
fi
