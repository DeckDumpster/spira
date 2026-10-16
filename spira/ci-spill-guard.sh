#!/usr/bin/env bash
#
# ci-spill-guard.sh — every CI workflow that provisions a machine must spill to EC2 only.
#
#   ci-spill-guard.sh scan <tree>                       print violations; exit 1 on any
#   ci-spill-guard.sh --gate <tree>                     scan as a gate fence (prints `fence: ...`)
#   ci-spill-guard.sh pass                              scan every registered repo's landing ref
#   ci-spill-guard.sh watch [--interval S] [--ticks N]  watchd daemon: tell the inbox once per violation
#   ci-spill-guard.sh health                            non-zero when the loop stopped polling
#
# A violation is a `.../provision@<ref>` step whose `spill:` input is absent or is anything but
# `ec2-only`. The report names file:line and the override that fixes it.

# covers: spira/ci-spill-guard.sh spira/watchers spira/lib.sh .github/workflows/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"

_csg_file() {   # _csg_file <label> <file> -> violations, then "COUNT <provision steps>"
    awk -v file="$1" '
        function ind(s) { match(s, /^ */); return RLENGTH }
        function flush() {
            if (!inprov) return
            n++
            if (!found) print file ":" ln ": provision has no spill: input; override: spill: ec2-only"
            else if (val != "ec2-only") print file ":" sline ": provision spill: " val "; override: spill: ec2-only"
            inprov = 0
        }
        /^[ ]*-[ ]/ && inprov && ind($0) <= sidx { flush() }
        /^[ ]*-[ ]/ { cur = ind($0) }
        /^[^ #]/ { flush() }
        /uses:[ ]*[^ #]*\/provision@/ { flush(); inprov = 1; found = 0; ln = NR; sidx = cur; next }
        inprov && /^[ ]*spill:/ {
            v = $0; sub(/^[ ]*spill:[ ]*/, "", v); sub(/[ ]+#.*$/, "", v); gsub(/^["\047]|["\047]$/, "", v)
            found = 1; val = v; sline = NR
        }
        END { flush(); print "COUNT " n+0 }
    ' "$2"
}

_csg_tree() {   # _csg_tree <tree> [prefix] -> violations, then "COUNT <n>"; exit 1 on violations
    local tree="$1" prefix="${2:-}" f line checked=0 rc=0
    for f in "$tree"/.github/workflows/*.yml "$tree"/.github/workflows/*.yaml; do
        [ -f "$f" ] || continue
        while IFS= read -r line; do
            case "$line" in
                "COUNT "*) checked=$(( checked + ${line#COUNT } )) ;;
                '') ;;
                *) printf '%s\n' "$line"; rc=1 ;;
            esac
        done < <(_csg_file "$prefix${f#"$tree"/}" "$f")
    done
    echo "COUNT $checked"
    return $rc
}

cmd_scan() {
    local tree="${1:?scan needs a tree}" gate="${2:-}" out rc=0 n
    out="$(_csg_tree "$tree")" || rc=1
    n="$(printf '%s\n' "$out" | sed -n 's/^COUNT //p')"
    printf '%s\n' "$out" | grep -v '^COUNT '
    if [ -n "$gate" ]; then
        [ "${n:-0}" -gt 0 ] || { echo "ci-spill-guard: no provision step found under $tree; a scan that finds nothing proves nothing" >&2; return 1; }
        echo "fence: ci-spill-guard checked $n provision steps"
    fi
    return $rc
}

HEALTH_FILE=""
_csg_init() {
    . "$HERE/lib.sh"
    HEALTH_FILE="$SPIRA_RUN/watchd/ci-spill-guard.health"
}

_csg_once() {   # _csg_once <key> <message>
    local mark="$SPIRA_RUN/watchd/ci-spill-guard.said"
    grep -qxF -- "$1" "$mark" 2>/dev/null && return 0
    printf '%s %s\n' "$(date -u +%FT%TZ)" "$2"
    { mkdir -p "$(dirname "$SPIRA_CONCIERGE_INBOX")" \
        && printf '%s [watch:ci-spill-guard] %s\n' "$(date -u +%FT%TZ)" "$2" >> "$SPIRA_CONCIERGE_INBOX"; } 2>/dev/null
    mkdir -p "$(dirname "$mark")" && printf '%s\n' "$1" >> "$mark"
}

cmd_pass() {
    _csg_init
    local name root ref tmp out line
    for name in $(repo_names); do
        root="$(repo_root "$name")" || continue
        ref="$(spira_landref "$name")" || { _csg_once "noref-$name" "CI SPILL: $name has no resolvable landing ref; its workflows are unchecked"; continue; }
        tmp="$(mktemp -d)" || continue
        if git -C "$root" archive "$ref" .github/workflows 2>/dev/null | tar -x -C "$tmp" 2>/dev/null; then
            out="$(_csg_tree "$tmp" "$name:")"
            while IFS= read -r line; do
                case "$line" in COUNT*|'') ;; *) _csg_once "$line" "CI SPILL: law-buy-capacity-rather-than-wait: $line (at $ref)" ;; esac
            done <<< "$out"
        fi
        rm -rf "$tmp"
    done
}

_csg_write_health() {
    mkdir -p "$(dirname "$HEALTH_FILE")"
    printf 'ok %s %s\n' "$(date +%s)" "$1" > "$HEALTH_FILE.tmp" && mv "$HEALTH_FILE.tmp" "$HEALTH_FILE"
}

cmd_health() {
    _csg_init
    [ -r "$HEALTH_FILE" ] || { echo "ci-spill-guard: never polled ($HEALTH_FILE absent)" >&2; return 1; }
    local st t iv age
    read -r st t iv < "$HEALTH_FILE"
    case "$t" in ''|*[!0-9]*) t=0 ;; esac
    case "$iv" in ''|*[!0-9]*) iv=900 ;; esac
    age=$(( $(date +%s) - t ))
    [ "$age" -le $(( 3 * iv )) ] || { echo "ci-spill-guard: last poll ${age}s ago (interval ${iv}s) — hung or dead" >&2; return 1; }
}

cmd_watch() {
    local interval=900 ticks="" tick=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --interval) interval="${2:?--interval needs a value}"; shift 2 ;;
            --ticks)    ticks="${2:?--ticks needs a value}"; shift 2 ;;
            *) echo "ci-spill-guard.sh: watch: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    _csg_init
    while :; do
        cmd_pass
        _csg_write_health "$interval"
        tick=$((tick + 1))
        [ -n "$ticks" ] && [ "$tick" -ge "$ticks" ] && return 0
        sleep "$interval"
    done
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        scan)   shift; cmd_scan "$@"; exit $? ;;
        --gate) shift; cmd_scan "${1:-.}" gate; exit $? ;;
        pass)   cmd_pass ;;
        watch)  shift; cmd_watch "$@" ;;
        health) cmd_health; exit $? ;;
        *) echo "usage: ci-spill-guard.sh scan <tree> | --gate <tree> | pass | watch [--interval S] [--ticks N] | health" >&2; exit 2 ;;
    esac
fi
