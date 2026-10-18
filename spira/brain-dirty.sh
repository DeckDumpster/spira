#!/usr/bin/env bash
#
# brain-dirty.sh — alarm when a path under the wiki checkout stays uncommitted past SPIRA_BRAIN_DIRTY_AGE.
#
#   brain-dirty.sh pass                              alarm once per stale path (prints, and tells the inbox)
#   brain-dirty.sh watch [--interval S] [--ticks N]  watchd daemon: pass, then sleep
#   brain-dirty.sh health                            non-zero when the loop stopped polling
#
# Replicas sync only committed pages and no other session may stage a path it did not write, so
# a dirty page has no one coming for it. Age is the file's mtime; a deleted path has none and is
# not judged.

# covers: spira/brain-dirty.sh spira/watchers spira/conf.d/SPIRA_BRAIN_DIRTY_AGE archivist/src/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"

HEALTH_FILE=""
_bd_init() {
    . "$HERE/lib.sh"
    HEALTH_FILE="$SPIRA_RUN/watchd/brain-dirty.health"
}

_bd_once() {   # _bd_once <key> <message>
    local mark="$SPIRA_RUN/watchd/brain-dirty.said"
    grep -qxF -- "$1" "$mark" 2>/dev/null && return 0
    printf '%s %s\n' "$(date -u +%FT%TZ)" "$2"
    { mkdir -p "$(dirname "$SPIRA_CONCIERGE_INBOX")" \
        && printf '%s [watch:brain-dirty] %s\n' "$(date -u +%FT%TZ)" "$2" >> "$SPIRA_CONCIERGE_INBOX"; } 2>/dev/null
    mkdir -p "$(dirname "$mark")" && printf '%s\n' "$1" >> "$mark"
}

cmd_pass() {
    _bd_init
    [ -n "${SPIRA_WIKI:-}" ] || return 0
    local now rec path mt age
    now="$(date +%s)"
    if ! git -C "$SPIRA_WIKI" rev-parse --git-dir >/dev/null 2>&1; then
        _bd_once "notrepo" "BRAIN DIRTY: SPIRA_WIKI ($SPIRA_WIKI) is not a git repository; nothing it holds can be committed"
        return 0
    fi
    while IFS= read -r -d '' rec; do
        path="${rec:3}"
        case "${rec:0:1}" in R|C) read -r -d '' _ ;; esac
        mt="$(stat -c %Y -- "$SPIRA_WIKI/$path" 2>/dev/null)" || continue
        age=$(( now - mt ))
        [ "$age" -gt "$SPIRA_BRAIN_DIRTY_AGE" ] || continue
        _bd_once "$path@$mt" "BRAIN DIRTY: $path has been uncommitted for $(( age / 60 )) min; commit it by explicit path (git -C $SPIRA_WIKI add -- $path)"
    done < <(git -C "$SPIRA_WIKI" status --porcelain -z --untracked-files=all 2>/dev/null)
}

_bd_write_health() {
    mkdir -p "$(dirname "$HEALTH_FILE")"
    printf 'ok %s %s\n' "$(date +%s)" "$1" > "$HEALTH_FILE.tmp" && mv "$HEALTH_FILE.tmp" "$HEALTH_FILE"
}

cmd_health() {
    _bd_init
    [ -r "$HEALTH_FILE" ] || { echo "brain-dirty: never polled ($HEALTH_FILE absent)" >&2; return 1; }
    local st t iv age
    read -r st t iv < "$HEALTH_FILE"
    case "$t" in ''|*[!0-9]*) t=0 ;; esac
    case "$iv" in ''|*[!0-9]*) iv=600 ;; esac
    age=$(( $(date +%s) - t ))
    [ "$age" -le $(( 3 * iv )) ] || { echo "brain-dirty: last poll ${age}s ago (interval ${iv}s) — hung or dead" >&2; return 1; }
}

cmd_watch() {
    local interval=600 ticks="" tick=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --interval) interval="${2:?--interval needs a value}"; shift 2 ;;
            --ticks)    ticks="${2:?--ticks needs a value}"; shift 2 ;;
            *) echo "brain-dirty.sh: watch: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    _bd_init
    while :; do
        cmd_pass
        _bd_write_health "$interval"
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
        *) echo "usage: brain-dirty.sh pass | watch [--interval S] [--ticks N] | health" >&2; exit 2 ;;
    esac
fi
