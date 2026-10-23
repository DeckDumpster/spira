#!/usr/bin/env bash
#
# design-asks.sh — a design page in draft or in-review with no open ask naming it, as a watchd daemon.
#
#   design-asks.sh pass                              one check
#   design-asks.sh watch [--interval S] [--ticks N]  loop forever (or N times)
#   design-asks.sh health                            non-zero when the loop stopped polling or the last pass failed
#
# Scans $SPIRA_WIKI/wiki/projects/*/designs/*.md for frontmatter status draft or in-review. A page
# is covered when an open SPIRA_ASK_LABEL bead, or the mail of a held ask, names its path. Each
# uncovered page is told to the Concierge inbox as one line, again every SPIRA_DESIGN_ASKS_REPEAT
# seconds until an ask exists or the status changes. A pass that cannot read the brain or the
# store says so and fails health; it never reports "no designs".
#
# covers: spira/design-asks.sh spira/watchers spira/conf.d/SPIRA_DESIGN_ASKS_REPEAT
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

HEALTH_FILE="$SPIRA_RUN/watchd/design-asks.health"
STATE="$SPIRA_RUN/watchd/design-asks.state"

_da_say() {
    printf '%s %s\n' "$(date -u +%FT%TZ)" "$1"
    { mkdir -p "$(dirname "$SPIRA_CONCIERGE_INBOX")" \
        && printf '%s [watch:design-asks] %s\n' "$(date -u +%FT%TZ)" "$1" >> "$SPIRA_CONCIERGE_INBOX"; } 2>/dev/null
    return 0
}

_da_fail() {
    grep -qxF -- "$1" "$STATE.failed" 2>/dev/null \
        || _da_say "DESIGN-ASKS FAILED: $1; designs without an ask cannot be listed"
    mkdir -p "$(dirname "$STATE")" && printf '%s\n' "$1" > "$STATE.failed"
    _da_write_health fail "${_da_interval:-600}"
    return 1
}

_da_field() {   # _da_field <file> <key>: a top-level frontmatter value, unquoted
    awk -v k="$2" '
        NR == 1 { if ($0 != "---") exit; next }
        $0 == "---" { exit }
        index($0, k ":") == 1 { v = substr($0, length(k) + 2); gsub(/^[ \t"\047]+|[ \t"\047\r]+$/, "", v); print v; exit }
    ' "$1"
}

_da_asks_text() {   # every open ask body, then the mail of every held ask; non-zero when the store cannot be read
    local json id
    json="$(timeout 10 "${SPIRA_BD:-bd}" -C "$SPIRA_DB" list --label "$SPIRA_ASK_LABEL" --status open --limit 0 --json 2>/dev/null)" || return 1
    case "$json" in "["*) printf '%s\n' "$json" ;; *) return 1 ;; esac
    while read -r id; do
        [ -n "$id" ] || continue
        grep -lsx "X-Spira-Work-Bead: $id" "$SPIRA_MAIL"/*/new/* "$SPIRA_MAIL"/*/cur/* 2>/dev/null | xargs -r cat
    done < <(timeout 10 spira-lc list-held ask 2>/dev/null)
    return 0
}

cmd_pass() {
    local now repeat brain f rel status text writer first last prev=""
    now="$(date +%s)"; repeat="${SPIRA_DESIGN_ASKS_REPEAT:-86400}"
    [ -n "${SPIRA_WIKI:-}" ] || { _da_fail "SPIRA_WIKI is not set"; return 1; }
    brain="$SPIRA_WIKI/wiki/projects"
    [ -d "$brain" ] || { _da_fail "$brain is not a directory"; return 1; }
    text="$(_da_asks_text)" || { _da_fail "the ask store ($SPIRA_DB) could not be read"; return 1; }

    rm -f "$STATE.failed"
    local -A since=() said=()
    if [ -r "$STATE" ]; then
        while read -r f first last; do since[$f]="$first"; said[$f]="$last"; done < "$STATE"
    fi
    local new=""
    shopt -s nullglob
    for f in "$brain"/*/designs/*.md; do
        rel="wiki/projects/${f#"$brain"/}"
        status="$(_da_field "$f" status)"
        case "$status" in draft|in-review) ;; *) continue ;; esac
        [[ "$text" == *"$rel"* ]] && continue
        first="${since[$rel]:-$now}"; last="${said[$rel]:-0}"
        if [ $(( now - last )) -ge "$repeat" ]; then
            writer="$(_da_field "$f" bead)"
            [ -n "$writer" ] || writer="$(_da_field "$f" session)"
            [ -n "$writer" ] || writer="$(_da_field "$f" author)"
            [ -n "$writer" ] || writer="$(git -C "$SPIRA_WIKI" log -1 --format=%s -- "$rel" 2>/dev/null)"
            _da_say "DESIGN WITHOUT ASK: $rel is $status and has had no open ask for $(( (now - first) / 3600 ))h (written by ${writer:-unknown}); post one with work ask --class design"
            last="$now"
        fi
        new+="$rel $first $last"$'\n'
    done
    shopt -u nullglob
    mkdir -p "$(dirname "$STATE")" && printf '%s' "$new" > "$STATE.tmp" && mv "$STATE.tmp" "$STATE"
}

_da_write_health() {   # _da_write_health <ok|fail> <interval>
    mkdir -p "$(dirname "$HEALTH_FILE")"
    printf '%s %s %s\n' "$1" "$(date +%s)" "$2" > "$HEALTH_FILE.tmp" && mv "$HEALTH_FILE.tmp" "$HEALTH_FILE"
}

cmd_health() {
    [ -r "$HEALTH_FILE" ] || { echo "design-asks: never polled ($HEALTH_FILE absent)" >&2; return 1; }
    local st t iv age
    read -r st t iv < "$HEALTH_FILE"
    case "$t" in ''|*[!0-9]*) t=0 ;; esac
    case "$iv" in ''|*[!0-9]*) iv=600 ;; esac
    [ "$st" = ok ] || { echo "design-asks: the last pass failed" >&2; return 1; }
    age=$(( $(date +%s) - t ))
    [ "$age" -le $(( 3 * iv )) ] || { echo "design-asks: last poll ${age}s ago (interval ${iv}s) — hung or dead" >&2; return 1; }
}

cmd_watch() {
    local ticks="" tick=0
    _da_interval=600
    while [ $# -gt 0 ]; do
        case "$1" in
            --interval) _da_interval="${2:?--interval needs a value}"; shift 2 ;;
            --ticks)    ticks="${2:?--ticks needs a value}"; shift 2 ;;
            *) echo "design-asks.sh: watch: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    while :; do
        cmd_pass && _da_write_health ok "$_da_interval"
        tick=$((tick + 1))
        [ -n "$ticks" ] && [ "$tick" -ge "$ticks" ] && return 0
        sleep "$_da_interval"
    done
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        pass)   cmd_pass; exit $? ;;
        watch)  shift; cmd_watch "$@" ;;
        health) cmd_health; exit $? ;;
        *) echo "usage: design-asks.sh pass | watch [--interval S] [--ticks N] | health" >&2; exit 2 ;;
    esac
fi
