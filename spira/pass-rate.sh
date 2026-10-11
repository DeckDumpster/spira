#!/usr/bin/env bash
#
# pass-rate.sh — the green-pass scoreboard: gate passes per landed round, from the batch
# timing ledger gate-timing.sh writes. A pass is a ledger row; a landing is a green row.
#
#   pass-rate.sh check [--now EPOCH]   print "<passes> <landed> <pct> ok|over"; exit 1 when over
#   pass-rate.sh watch [--interval S] [--ticks N]   watchd daemon: mails the crossing only
#   pass-rate.sh health                exit non-zero when the watch loop has stopped polling
#
# Over is more than SPIRA_PASS_RATE_MAX_PCT percent passes per landing across the last
# SPIRA_PASS_RATE_WINDOW seconds; passes with no landing at all are over, never a divide by
# zero read as fine.

# covers: spira/pass-rate.sh spira/gate-timing.sh spira/watchers spira/conf.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

STATE_FILE="$SPIRA_RUN/watchd/pass-rate.state"
HEALTH_FILE="$SPIRA_RUN/watchd/pass-rate.health"

_pr_measure() {
    local now="${1:-$(date +%s)}" since
    [ -r "$SPIRA_BATCH_LEDGER" ] || { printf '0 0 0 ok\n'; return 0; }
    since="$(date -u -d "@$((now - SPIRA_PASS_RATE_WINDOW))" +%Y-%m-%dT%H:%M:%SZ)" || return 2
    awk -F'\t' -v since="$since" -v max="$SPIRA_PASS_RATE_MAX_PCT" '
        $2 >= since { p++; if ($5 == "green") g++ }
        END {
            pct = g ? int(p * 100 / g) : (p ? 999999 : 0)
            printf "%d %d %d %s\n", p, g, pct, (p && pct > max) ? "over" : "ok"
        }' "$SPIRA_BATCH_LEDGER"
}

_pr_report() {
    printf '%s\n' "$1"
    mail send concierge \
        --from "Pass Rate <pass-rate@spira>" \
        --subject "pass-rate: $1" \
        --kind event <<MAILEOF >/dev/null || printf 'pass-rate: mail send failed for: %s\n' "$1" >&2
## Event
$1
MAILEOF
}

_pr_tick() {
    local out passes landed pct state prev=ok
    out="$(_pr_measure)" || { _pr_report "BLIND: the pass-rate watcher cannot read the ledger window"; return 0; }
    read -r passes landed pct state <<< "$out"
    [ -r "$STATE_FILE" ] && read -r prev < "$STATE_FILE"
    if [ "$state" = over ] && [ "$prev" != over ]; then
        _pr_report "OVER: $passes gate pass(es) for $landed landing(s) in ${SPIRA_PASS_RATE_WINDOW}s (limit ${SPIRA_PASS_RATE_MAX_PCT}%) — rounds are not landing on their first green pass"
    elif [ "$state" = ok ] && [ "$prev" = over ]; then
        _pr_report "CLEAR: $passes gate pass(es) for $landed landing(s), back under ${SPIRA_PASS_RATE_MAX_PCT}%"
    fi
    mkdir -p "$(dirname "$STATE_FILE")"
    printf '%s\n' "$state" > "$STATE_FILE.tmp" && mv "$STATE_FILE.tmp" "$STATE_FILE"
}

cmd_check() {
    local now=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --now) now="${2:?--now needs a value}"; shift 2 ;;
            *) echo "pass-rate.sh: check: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    local out
    out="$(_pr_measure "$now")" || return 2
    printf '%s\n' "$out"
    [ "${out##* }" = ok ]
}

cmd_health() {
    [ -r "$HEALTH_FILE" ] || { echo "pass-rate: never polled ($HEALTH_FILE absent)" >&2; return 1; }
    local t iv age
    read -r t iv < "$HEALTH_FILE"
    case "$t" in ''|*[!0-9]*) t=0 ;; esac
    case "$iv" in ''|*[!0-9]*) iv=900 ;; esac
    age=$(( $(date +%s) - t ))
    if [ "$age" -gt $(( 3 * iv )) ]; then
        echo "pass-rate: last poll ${age}s ago (interval ${iv}s) — the watcher is hung or dead" >&2
        return 1
    fi
}

cmd_watch() {
    local interval=900 ticks="" tick=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --interval) interval="${2:?--interval needs a value}"; shift 2 ;;
            --ticks)    ticks="${2:?--ticks needs a value}"; shift 2 ;;
            *) echo "pass-rate.sh: watch: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    while :; do
        _pr_tick
        mkdir -p "$(dirname "$HEALTH_FILE")"
        printf '%s %s\n' "$(date +%s)" "$interval" > "$HEALTH_FILE.tmp" && mv "$HEALTH_FILE.tmp" "$HEALTH_FILE"
        tick=$((tick + 1))
        [ -n "$ticks" ] && [ "$tick" -ge "$ticks" ] && return 0
        sleep "$interval"
    done
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        check|"") shift; cmd_check "$@"; exit $? ;;
        watch)  shift; cmd_watch "$@" ;;
        health) cmd_health; exit $? ;;
        *) echo "usage: pass-rate.sh check [--now EPOCH] | watch [--interval S] [--ticks N] | health" >&2
           exit 2 ;;
    esac
fi
