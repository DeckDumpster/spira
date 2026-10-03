#!/usr/bin/env bash
#
# stranded-runner.sh — one line per queued self-hosted job of $SPIRA_REPO whose per-run
# runner is not online. Detects earlier and more precisely than GitHub's own 24h queued-job
# ceiling, which was the only backstop before this: a runner that registers and then
# disappears (its VM powered off before taking the job) leaves the job queued with nothing
# ever going to pick it up again, and GitHub does not notice for a day.
#
#   stranded-runner.sh             one scan; same as --show
#   stranded-runner.sh --show      one scan; prints to stdout
#   stranded-runner.sh watch [--interval S] [--ticks N]   loop forever (or N times); watchd daemon
#   stranded-runner.sh health      exit non-zero when the watch loop has stopped polling
#
# THE DETECTION ITSELF LIVES IN the forge binary's stranded-runners verb, not here — this is a
# thin daemon shell around the one forge query, same division as pr-notify.sh and
# publish-backlog.sh keep between "what to report" and "the watchd loop that reports it".
#
# covers: spira/stranded-runner.sh spira/watchers forge/src/cmds.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

HEALTH_FILE="$SPIRA_RUN/watchd/stranded-runner.health"

_sr_tick() {
    [ -n "${SPIRA_REPO:-}" ] || return 0
    local forge="${SPIRA_FORGE:-forge}"
    "$forge" stranded-runners "$SPIRA_REPO" 2>/dev/null
}

_sr_write_health() {
    local interval="$1"
    mkdir -p "$(dirname "$HEALTH_FILE")"
    printf 'ok %s %s\n' "$(date +%s)" "$interval" > "$HEALTH_FILE.tmp" \
        && mv "$HEALTH_FILE.tmp" "$HEALTH_FILE"
}

# cmd_health — the watchd health probe. Fails when the watch loop has never polled, or its
# last poll is older than three intervals (it is hung or dead).
cmd_health() {
    [ -r "$HEALTH_FILE" ] || {
        echo "stranded-runner: never polled ($HEALTH_FILE absent)" >&2
        return 1
    }
    local st t iv now age
    read -r st t iv < "$HEALTH_FILE"
    case "$t" in ''|*[!0-9]*) t=0 ;; esac
    case "$iv" in ''|*[!0-9]*) iv=120 ;; esac
    now="$(date +%s)"
    age=$(( now - t ))
    if [ "$age" -gt $(( 3 * iv )) ]; then
        echo "stranded-runner: last poll ${age}s ago (interval ${iv}s) — the watcher is hung or dead" >&2
        return 1
    fi
    return 0
}

# cmd_watch [--interval S] [--ticks N] — the watchd daemon body. Runs forever (or --ticks
# times, for a test), polling forge and writing a health record each pass.
cmd_watch() {
    local interval=120 ticks="" tick=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --interval) interval="${2:?--interval needs a value}"; shift 2 ;;
            --ticks)    ticks="${2:?--ticks needs a value}"; shift 2 ;;
            *) echo "stranded-runner.sh: watch: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    while :; do
        _sr_tick
        _sr_write_health "$interval"
        tick=$((tick + 1))
        [ -n "$ticks" ] && [ "$tick" -ge "$ticks" ] && return 0
        sleep "$interval"
    done
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        --show|-s|"") _sr_tick ;;
        watch)  shift; cmd_watch "$@" ;;
        health) cmd_health; exit $? ;;
        *) echo "usage: stranded-runner.sh [--show] | watch [--interval S] [--ticks N] | health" >&2
           exit 2 ;;
    esac
fi
