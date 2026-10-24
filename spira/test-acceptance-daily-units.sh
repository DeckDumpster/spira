#!/usr/bin/env bash
#
# test-acceptance-daily-units.sh — the daily acceptance runs from a calendar timer under a CPU fence, a
# failed pass fails the unit, and the manifest installs and enables both units.
#
# tier: T0
# covers: spira/acceptance-local.sh systemd/spira-acceptance-daily.service systemd/spira-acceptance-daily.timer install/src/manifest.rs acceptance-daily/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
UNIT_DIR="$HERE/../systemd"
SVC="$UNIT_DIR/spira-acceptance-daily.service"
TMR="$UNIT_DIR/spira-acceptance-daily.timer"

want "ExecStart runs acceptance-daily pass" "bin/acceptance-daily pass" "$(grep '^ExecStart=' "$SVC" 2>/dev/null)"
want "the service is fenced" "CPUQuota=" "$(grep '^CPUQuota=' "$SVC" 2>/dev/null)"
want "the service is a oneshot" "Type=oneshot" "$(grep '^Type=' "$SVC" 2>/dev/null)"
nowant "an alarming pass is not hidden as success" "SuccessExitStatus" "$(cat "$SVC")"
want "the timer fires daily in the quiet hours" "OnCalendar=*-*-* 04:" "$(grep '^OnCalendar=' "$TMR" 2>/dev/null)"
want "a missed day runs on the next boot" "Persistent=true" "$(grep '^Persistent=' "$TMR" 2>/dev/null)"
want "the timer names the service" "spira-acceptance-daily.service" "$(grep '^Unit=' "$TMR" 2>/dev/null)"

union_out="$(SPIRA_HOME="$UNIT_DIR/../spira" units-install --list-union 2>&1)"
units_block="$(printf '%s\n' "$union_out" | grep '^UNITS ')"
enable_block="$(printf '%s\n' "$union_out" | grep '^ENABLE ')"
want "positive control: units block is parseable" "spira-sentinel.timer" "$units_block"
want "service is in UNITS" "spira-acceptance-daily.service" "$units_block"
want "timer is in UNITS" "spira-acceptance-daily.timer" "$units_block"
want "timer is enabled" "spira-acceptance-daily.timer" "$enable_block"

tl_summary
