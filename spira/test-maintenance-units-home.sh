#!/usr/bin/env bash
#
# test-maintenance-units-home.sh — a Plane=maintenance service whose bin/ tool refuses to run
# without SPIRA_HOME must declare it, or systemd runs it with no home and it exits 2 every time.
#
# tier: T0
# covers: systemd/*.service testenv/src/*.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
ROOT="$HERE/.."
UNIT_DIR="$ROOT/systemd"

needs_home() {
    local tool="$1" f
    for f in $(grep -rl 'SPIRA_HOME is not set' "$ROOT" --include='*.rs' --exclude-dir=target --exclude-dir=tests 2>/dev/null); do
        case "$f" in *"/${tool//-/_}"*|*"/$tool/"*|*"/${tool//-/_}_main.rs") return 0 ;; esac
    done
    return 1
}

checked=0
for svc in "$UNIT_DIR"/*.service; do
    grep -q '^Plane=maintenance' "$svc" || continue
    exec_line="$(grep '^ExecStart=' "$svc" | head -1)"
    tool="$(printf '%s' "$exec_line" | sed -n 's|.*/bin/\([A-Za-z0-9_-]*\).*|\1|p')"
    [ -n "$tool" ] || continue
    needs_home "$tool" || continue
    checked=$((checked + 1))
    want "$(basename "$svc") declares SPIRA_HOME for bin/$tool" "Environment=SPIRA_HOME=@SPIRA_HOME@" "$(cat "$svc")"
done

[ "$checked" -ge 1 ] && ok "matcher found a maintenance unit needing SPIRA_HOME (positive control)" \
    || bad "matcher found a maintenance unit needing SPIRA_HOME (positive control)" "none matched"

tl_summary
