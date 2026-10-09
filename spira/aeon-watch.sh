#!/usr/bin/env bash
#
# aeon-watch.sh — one status line per live aeon, plus a flag line for each fleet fault:
# STALL, GATE, TESTENV, CHURN, REOPENS. A watchd daemon.
#
#   aeon-watch.sh [--show]                       one pass to stdout
#   aeon-watch.sh watch [--interval S] [--ticks N]
#   aeon-watch.sh health
#
# STATUS ONLY: log mtimes, process ages, systemd, the journal's own claim lines. An aeon's
# session transcript is its work and is never read. A process is matched on comm and printed as
# pid, age and unit — never its argv, which carries credentials.
#
# covers: spira/aeon-watch.sh spira/watchers
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
HEALTH_FILE="$SPIRA_RUN/watchd/aeon-watch.health"
PROC="${AEON_WATCH_PROC:-/proc}"
STALL_MIN="${AEON_WATCH_STALL_MIN:-15}"
GATE_MIN="${AEON_WATCH_GATE_MIN:-5}"
TEST_MIN="${AEON_WATCH_TEST_MIN:-15}"
CHURN_MIN_COUNT="${AEON_WATCH_CHURN_COUNT:-5}"
REOPEN_MIN_COUNT="${AEON_WATCH_REOPEN_COUNT:-3}"
WINDOW="${AEON_WATCH_WINDOW:-600}"
ID_PREFIX="${SPIRA_ID_PREFIX:-sp}"

_aw_unit_of() {
    sed -n 's#.*/\([^/]*\.service\).*#\1#p' "$PROC/$1/cgroup" 2>/dev/null | head -1
}

_aw_count() {
    local n
    n="$(timeout 5 journalctl --user --since "-${WINDOW}s" -g "$1" -o cat --no-pager 2>/dev/null \
        | grep -cF -- "$1")"
    printf '%s' "${n:-0}"
}

_aw_tick() {
    local now ts u n=0 unknown=0 st stS who bead fayth age idle flag
    now="$(date +%s)"; ts="$(date -u +%H:%MZ)"
    local lines=""
    while read -r u; do
        [ -n "$u" ] || continue
        n=$((n + 1))
        st="$(systemctl --user show "$u" -p ActiveEnterTimestamp --value 2>/dev/null)"
        stS="$(date -d "$st" +%s 2>/dev/null || echo "$now")"
        who="$(timeout 5 journalctl --user -u "$u" --since "@$stS" -g "(claiming|claimed|resuming) $ID_PREFIX-" -o cat --no-pager 2>/dev/null \
            | grep -oE "[a-z]+/[a-z]+: (claiming|claimed|resuming) $ID_PREFIX-[a-z0-9.]+" | tail -1)"
        bead="$(printf '%s' "$who" | grep -oE "$ID_PREFIX-[a-z0-9.]+")"
        fayth="${who%%:*}"
        age=$(( (now - stS) / 60 )); idle="?"; flag=""
        if [ -n "$bead" ] && [ -f "$SPIRA_RUN/$bead.log" ]; then
            idle=$(( (now - $(stat -c %Y "$SPIRA_RUN/$bead.log")) / 60 ))
            [ "$idle" -gt "$STALL_MIN" ] && flag=" STALL(log idle ${idle}m > ${STALL_MIN}m)"
        fi
        if [ -z "$bead" ]; then flag=" (no claim seen yet)"; unknown=$((unknown + 1)); fi
        lines="$lines
  ${fayth:-?} ${bead:-?}: ${age}m on it, log idle ${idle}m$flag"
    done < <(systemctl --user list-units 'spira-aeon-*' --state=active --no-legend --plain 2>/dev/null | awk '{print $1}')

    local pid et comm m long=""
    [ "$unknown" -ge "$CHURN_MIN_COUNT" ] && long="
  THRASH $unknown of $n live aeons have no claim yet — builders exiting within seconds, the churn seen from the aeon side"
    while read -r pid et comm; do
        [ -n "$pid" ] || continue
        m=$(( et / 60 ))
        case "$comm" in
            gate)    [ "$m" -gt "$GATE_MIN" ] && long="$long
  GATE pid $pid running ${m}m (> ${GATE_MIN}m) in $(_aw_unit_of "$pid")" ;;
            testenv) [ "$m" -gt "$TEST_MIN" ] && long="$long
  TESTENV pid $pid running ${m}m (> ${TEST_MIN}m) in $(_aw_unit_of "$pid")" ;;
        esac
    done < <(ps -eo pid=,etimes=,comm= 2>/dev/null | awk '$3=="gate"||$3=="testenv"')

    local churn reopen
    churn="$(_aw_count 'nothing ready to claim')"
    reopen="$(_aw_count 'REOPENED — fast tier red')"
    [ "$churn" -ge "$CHURN_MIN_COUNT" ] && long="$long
  CHURN $churn builders exited 'nothing ready to claim' in the last $((WINDOW / 60))m"
    [ "$reopen" -ge "$REOPEN_MIN_COUNT" ] && long="$long
  REOPENS $reopen fast-tier refusals in the last $((WINDOW / 60))m (check for a base-wide cause)"

    echo "[$ts] aeons live: $n$lines$long"
}

_aw_write_health() {
    mkdir -p "$(dirname "$HEALTH_FILE")"
    printf 'ok %s %s\n' "$(date +%s)" "$1" > "$HEALTH_FILE.tmp" \
        && mv "$HEALTH_FILE.tmp" "$HEALTH_FILE"
}

cmd_health() {
    [ -r "$HEALTH_FILE" ] || {
        echo "aeon-watch: never polled ($HEALTH_FILE absent)" >&2
        return 1
    }
    local st t iv age
    read -r st t iv < "$HEALTH_FILE"
    case "$t" in ''|*[!0-9]*) t=0 ;; esac
    case "$iv" in ''|*[!0-9]*) iv=600 ;; esac
    age=$(( $(date +%s) - t ))
    if [ "$age" -gt $(( 3 * iv )) ]; then
        echo "aeon-watch: last poll ${age}s ago (interval ${iv}s) — the watcher is hung or dead" >&2
        return 1
    fi
}

cmd_watch() {
    local interval=600 ticks="" tick=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --interval) interval="${2:?--interval needs a value}"; shift 2 ;;
            --ticks)    ticks="${2:?--ticks needs a value}"; shift 2 ;;
            *) echo "aeon-watch.sh: watch: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    WINDOW="${AEON_WATCH_WINDOW:-$interval}"
    while :; do
        _aw_tick
        _aw_write_health "$interval"
        tick=$((tick + 1))
        [ -n "$ticks" ] && [ "$tick" -ge "$ticks" ] && return 0
        sleep "$interval"
    done
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        --show|-s|"") _aw_tick ;;
        watch)  shift; cmd_watch "$@" ;;
        health) cmd_health; exit $? ;;
        *) echo "usage: aeon-watch.sh [--show] | watch [--interval S] [--ticks N] | health" >&2
           exit 2 ;;
    esac
fi
