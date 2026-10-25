#!/usr/bin/env bash
#
# test-mend-watch-units.sh — the lifecycle mend-watch check runs from a timer, its
# alarm is a watchd row on the log it writes, and the manifest installs and enables both units.
#
# tier: T0
# covers: systemd/spira-mend-watch.service systemd/spira-mend-watch.timer install/src/manifest.rs spira/watchers
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
UNIT_DIR="$HERE/../systemd"
SVC="$UNIT_DIR/spira-mend-watch.service"
TMR="$UNIT_DIR/spira-mend-watch.timer"

echo "=== service ==="
execstart="$(grep '^ExecStart=' "$SVC" 2>/dev/null | head -1)"
want "ExecStart runs spira-lc mend-watch" "spira-lc mend-watch" "$execstart"
want "the same tick sweeps scoped pauses whose bead condition is met" "spira-lc pause-sweep" "$(grep "^ExecStartPost=" "$SVC" 2>/dev/null)"
want "output goes to the log the watcher row names" "mend-watch.log" "$(grep '^StandardOutput=' "$SVC" 2>/dev/null)"

echo "=== timer ==="
want "timer repeats from its last activation" "OnUnitActiveSec=" "$(grep '^OnUnitActiveSec=' "$TMR" 2>/dev/null)"
want "timer arms itself at boot" "OnBootSec=" "$(grep '^OnBootSec=' "$TMR" 2>/dev/null)"
want "timer names the service" "spira-mend-watch.service" "$(grep '^Unit=' "$TMR" 2>/dev/null)"

echo "=== watcher row ==="
want "a log row reads the service's log" "mend-watch|log|@SPIRA_RUN@/mend-watch.log" "$(grep '^mend-watch|' "$HERE/watchers" 2>/dev/null)"

echo "=== manifest ==="
union_out="$(SPIRA_HOME="$UNIT_DIR/../spira" units-install --list-union 2>&1)"
units_block="$(printf '%s\n' "$union_out" | grep '^UNITS ')"
enable_block="$(printf '%s\n' "$union_out" | grep '^ENABLE ')"
want "positive control: units block is parseable" "spira-sentinel.timer" "$units_block"
want "positive control: enable block is parseable" "spira-sentinel.timer" "$enable_block"
want "service is in UNITS" "spira-mend-watch.service" "$units_block"
want "timer is in UNITS" "spira-mend-watch.timer" "$units_block"
want "timer is enabled" "spira-mend-watch.timer" "$enable_block"

tl_summary
