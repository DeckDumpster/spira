#!/usr/bin/env bash
#
# test-filings-collect-units.sh — the filings collector runs from a timer under a CPU fence, a
# failed pass fails the unit, and the manifest installs and enables both units.
#
# tier: T0
# covers: systemd/spira-filings-collect.service systemd/spira-filings-collect.timer install/src/manifest.rs filings-collect/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
UNIT_DIR="$HERE/../systemd"
SVC="$UNIT_DIR/spira-filings-collect.service"
TMR="$UNIT_DIR/spira-filings-collect.timer"

want "ExecStart runs filings-collect pass" "bin/filings-collect pass" "$(grep '^ExecStart=' "$SVC" 2>/dev/null)"
want "the service is fenced" "CPUQuota=" "$(grep '^CPUQuota=' "$SVC" 2>/dev/null)"
want "the service is a oneshot" "Type=oneshot" "$(grep '^Type=' "$SVC" 2>/dev/null)"
nowant "an alarming pass is not hidden as success" "SuccessExitStatus" "$(cat "$SVC")"
want "the timer is interval-driven" "OnUnitActiveSec=10min" "$(grep '^OnUnitActiveSec=' "$TMR" 2>/dev/null)"
want "the timer names the service" "spira-filings-collect.service" "$(grep '^Unit=' "$TMR" 2>/dev/null)"

union_out="$(SPIRA_HOME="$UNIT_DIR/../spira" units-install --list-union 2>&1)"
units_block="$(printf '%s\n' "$union_out" | grep '^UNITS ')"
enable_block="$(printf '%s\n' "$union_out" | grep '^ENABLE ')"
want "positive control: units block is parseable" "spira-sentinel.timer" "$units_block"
want "service is in UNITS" "spira-filings-collect.service" "$units_block"
want "timer is in UNITS" "spira-filings-collect.timer" "$units_block"
want "timer is enabled" "spira-filings-collect.timer" "$enable_block"

tl_summary
