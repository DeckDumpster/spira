#!/usr/bin/env bash
#
# test-idea-prompt-units.sh — the idea prompt runs from a five-minute timer under a CPU fence,
# and the manifest installs and enables both units.
#
# tier: T0
# covers: systemd/spira-idea-prompt.service systemd/spira-idea-prompt.timer install/src/manifest.rs idea-prompt/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
UNIT_DIR="$HERE/../systemd"
SVC="$UNIT_DIR/spira-idea-prompt.service"
TMR="$UNIT_DIR/spira-idea-prompt.timer"

want "ExecStart runs idea-prompt pass" "bin/idea-prompt pass" "$(grep '^ExecStart=' "$SVC" 2>/dev/null)"
want "the service is fenced" "CPUQuota=" "$(grep '^CPUQuota=' "$SVC" 2>/dev/null)"
want "the service is a oneshot" "Type=oneshot" "$(grep '^Type=' "$SVC" 2>/dev/null)"
want "the timer is interval-driven" "OnUnitActiveSec=5min" "$(grep '^OnUnitActiveSec=' "$TMR" 2>/dev/null)"
want "the timer survives downtime" "Persistent=true" "$(grep '^Persistent=' "$TMR" 2>/dev/null)"
want "the timer names the service" "spira-idea-prompt.service" "$(grep '^Unit=' "$TMR" 2>/dev/null)"

union_out="$(SPIRA_HOME="$UNIT_DIR/../spira" units-install --list-union 2>&1)"
units_block="$(printf '%s\n' "$union_out" | grep '^UNITS ')"
enable_block="$(printf '%s\n' "$union_out" | grep '^ENABLE ')"
want "positive control: units block is parseable" "spira-sentinel.timer" "$units_block"
want "service is in UNITS" "spira-idea-prompt.service" "$units_block"
want "timer is in UNITS" "spira-idea-prompt.timer" "$units_block"
want "timer is enabled" "spira-idea-prompt.timer" "$enable_block"

tl_summary
