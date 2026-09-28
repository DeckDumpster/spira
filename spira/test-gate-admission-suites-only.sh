#!/usr/bin/env bash
#
# test-gate-admission-suites-only.sh — gate.sh's host-wide admission semaphore is paid
# for only by a trial that actually runs suites.
#
# THE DEFECT (sp-dv6ae). Every gate.sh run — including landing.sh's queue-mode fences pass
# (SPIRA_GATE_SUITES=off: bash -n, exclude.sh, skew.sh and the repository's fence commands,
# never a suite) — waited for the same admission semaphore SPIRA_CERTIFY_PAR sizes for real
# suite runs. A fences-only trial touches none of the CPU/memory the semaphore protects, so
# charging it a slot queued cheap work behind expensive work for a resource it never used.
#
# CASE 1 is the positive control: with the lone admission slot held externally and
# SPIRA_GATE_SUITES left at its default (on), gate.sh must still wait for it and time out —
# proving this fixture actually exercises the semaphore, not that it is vacuously bypassed
# everywhere (law-absence-needs-a-positive-control).
# CASE 2 is the fix: the same held slot, with SPIRA_GATE_SUITES=off, must not be waited on
# at all — gate.sh passes immediately.
#
# tier: T1
# covers: spira/gate.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testlib/gate-fixture.sh"

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
gate_fixture_init "$TMP"

BR=spira/sp-adm1
gate_fixture_branch "$BR"

echo "test-gate-admission-suites-only.sh"

# Hold the one admission slot externally for the duration of both cases below, via a
# background flock holder — a real lock contender, not a stand-in for one
# (law-prefer-the-real-dependency).
mkdir -p "$RUN/gate-admission"
(
    exec 8>"$RUN/gate-admission/slot.1.lock"
    flock 8
    sleep 20
) &
HOLDER_PID=$!
trap 'kill "$HOLDER_PID" 2>/dev/null; wait "$HOLDER_PID" 2>/dev/null; rm -rf "$TMP"' EXIT

# Give the holder a moment to actually acquire the lock before either trial starts. Each
# poll is its own fresh, uncontended `flock -n` attempt against the path — it acquires,
# runs `true`, and releases immediately when the lock is free, so it never disturbs the
# holder once the holder has it.
_waited=0
while flock -n "$RUN/gate-admission/slot.1.lock" true 2>/dev/null; do
    sleep 0.1; _waited=$((_waited + 1))
    [ "$_waited" -ge 30 ] && break
done
if flock -n "$RUN/gate-admission/slot.1.lock" true 2>/dev/null; then
    bad "prerequisite: could not observe the external holder taking the admission slot" ""
fi

# --------------------------------------------------------------------------------------
# CASE 1 — POSITIVE CONTROL. SPIRA_GATE_SUITES=on (default): the slot is genuinely
# contended, so gate.sh must wait and, since the wait is far shorter than the holder's
# 20s sleep, time out with NO_VERDICT rather than judge anything.
# --------------------------------------------------------------------------------------
t0=$(date +%s)
out1="$(gate_fixture_run "$BR" repo \
    SPIRA_CERTIFY_PAR=1 SPIRA_GATE_LOCK_WAIT=6)"
rc1=$?
t1=$(date +%s)
is   "suites=on: waits for the held slot and reports NO_VERDICT"      75 "$rc1"
want "suites=on: names the admission timeout"                        "reason=admission-timeout" "$out1"
[ $(( t1 - t0 )) -ge 6 ] && ok "suites=on: actually waited (did not skip the semaphore)" \
    || bad "suites=on: actually waited (did not skip the semaphore)" "returned in $((t1 - t0))s"

# --------------------------------------------------------------------------------------
# CASE 2 — THE FIX. SPIRA_GATE_SUITES=off: the same held slot must not be waited on at
# all. A short SPIRA_GATE_LOCK_WAIT that would time out CASE 1 in 2s must not matter here —
# gate.sh should pass well within it because it never touches the semaphore.
# --------------------------------------------------------------------------------------
t0=$(date +%s)
out2="$(gate_fixture_run "$BR" repo \
    SPIRA_CERTIFY_PAR=1 SPIRA_GATE_LOCK_WAIT=6 SPIRA_GATE_SUITES=off)"
rc2=$?
t1=$(date +%s)
is   "suites=off: passes despite the held admission slot"             0 "$rc2"
want "suites=off: says PASS"                                          "VERDICT=PASS" "$out2"
nowant "suites=off: never reports an admission timeout"               "admission-timeout" "$out2"

echo
tl_summary
