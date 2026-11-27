#!/usr/bin/env bash
#
# ci-spill-guard.sh — every CI workflow that provisions a machine must spill to EC2 only.
#
#   ci-spill-guard.sh scan <tree>      print violations; exit 1 on any
#   ci-spill-guard.sh --gate <tree>    scan as a gate fence (prints `fence: ...`)
#
# A violation is a `.../provision@<ref>` step whose `spill:` input is absent or is anything but
# `ec2-only`. The report names file:line and the override that fixes it.

# covers: spira/ci-spill-guard.sh .github/workflows/*
set -uo pipefail

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

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        scan)   shift; cmd_scan "$@"; exit $? ;;
        --gate) shift; cmd_scan "${1:-.}" gate; exit $? ;;
        *) echo "usage: ci-spill-guard.sh scan <tree> | --gate <tree>" >&2; exit 2 ;;
    esac
fi
