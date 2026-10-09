#!/usr/bin/env bash
#
# test-event-continuity-units.sh — the lifecycle event-continuity check runs from a timer, its
# alarm is a watchd row on the log it writes, and the manifest installs and enables both units.
#
# tier: T0
# covers: systemd/spira-event-continuity.service systemd/spira-event-continuity.timer install/src/manifest.rs spira/watchers
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
UNIT_DIR="$HERE/../systemd"
SVC="$UNIT_DIR/spira-event-continuity.service"
TMR="$UNIT_DIR/spira-event-continuity.timer"

echo "=== service ==="
execstart="$(grep '^ExecStart=' "$SVC" 2>/dev/null | head -1)"
want "ExecStart runs spira-lc event-continuity" "spira-lc event-continuity" "$execstart"
want "output goes to the log the watcher row names" "event-continuity.log" "$(grep '^StandardOutput=' "$SVC" 2>/dev/null)"

echo "=== timer ==="
want "timer repeats from its last activation" "OnUnitActiveSec=" "$(grep '^OnUnitActiveSec=' "$TMR" 2>/dev/null)"
want "timer arms itself at boot" "OnBootSec=" "$(grep '^OnBootSec=' "$TMR" 2>/dev/null)"
want "timer names the service" "spira-event-continuity.service" "$(grep '^Unit=' "$TMR" 2>/dev/null)"

echo "=== watcher row ==="
want "a log row reads the service's log" "event-continuity|log|@SPIRA_RUN@/event-continuity.log" "$(grep '^event-continuity|' "$HERE/watchers" 2>/dev/null)"

echo "=== manifest ==="
union_out="$(SPIRA_HOME="$UNIT_DIR/../spira" units-install --list-union 2>&1)"
units_block="$(printf '%s\n' "$union_out" | grep '^UNITS ')"
enable_block="$(printf '%s\n' "$union_out" | grep '^ENABLE ')"
want "positive control: units block is parseable" "spira-sentinel.timer" "$units_block"
want "positive control: enable block is parseable" "spira-sentinel.timer" "$enable_block"
want "service is in UNITS" "spira-event-continuity.service" "$units_block"
want "timer is in UNITS" "spira-event-continuity.timer" "$units_block"
want "timer is enabled" "spira-event-continuity.timer" "$enable_block"

tl_summary
