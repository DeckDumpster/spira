#!/usr/bin/env bash
#
# test-capacity-probe.sh — while a capacity pause horizon is far out, the account
#   is probed on a bounded cadence; a successful probe lifts the pause early; a
#   refused probe keeps it.
#
#   ./test-capacity-probe.sh
#
# WHAT THIS TESTS. capacity_paused() trusted the reset time in the pause file for
# the whole duration, with no positive check that the account was still refusing.
# A pause written as "resets in 67 hours" held the loop dark for 67 hours even
# though the account began serving again 22 hours early. The fix probes when the
# pause horizon exceeds SPIRA_CAPACITY_PROBE_WINDOW; a probe that receives a
# response lifts the pause, a refused probe keeps it.
#
# FOUR CASES (law-absence-needs-a-positive-control):
#
#   0. POSITIVE CONTROL — no pause at all → capacity_paused returns 1 (not paused),
#      probe-last file is NOT written. Proves probe path cannot swallow open state.
#
#   1. PROBE SUCCEEDS — pause far in the future + stub agent exits 0 →
#      capacity_paused returns 1 (not paused), pause file is gone.
#
#   2. PROBE REFUSED — pause far in the future + stub agent exits 1 →
#      capacity_paused returns 0 (still paused), pause file remains.
#
#   3. CLOSE HORIZON — pause < SPIRA_CAPACITY_PROBE_WINDOW → probe NOT called,
#      capacity_paused returns 0 (still paused). Proves the window threshold gates
#      the probe correctly.
#
# SEEN-RED (run against unfixed tree 2026-09-15): cases 1 and 2 fail because
# capacity_paused never calls capacity_probe_maybe or capacity_probe at all.
#
# defect: sp-m00m
# covers: spira/lib.sh
# hermetic-ok: agent stubbed via SPIRA_AGENT; no database, no systemd, no network
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
trap 'rm -rf "$T"; exit 130' INT TERM
mkdir -p "$T/run" "$T/bin"

# Minimal environment. No real config, no real database, nothing ambient.
export SPIRA_RUN="$T/run"
export SPIRA_CONF="$T/no-such.conf"
export SPIRA_HOME="$T"
export SPIRA_DB="$T/no-db"
# Pin the probe window so the test does not depend on the shipped default.
export SPIRA_CAPACITY_PROBE_WINDOW=300   # 5 minutes; far-future pause (24h) exceeds this
export SPIRA_CAPACITY_PROBE_INTERVAL=0   # always probe when horizon is far out
export SPIRA_CAPACITY_PROBE_MODEL=test-model
# Explicit path for the probe-last file so the test controls it and set -u does not
# trip on the variable being unset before lib.sh (fixed version) defines it.
export SPIRA_CAPACITY_PROBE_LAST="$T/run/capacity-probe-last"

# shellcheck disable=SC1090
. "$HERE/lib.sh"

# Stubs that record calls and exit with a controlled code.
make_stub() {
    local name="$1" rc="$2"
    cat > "$T/bin/$name" <<STUB
#!/bin/sh
printf '%s\n' "\$*" >> "$T/probe-calls"
exit $rc
STUB
    chmod +x "$T/bin/$name"
}

echo "test-capacity-probe.sh"

# ======================================================================================
echo
echo "case 0 — positive control: no pause → capacity_paused returns 1, probe not called:"
# ======================================================================================
make_stub agent-serve 0
export SPIRA_AGENT="$T/bin/agent-serve"
rm -f "$SPIRA_CAPACITY_PAUSE" "$SPIRA_CAPACITY_PROBE_LAST" "$T/probe-calls"

capacity_paused; rc=$?
is "no pause: rc=1 (not paused)"   "1" "$rc"
is "no pause: probe NOT called"    "0" "$([ -f "$T/probe-calls" ] && echo 1 || echo 0)"

# ======================================================================================
echo
echo "case 1 — probe succeeds: far-future pause + stub exits 0 → pause lifted:"
# ======================================================================================
# Pause far in the future (24 h), horizon well above SPIRA_CAPACITY_PROBE_WINDOW.
# Stub exits 0 — simulates the account serving the probe.
# Afterwards capacity_paused must return 1 (not paused) and the pause file is gone.
FAR_FUTURE=$(( $(date +%s) + 86400 ))
make_stub agent-serve 0
export SPIRA_AGENT="$T/bin/agent-serve"
rm -f "$SPIRA_CAPACITY_PAUSE" "$SPIRA_CAPACITY_PROBE_LAST" "$T/probe-calls"
printf '%s %s probe-test\n' "$FAR_FUTURE" \
    "$(date -u -d "@$FAR_FUTURE" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)" \
    > "$SPIRA_CAPACITY_PAUSE"

capacity_paused; rc=$?
is "probe served: rc=1 (not paused)"           "1" "$rc"
is "probe served: pause file gone"             "0" "$([ -f "$SPIRA_CAPACITY_PAUSE" ] && echo 1 || echo 0)"
is "probe served: probe WAS called"            "1" "$([ -f "$T/probe-calls" ] && echo 1 || echo 0)"

# ======================================================================================
echo
echo "case 2 — probe refused: far-future pause + stub exits 1 → pause kept:"
# ======================================================================================
# Same fixture; stub exits 1 — simulates the account refusing the probe.
# Afterwards capacity_paused must return 0 (still paused) and the pause file remains.
make_stub agent-refuse 1
export SPIRA_AGENT="$T/bin/agent-refuse"
rm -f "$SPIRA_CAPACITY_PAUSE" "$SPIRA_CAPACITY_PROBE_LAST" "$T/probe-calls"
printf '%s %s probe-test\n' "$FAR_FUTURE" \
    "$(date -u -d "@$FAR_FUTURE" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)" \
    > "$SPIRA_CAPACITY_PAUSE"

capacity_paused; rc=$?
is "probe refused: rc=0 (still paused)"         "0" "$rc"
is "probe refused: pause file still present"    "1" "$([ -f "$SPIRA_CAPACITY_PAUSE" ] && echo 1 || echo 0)"
is "probe refused: probe WAS called"            "1" "$([ -f "$T/probe-calls" ] && echo 1 || echo 0)"

# ======================================================================================
echo
echo "case 3 — close horizon: pause within PROBE_WINDOW → probe NOT called:"
# ======================================================================================
# Pause horizon is within SPIRA_CAPACITY_PROBE_WINDOW (5 minutes above), so no probe.
# capacity_paused must still return 0 (still paused) and the pause file remains.
make_stub agent-serve 0
export SPIRA_AGENT="$T/bin/agent-serve"
CLOSE_FUTURE=$(( $(date +%s) + 120 ))   # 2 minutes — below the 5-minute window
rm -f "$SPIRA_CAPACITY_PAUSE" "$SPIRA_CAPACITY_PROBE_LAST" "$T/probe-calls"
printf '%s %s probe-test\n' "$CLOSE_FUTURE" \
    "$(date -u -d "@$CLOSE_FUTURE" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)" \
    > "$SPIRA_CAPACITY_PAUSE"

capacity_paused; rc=$?
is "close horizon: rc=0 (still paused)"         "0" "$rc"
is "close horizon: probe NOT called"            "0" "$([ -f "$T/probe-calls" ] && echo 1 || echo 0)"
is "close horizon: pause file still present"    "1" "$([ -f "$SPIRA_CAPACITY_PAUSE" ] && echo 1 || echo 0)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
