#!/usr/bin/env bash
#
# test-escape-census-units.sh — the weekly escape census runs escape-classify.sh census from
# a weekly timer, and the manifest installs and enables both units.
#
# tier: T0
# covers: systemd/spira-escape-census.service systemd/spira-escape-census.timer install/src/manifest.rs spira/escape-classify.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
UNIT_DIR="$HERE/../systemd"
SVC="$UNIT_DIR/spira-escape-census.service"
TMR="$UNIT_DIR/spira-escape-census.timer"

echo "=== service ==="
[ -r "$SVC" ] && ok "service readable" || bad "service readable" "not found at $SVC"
execstart="$(grep '^ExecStart=' "$SVC" 2>/dev/null | head -1)"
want "ExecStart runs escape-classify.sh" "escape-classify.sh" "$execstart"
want "ExecStart runs the census subcommand" " census" "$execstart"
want "output goes to a log" "escape-census.log" "$(grep '^StandardOutput=' "$SVC" 2>/dev/null)"

echo "=== timer ==="
[ -r "$TMR" ] && ok "timer readable" || bad "timer readable" "not found at $TMR"
want "timer is calendar-driven and weekly (Mon)" "OnCalendar=Mon " "$(grep '^OnCalendar=' "$TMR" 2>/dev/null)"
want "timer survives downtime" "Persistent=true" "$(grep '^Persistent=' "$TMR" 2>/dev/null)"
want "timer names the census service" "spira-escape-census.service" "$(grep '^Unit=' "$TMR" 2>/dev/null)"
if command -v systemd-analyze >/dev/null 2>&1; then
    next="$(systemd-analyze calendar "$(grep '^OnCalendar=' "$TMR" | cut -d= -f2-)" 2>&1)"
    want "the OnCalendar expression parses to a next elapse" "Next elapse" "$next"
fi

echo "=== manifest ==="
union_out="$(SPIRA_HOME="$UNIT_DIR/../spira" units-install --list-union 2>&1)"
units_block="$(printf '%s\n' "$union_out" | grep '^UNITS ')"
enable_block="$(printf '%s\n' "$union_out" | grep '^ENABLE ')"
want "positive control: units block is parseable" "spira-sentinel.timer" "$units_block"
want "positive control: enable block is parseable" "spira-sentinel.timer" "$enable_block"
want "service is in UNITS" "spira-escape-census.service" "$units_block"
want "timer is in UNITS" "spira-escape-census.timer" "$units_block"
want "timer is enabled" "spira-escape-census.timer" "$enable_block"

tl_summary
