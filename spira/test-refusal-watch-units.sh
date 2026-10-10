#!/usr/bin/env bash
#
# test-refusal-watch-units.sh — the refusal watch runs from an hourly timer under a CPU fence,
# and the manifest installs and enables both units.
#
# tier: T0
# covers: systemd/spira-refusal-watch.service systemd/spira-refusal-watch.timer install/src/manifest.rs refusal-watch/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
UNIT_DIR="$HERE/../systemd"
SVC="$UNIT_DIR/spira-refusal-watch.service"
TMR="$UNIT_DIR/spira-refusal-watch.timer"

want "ExecStart runs refusal-watch pass" "bin/refusal-watch pass" "$(grep '^ExecStart=' "$SVC" 2>/dev/null)"
want "the service is fenced" "CPUQuota=" "$(grep '^CPUQuota=' "$SVC" 2>/dev/null)"
want "the service is a oneshot" "Type=oneshot" "$(grep '^Type=' "$SVC" 2>/dev/null)"
want "a failed filing is retried, not a red unit" "SuccessExitStatus=1" "$(grep '^SuccessExitStatus=' "$SVC" 2>/dev/null)"
want "the timer is interval-driven" "OnUnitActiveSec=1h" "$(grep '^OnUnitActiveSec=' "$TMR" 2>/dev/null)"
want "the timer survives downtime" "Persistent=true" "$(grep '^Persistent=' "$TMR" 2>/dev/null)"
want "the timer names the service" "spira-refusal-watch.service" "$(grep '^Unit=' "$TMR" 2>/dev/null)"

union_out="$(SPIRA_HOME="$UNIT_DIR/../spira" units-install --list-union 2>&1)"
units_block="$(printf '%s\n' "$union_out" | grep '^UNITS ')"
enable_block="$(printf '%s\n' "$union_out" | grep '^ENABLE ')"
want "positive control: units block is parseable" "spira-sentinel.timer" "$units_block"
want "service is in UNITS" "spira-refusal-watch.service" "$units_block"
want "timer is in UNITS" "spira-refusal-watch.timer" "$units_block"
want "timer is enabled" "spira-refusal-watch.timer" "$enable_block"

tl_summary
