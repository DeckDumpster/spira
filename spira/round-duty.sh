#!/usr/bin/env bash
#
# round-duty.sh — ROUND DUE and ROUND RESULT events for a round owner, as a watchd daemon.
#
#   round-duty.sh watch [--interval S] [--ticks N]   loop forever (or N times)
#   round-duty.sh health                             non-zero when the loop stopped polling
#
# Every event is printed (the watcher log) and also appended to the Concierge inbox as
# "[watch:round-duty] <event>". A failed inbox write never drops the event: the log line is
# already written.
#
# Whether a round is running is asked of `queue round status`, the only authority. A failed or
# unparseable answer is "unknown" and is reported as ROUND STATE UNKNOWN, never as "no round
# running" — absence of evidence is not a round-less queue (law-absence-needs-a-positive-control).
#
# covers: spira/round-duty.sh spira/watchers spira/conf.d/SPIRA_ROUND_*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

ROUNDS="$SPIRA_RUN/rounds"
HEALTH_FILE="$SPIRA_RUN/watchd/round-duty.health"
_rd_last_due=0 _rd_last_unknown=0 _rd_last_c="" _rd_last_change=0
declare -A _rd_lines

_rd_say() {
    printf '%s %s\n' "$(date -u +%FT%TZ)" "$1"
    { mkdir -p "$(dirname "$SPIRA_CONCIERGE_INBOX")" \
        && printf '%s [watch:round-duty] %s\n' "$(date -u +%FT%TZ)" "$1" >> "$SPIRA_CONCIERGE_INBOX"; } 2>/dev/null
    return 0
}

_rd_certified() {
    local n
    n="$(spira-lc list-state CERTIFIED 2>/dev/null | grep -c .)"
    echo "${n:-0}"
}

# Sets _rd_state to open, none or unknown.
_rd_round_state() {
    local out rc
    out="$(queue round status 2>/dev/null)"; rc=$?
    _rd_state=unknown
    [ "$rc" -eq 0 ] || return 0
    case "$(printf '%s\n' "$out" | sed -n 's/^round=//p' | head -n1)" in
        open) _rd_state=open ;;
        none) _rd_state=none ;;
    esac
}

_rd_tick() {
    local f n p c now idle
    mkdir -p "$ROUNDS"
    for f in "$ROUNDS"/*.result; do
        [ -e "$f" ] || continue
        n="$(wc -l < "$f")"; p="${_rd_lines[$f]:-}"
        if [ -z "$p" ]; then _rd_lines[$f]=$n; continue; fi
        if [ "$n" -gt "$p" ]; then
            tail -n $((n - p)) "$f" | while IFS= read -r l; do
                _rd_say "ROUND RESULT $(basename "$f" .result): $l"
                case "${l,,}" in
                    *"no verdict"*|*"no-verdict"*|*"verdict-less"*|*"verdictless"*)
                        _rd_say "ROUND NO VERDICTS $(basename "$f" .result): the round produced no verdicts — a fault of the round machinery (VM, mirror, host address), not of the candidates; fix it before cutting another" ;;
                esac
            done
            _rd_lines[$f]=$n
        fi
    done
    c="$(_rd_certified)"; now="$(date +%s)"
    [ "$c" != "$_rd_last_c" ] && { _rd_last_c="$c"; _rd_last_change=$now; }
    idle=$(( now - _rd_last_change ))
    _rd_round_state
    if [ "$_rd_state" = unknown ]; then
        if [ $((now - _rd_last_unknown)) -ge "$SPIRA_ROUND_DUE_REMIND" ]; then
            _rd_say "ROUND STATE UNKNOWN: 'queue round status' gave no usable answer — cannot tell whether a round is running, so ROUND DUE is withheld"
            _rd_last_unknown=$now
        fi
        return 0
    fi
    _rd_last_unknown=0
    if [ "$_rd_state" = none ] && { [ "$c" -ge "$SPIRA_ROUND_MIN" ] \
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
