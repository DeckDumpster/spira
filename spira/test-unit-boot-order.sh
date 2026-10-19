#!/usr/bin/env bash
#
# test-unit-boot-order.sh — no unit that default.target pulls in orders itself After=default.target.
# default.target starts after everything it wants, so that pair is an ordering cycle and systemd
# breaks it by deleting a start job at boot.
#
# tier: T0
# covers: systemd/*.service systemd/*.timer systemd/*.socket
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
UNIT_DIR="$HERE/../systemd"

offenders() {
    local dir="$1" f
    for f in "$dir"/*; do
        grep -q '^WantedBy=.*default\.target' "$f" 2>/dev/null || continue
        grep -q '^\(After\|Before\)=.*default\.target' "$f" && echo "$(basename "$f")"
    done
}

echo "=== positive control ==="
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
printf '[Unit]\nAfter=default.target\n[Install]\nWantedBy=default.target\n' > "$T/bad.service"
printf '[Unit]\nAfter=x.service\n[Install]\nWantedBy=default.target\n' > "$T/good.service"
is "the matcher flags a planted cycle" "bad.service" "$(offenders "$T")"

echo "=== shipped units ==="
is "no wanted-by-default.target unit orders itself against default.target" "" "$(offenders "$UNIT_DIR")"
nowant "dolt-beads does not order after default.target" "default.target" "$(grep '^After=' "$UNIT_DIR/dolt-beads.service")"
want "dolt-persist orders after dolt-beads" "dolt-beads.service" "$(grep '^After=' "$UNIT_DIR/dolt-persist.service")"
want "lc-serve orders after dolt-beads" "dolt-beads.service" "$(grep '^After=' "$UNIT_DIR/lc-serve.service")"

tl_summary
