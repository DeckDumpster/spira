#!/usr/bin/env bash
#
# timeout-lint.sh — refuses a bare numeric timeout/wall literal in the three programs that
# summon, watch and land an aeon.
#
#   timeout-lint.sh              lint the default file set, exit 0/1
#   timeout-lint.sh --scan FILE  lint one file, exit 0/1
#   timeout-lint.sh FILE...      lint the given files, exit 0/1
#
# WHAT THIS CATCHES. `timeout <N>`, `sleep <N>` where N is two or more bare digits, and
# `TimeoutStartSec=<N>` / `RuntimeMaxSec=<N>` — the shapes a wall or a timeout actually takes
# in this codebase. A one-digit `sleep` is exempt: every instance of it here is a poll tick
# inside a loop bounded by a NAMED wall (`while [ "$waited" -lt "$grace" ]; do sleep 1`), not
# a policy an operator would want to change, and flagging it would only teach the next reader
# to ignore this tool.
#
# WHY THESE THREE FILES. aeon.sh, sentinel.sh and landing.sh summon, watch and land an aeon;
# sp-eibeu found one of their walls stated twice, in two files, disagreeing. A wall spelled
# `${FAYTH_X:-N}` or `${SPIRA_X:-N}` names the key an operator would change and carries its
# default in the one place code reads it; a bare `N` does neither.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"

MATCH='\b(timeout|sleep)[[:space:]]+[0-9]{2,}\b|TimeoutStartSec=[0-9]+|RuntimeMaxSec=[0-9]+'

scan_file() {   # scan_file <path> -> violation lines on stdout, "path:line:text"
    local f="$1"
    [ -r "$f" ] || return 0
    grep -nE "$MATCH" "$f" 2>/dev/null | sed "s|^|$f:|"
}

if [ "${1:-}" = "--scan" ]; then
    [ -n "${2:-}" ] || { echo "usage: timeout-lint.sh --scan <file>" >&2; exit 2; }
    scan_file "$2"
    exit 0
fi

if [ "$#" -gt 0 ]; then
    files=("$@")
else
    files=("$HERE/aeon.sh" "$HERE/sentinel.sh" "$HERE/landing.sh")
fi

out=""
for f in "${files[@]}"; do
    hit="$(scan_file "$f")"
    [ -n "$hit" ] && out="${out}${out:+$'\n'}${hit}"
done

if [ -n "$out" ]; then
    printf 'timeout-lint: bare numeric timeout/wall literal(s) found — name the key in conf.sh instead:\n' >&2
    printf '%s\n' "$out" >&2
    exit 1
fi
printf 'timeout-lint: clean (%d file(s) checked)\n' "${#files[@]}"
exit 0
