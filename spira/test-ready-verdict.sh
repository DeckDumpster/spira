#!/usr/bin/env bash
#
# test-ready-verdict.sh — ready.sh's timers check and its exit rule, run in-process from
# ready-lib.sh. systemctl is a shell function answering from fixed tables, so no case waits on
# a timer, a port or a process: the timers check reads nothing but its unit directory and "$SC".
#
# tier: T1
# covers: spira/ready.sh spira/ready-lib.sh UC-cockpit-observability-35
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/ready-lib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
UNITS="$TMP/units"; mkdir -p "$UNITS"

# The units is-active answers yes for, and each unit's two NextElapse properties; a unit absent
# from a table reports the property empty, as systemd does for a timer with no such trigger.
ACTIVE=""
declare -A NEXT_R=() NEXT_M=()
fake_sc() {
    local unit="${*: -1}"
    case "$*" in
        *" is-active "*) [[ " $ACTIVE " == *" $unit "* ]] ;;
        *" NextElapseUSecRealtime "*) printf '%s\n' "${NEXT_R[$unit]:-}" ;;
        *" NextElapseUSecMonotonic "*) printf '%s\n' "${NEXT_M[$unit]:-}" ;;
        *) return 1 ;;
    esac
}
SC=fake_sc

# fixture <timer>... — exactly these timer files in the unit directory, every table empty.
fixture() {
    rm -f "$UNITS"/*
    local t; for t in "$@"; do : > "$UNITS/$t"; done
    ACTIVE=""; NEXT_R=(); NEXT_M=()
}

# ready <instance> — the timers rows for that instance, then the verdict and its status.
ready() {
    pass=0; warn=0; fail=0; unkn=0; skip=0
    ready_timers "$UNITS" "$1"
    ready_verdict
    printf 'rc=%s\n' "$?"
}

T=spira-mail-tidy-blue.timer

echo "--- an active timer with no next trigger has stopped scheduling ---"
fixture "$T"; ACTIVE="$T"
out="$(ready blue)"
want "no next trigger: FAIL names the unit" "  FAIL  $T is active but has stopped scheduling" "$out"
want "no next trigger: says how to restart it" "systemctl --user restart $T" "$out"
want "no next trigger: not ready, exit 1" "rc=1" "$out"

fixture "$T"; ACTIVE="$T"; NEXT_R[$T]=0; NEXT_M[$T]=0
out="$(ready blue)"
want "both triggers 0: FAIL, as for unset" "  FAIL  $T is active but has stopped scheduling" "$out"

echo "--- either next trigger is enough ---"
fixture "$T"; ACTIVE="$T"; NEXT_R[$T]=1730000000000000
out="$(ready blue)"
want "a realtime trigger: still scheduled" "  pass  $T still scheduled" "$out"
want "a realtime trigger: ready, exit 0" "rc=0" "$out"

fixture "$T"; ACTIVE="$T"; NEXT_R[$T]=0; NEXT_M[$T]=123456789
out="$(ready blue)"
want "a monotonic trigger alone: still scheduled" "  pass  $T still scheduled" "$out"
nowant "a monotonic trigger alone: no FAIL" "FAIL" "$out"

echo "--- which timers are checked ---"
fixture "$T" spira-idle-blue.timer; ACTIVE="$T"; NEXT_M[$T]=1
out="$(ready blue)"
nowant "an inactive timer is not checked" "spira-idle-blue.timer" "$out"
want "an inactive timer is not checked: the active one still is" "  pass  $T still scheduled" "$out"

fixture spira-mail-tidy-prod.timer; ACTIVE="spira-mail-tidy-prod.timer"
out="$(ready blue)"
nowant "another instance's timer is not checked" "spira-mail-tidy-prod.timer" "$out"
want "no timer of this instance: the row is a skip" "  skip  timers — no installed spira-*-blue.timer found" "$out"
want "a skip alone is ready" "rc=0" "$out"

fixture spira-idle-blue.timer
out="$(ready blue)"
want "only inactive timers: the row is a skip" "  skip  timers — no installed spira-*-blue.timer found" "$out"

echo "--- the exit rule: any FAIL or ? is not ready; WARN and skip are ---"
out="$(pass=0 warn=1 fail=0 unkn=0 skip=0; ready_verdict; printf 'rc=%s\n' "$?")"
want "a WARN alone: armed" "armed: loop is ready to receive work" "$out"
want "a WARN alone: exit 0" "rc=0" "$out"

out="$(pass=3 warn=0 fail=0 unkn=1 skip=0; ready_verdict; printf 'rc=%s\n' "$?")"
want "a ? alone: NOT READY" "1 unknown, 0 skip — NOT READY" "$out"
want "a ? alone: exit 1" "rc=1" "$out"

out="$(pass=3 warn=2 fail=1 unkn=0 skip=1; ready_verdict; printf 'rc=%s\n' "$?")"
want "a FAIL among passes: counts every row" "3 pass, 2 WARN, 1 FAIL, 0 unknown, 1 skip" "$out"
want "a FAIL among passes: exit 1" "rc=1" "$out"

echo "--- ready.sh runs these, not a copy ---"
src="$(cat "$HERE/ready.sh")"
want "ready.sh sources ready-lib.sh" '/ready-lib.sh" || exit 1' "$src"
want "ready.sh runs the timers check on the instance's unit directory" \
     'ready_timers "$HOME/.config/systemd/user" "${SPIRA_INSTANCE:-prod}"' "$src"
want "ready.sh exits with the verdict's status" $'ready_verdict\nexit $?' "$src"
nowant "ready.sh keeps no rows of its own" 'PASS() {' "$src"

tl_summary
