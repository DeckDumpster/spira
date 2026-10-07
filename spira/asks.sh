#!/usr/bin/env bash
#
# asks.sh — one line per bead newly carrying SPIRA_ASK_LABEL; a watchd daemon.
#
#   asks.sh watch [--interval S] [--ticks N]   loop forever (or N times)
#   asks.sh health                             non-zero when the loop stopped polling
#
# NEW ASK <id>: <title> is printed (the watcher log) and appended to the Concierge inbox as
# "[watch:asks] <event>". Beads open at the first poll are the baseline and are not announced.
# Once every line in the log reached the inbox the cursor is moved to the end of the log, so
# watchd notify does not re-escalate asks the inbox already delivered; a failed inbox write
# holds the cursor for the life of the process, so those lines still escalate.
#
# covers: spira/asks.sh spira/watchers spira/conf.d/SPIRA_ASK_*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

HEALTH_FILE="$SPIRA_RUN/watchd/asks.health"
LOG_FILE="$SPIRA_RUN/watchd/asks.log"
_as_seeded=0 _as_inbox_failed=0
declare -A _as_seen

_as_say() {
    printf '%s %s\n' "$(date -u +%FT%TZ)" "$1"
    { mkdir -p "$(dirname "$SPIRA_CONCIERGE_INBOX")" \
        && printf '%s [watch:asks] %s\n' "$(date -u +%FT%TZ)" "$1" >> "$SPIRA_CONCIERGE_INBOX"; } 2>/dev/null \
        || _as_inbox_failed=1
    return 0
}

_as_mark_delivered() {
    [ "$_as_inbox_failed" = 0 ] || return 0
    [ -f "$LOG_FILE" ] || return 0
    wc -l < "$LOG_FILE" > "${LOG_FILE%.log}.cursor"
}

_as_tick() {
    local id title json
    json="$(timeout 5 "${SPIRA_BD:-bd}" -C "$SPIRA_DB" list --label "$SPIRA_ASK_LABEL" --status open --limit 0 --json 2>/dev/null)" || return 0
    while IFS=$'\t' read -r id title; do
        [ -n "$id" ] || continue
        [ -n "${_as_seen[$id]:-}" ] && continue
        _as_seen[$id]=1
        [ "$_as_seeded" -eq 1 ] && _as_say "NEW ASK $id: $title"
    done < <(printf '%s' "$json" | tr -d '\n' \
        | grep -oE '"id": *"[^"]+", *"title": *"([^"\\]|\\.)*"' \
        | sed -E 's/^"id": *"([^"]+)", *"title": *"(.*)"$/\1\t\2/' | cut -c1-170)
    while read -r id; do
        [ -n "$id" ] || continue
        [ -n "${_as_seen[$id]:-}" ] && continue
        _as_seen[$id]=1
        [ "$_as_seeded" -eq 1 ] && _as_say "NEW ASK HOLD $id (spira-lc show $id names why)"
    done < <(timeout 5 spira-lc list-held ask 2>/dev/null)
    _as_seeded=1
}

_as_write_health() {
    mkdir -p "$(dirname "$HEALTH_FILE")"
    printf 'ok %s %s\n' "$(date +%s)" "$1" > "$HEALTH_FILE.tmp" && mv "$HEALTH_FILE.tmp" "$HEALTH_FILE"
}

cmd_health() {
    [ -r "$HEALTH_FILE" ] || { echo "asks: never polled ($HEALTH_FILE absent)" >&2; return 1; }
    local st t iv age
    read -r st t iv < "$HEALTH_FILE"
    case "$t" in ''|*[!0-9]*) t=0 ;; esac
    case "$iv" in ''|*[!0-9]*) iv=60 ;; esac
    age=$(( $(date +%s) - t ))
    [ "$age" -le $(( 3 * iv )) ] || { echo "asks: last poll ${age}s ago (interval ${iv}s) — hung or dead" >&2; return 1; }
}

cmd_watch() {
    local interval=60 ticks="" tick=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --interval) interval="${2:?--interval needs a value}"; shift 2 ;;
            --ticks)    ticks="${2:?--ticks needs a value}"; shift 2 ;;
            *) echo "asks.sh: watch: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    while :; do
        _as_mark_delivered
        _as_tick
        _as_write_health "$interval"
        tick=$((tick + 1))
        [ -n "$ticks" ] && [ "$tick" -ge "$ticks" ] && return 0
        sleep "$interval"
    done
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        watch)  shift; cmd_watch "$@" ;;
        health) cmd_health; exit $? ;;
        *) echo "usage: asks.sh watch [--interval S] [--ticks N] | health" >&2; exit 2 ;;
    esac
fi
