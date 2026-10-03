#!/usr/bin/env bash
#
# round-duty.sh — ROUND DUE and ROUND RESULT events for a round owner, as a watchd daemon.
#
#   round-duty.sh watch [--interval S] [--ticks N]   loop forever (or N times)
#   round-duty.sh health                             non-zero when the loop stopped polling
#
# A $SPIRA_RUN/rounds/<round>.running marker means a round's corpus is in progress and
# suppresses ROUND DUE. Nothing guarantees the marker is removed, and a leftover one silences
# the alert with no symptom, so a marker is believed only while it is younger than
# SPIRA_ROUND_MARKER_MAX_AGE and its round has written no .result since: otherwise it is
# reported as stale, removed, and ROUND DUE is judged without it.
#
# covers: spira/round-duty.sh spira/watchers spira/conf.d/SPIRA_ROUND_*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

ROUNDS="$SPIRA_RUN/rounds"
HEALTH_FILE="$SPIRA_RUN/watchd/round-duty.health"
_rd_last_due=0 _rd_last_c="" _rd_last_change=0
declare -A _rd_lines

_rd_say() { printf '%s %s\n' "$(date -u +%FT%TZ)" "$1"; }

_rd_certified() {
    local f s t id n=0
    for f in "$SPIRA_RUN"/landstate/sp-*; do
        read -r s t _ < "$f" 2>/dev/null || continue
        [ "$s" = CERTIFIED ] || continue
        id="$(basename "$f")"
        [ "$(git -C "$SPIRA_REPO" rev-parse -q --verify "refs/heads/spira/$id" 2>/dev/null)" = "$t" ] || continue
        n=$((n + 1))
    done
    echo "$n"
}

# Sets _rd_live to the number of live markers; removes and reports each stale one.
_rd_live_markers() {
    local m base res now age
    _rd_live=0
    now="$(date +%s)"
    for m in "$ROUNDS"/*.running; do
        [ -e "$m" ] || continue
        base="$(basename "$m" .running)"; res="$ROUNDS/$base.result"
        age=$(( now - $(stat -c %Y "$m" 2>/dev/null || echo "$now") ))
        if [ "$age" -gt "$SPIRA_ROUND_MARKER_MAX_AGE" ]; then
            _rd_say "ROUND MARKER STALE: rounds/$base.running is ${age}s old (limit ${SPIRA_ROUND_MARKER_MAX_AGE}s) — removed, ignoring it"
            rm -f "$m"
        elif [ -e "$res" ] && [ ! "$res" -ot "$m" ]; then
            _rd_say "ROUND MARKER STALE: rounds/$base.running outlived its round (rounds/$base.result is newer) — removed, ignoring it"
            rm -f "$m"
        else
            _rd_live=$((_rd_live + 1))
        fi
    done
}

_rd_tick() {
    local f n p c now idle
    mkdir -p "$ROUNDS"
    for f in "$ROUNDS"/*.result; do
        [ -e "$f" ] || continue
        n="$(wc -l < "$f")"; p="${_rd_lines[$f]:-}"
        if [ -z "$p" ]; then _rd_lines[$f]=$n; continue; fi
        if [ "$n" -gt "$p" ]; then
            tail -n $((n - p)) "$f" | while IFS= read -r l; do _rd_say "ROUND RESULT $(basename "$f" .result): $l"; done
            _rd_lines[$f]=$n
        fi
    done
    c="$(_rd_certified)"; now="$(date +%s)"
    [ "$c" != "$_rd_last_c" ] && { _rd_last_c="$c"; _rd_last_change=$now; }
    idle=$(( now - _rd_last_change ))
    _rd_live_markers
    if [ "$_rd_live" -eq 0 ] && { [ "$c" -ge "$SPIRA_ROUND_MIN" ] \
            || { [ "$c" -ge 1 ] && [ "$idle" -ge "$SPIRA_ROUND_IDLE_CUT" ]; }; }; then
        if [ $((now - _rd_last_due)) -ge "$SPIRA_ROUND_DUE_REMIND" ]; then
            _rd_say "ROUND DUE: $c certified waiting (idle ${idle}s), no round running — cut the next round"
            _rd_last_due=$now
        fi
    else
        _rd_last_due=0
    fi
}

_rd_write_health() {
    mkdir -p "$(dirname "$HEALTH_FILE")"
    printf 'ok %s %s\n' "$(date +%s)" "$1" > "$HEALTH_FILE.tmp" && mv "$HEALTH_FILE.tmp" "$HEALTH_FILE"
}

cmd_health() {
    [ -r "$HEALTH_FILE" ] || { echo "round-duty: never polled ($HEALTH_FILE absent)" >&2; return 1; }
    local st t iv age
    read -r st t iv < "$HEALTH_FILE"
    case "$t" in ''|*[!0-9]*) t=0 ;; esac
    case "$iv" in ''|*[!0-9]*) iv=15 ;; esac
    age=$(( $(date +%s) - t ))
    [ "$age" -le $(( 3 * iv )) ] || { echo "round-duty: last poll ${age}s ago (interval ${iv}s) — hung or dead" >&2; return 1; }
}

cmd_watch() {
    local interval=15 ticks="" tick=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --interval) interval="${2:?--interval needs a value}"; shift 2 ;;
            --ticks)    ticks="${2:?--ticks needs a value}"; shift 2 ;;
            *) echo "round-duty.sh: watch: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    while :; do
        _rd_tick
        _rd_write_health "$interval"
        tick=$((tick + 1))
        [ -n "$ticks" ] && [ "$tick" -ge "$ticks" ] && return 0
        sleep "$interval"
    done
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        watch)  shift; cmd_watch "$@" ;;
        health) cmd_health; exit $? ;;
        *) echo "usage: round-duty.sh watch [--interval S] [--ticks N] | health" >&2; exit 2 ;;
    esac
fi
