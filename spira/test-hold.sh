#!/usr/bin/env bash
#
# test-hold.sh — hold.sh and unhold.sh let a non-aeon actor hold a bead
# so the reaper does not reclaim it.
#
#   ./test-hold.sh
#
# THE PROPERTY UNDER TEST. A non-aeon session (brain, concierge, a hand-run
# tool) must be able to hold a bead for the duration of a piece of hand-work,
# and holder_alive must say so — otherwise strand.sh reclaims it mid-landing
# and charges an attempt that is not a fact about the work (sp-oz0b, sp-7pi).
#
# A REAL bd ON A THROWAWAY DATABASE, because every claim is about what bd does
# with a status, a claim and an assignee (law-prefer-the-real-dependency).
#
# defect: sp-oz0b
# tier: T2
# covers: spira/hold.sh spira/unhold.sh spira/lib.sh spira/strand.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-hold
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT; trap 'exit 143' INT TERM
testdb_up hold || { echo "test-hold: could not build a fixture database"; exit 1; }

export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_CONF="$TMP/no-such-conf"
export SPIRA_GOAL=sp-goal
export SPIRA_REPO_MAP="$TMP/repo-map"
printf '# fixture — empty\n' > "$TMP/repo-map"
export SPIRA_HOLD_HEARTBEAT=3600
export BD_TIMEOUT=10

# shellcheck disable=SC1090
. "$HERE/lib.sh"

status_of() { bdjson show "$1" | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
print(d[0].get("status","") if d else "")' 2>/dev/null; }

assignee_of() { bdjson show "$1" | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
print(d[0].get("assignee","") or "" if d else "")' 2>/dev/null; }

seed() {
    local id="$1" st="${2:-open}" as="${3:-}"
    testdb_reset
    local line; line="{\"id\":\"$id\",\"title\":\"test bead\",\"status\":\"$st\",\"issue_type\":\"task\",\"labels\":[\"spira\",\"plan\"]"
    [ -n "$as" ] && line="$line,\"assignee\":\"$as\""
    line="$line,\"updated_at\":\"2026-09-06T00:00:00Z\"}"
    testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-06T00:00:00Z"}
$line
JSONL
}

# ======================================================================================
# 1. POSITIVE CONTROL: hold_alive recognises a hold pidfile with a live pid.
#    Without this, every absence check below is meaningless.
# ======================================================================================
echo "hold_alive positive control:"

# Use our own pid — it is certainly alive.
echo $$ > "$SPIRA_RUN/hold-sp-pos.pid"
if hold_alive "$SPIRA_RUN/hold-sp-pos.pid"; then ok "hold_alive sees a live pid"
else bad "hold_alive sees a live pid" "returned 1"; fi
rm -f "$SPIRA_RUN/hold-sp-pos.pid"

# ======================================================================================
# 2. NEGATIVE CONTROL: hold_alive rejects a dead pid.
# ======================================================================================
echo
echo "hold_alive negative control:"

echo 999999999 > "$SPIRA_RUN/hold-sp-neg.pid"
if hold_alive "$SPIRA_RUN/hold-sp-neg.pid"; then bad "hold_alive rejects a dead pid" "returned 0"
else ok "hold_alive rejects a dead pid"; fi
rm -f "$SPIRA_RUN/hold-sp-neg.pid"

# Missing pidfile.
if hold_alive "$SPIRA_RUN/hold-sp-none.pid"; then bad "hold_alive rejects a missing pidfile" "returned 0"
else ok "hold_alive rejects a missing pidfile"; fi

# ======================================================================================
# 3. holder_alive checks hold pidfiles. This is the core fix: before sp-oz0b,
#    holder_alive only looked at aeon-*-<id>.pid.
# ======================================================================================
echo
echo "holder_alive recognises a hold:"

echo $$ > "$SPIRA_RUN/hold-sp-ha.pid"
if holder_alive sp-ha; then ok "holder_alive sees a hold pidfile"
else bad "holder_alive sees a hold pidfile" "returned 1"; fi
rm -f "$SPIRA_RUN/hold-sp-ha.pid"

# And still sees aeon pidfiles too.
echo $$ > "$SPIRA_RUN/aeon-test-sp-ha2.pid"
# This will fail aeon_alive (our argv is not aeon.sh), but that is correct — the positive
# control for the aeon path is already in test-slay. Here we test the hold path.
rm -f "$SPIRA_RUN/aeon-test-sp-ha2.pid"

# ======================================================================================
# 4. hold.sh claims a bead and writes the pidfile.
# ======================================================================================
echo
echo "hold.sh:"

seed sp-h1 open
is "bead starts open" open "$(status_of sp-h1)"

out="$(bash "$HERE/hold.sh" sp-h1 --pid $$ 2>&1)"
rc=$?
is "hold.sh exits 0"        0   "$rc"
is "pidfile exists"          yes "$([ -f "$SPIRA_RUN/hold-sp-h1.pid" ] && echo yes || echo no)"
is "pidfile contains our pid" $$ "$(cat "$SPIRA_RUN/hold-sp-h1.pid" 2>/dev/null)"
is "heartbeat file exists"   yes "$([ -f "$SPIRA_RUN/hold-sp-h1.hb" ] && echo yes || echo no)"
is "bead is in_progress"     in_progress "$(status_of sp-h1)"

# holder_alive must see it.
if holder_alive sp-h1; then ok "holder_alive sees the hold"
else bad "holder_alive sees the hold" "returned 1"; fi

# The heartbeat process should be alive.
hbpid="$(cat "$SPIRA_RUN/hold-sp-h1.hb" 2>/dev/null)"
if [ -n "$hbpid" ] && [ -d "/proc/$hbpid" ]; then ok "heartbeat is alive"
else bad "heartbeat is alive" "pid=${hbpid:-empty} not in /proc"; fi

# ======================================================================================
# 5. hold.sh refuses a bead already held.
# ======================================================================================
echo
echo "hold.sh refuses double hold:"

out="$(bash "$HERE/hold.sh" sp-h1 --pid $$ 2>&1)"
rc=$?
is "hold.sh refuses double hold" 1 "$rc"

# ======================================================================================
# 6. unhold.sh tears down the hold.
# ======================================================================================
echo
echo "unhold.sh:"

out="$(bash "$HERE/unhold.sh" sp-h1 2>&1)"
rc=$?
is "unhold.sh exits 0"          0   "$rc"
is "pidfile is gone"             no  "$([ -f "$SPIRA_RUN/hold-sp-h1.pid" ] && echo yes || echo no)"
is "heartbeat file is gone"      no  "$([ -f "$SPIRA_RUN/hold-sp-h1.hb" ] && echo yes || echo no)"

# The heartbeat process should be dead (give it a moment).
sleep 0.2
if [ -n "$hbpid" ] && [ -d "/proc/$hbpid" ]; then bad "heartbeat is dead" "pid $hbpid still in /proc"
else ok "heartbeat is dead"; fi

# ======================================================================================
# 7. holder_alive returns 1 after release.
# ======================================================================================
echo
echo "holder_alive after release:"

if holder_alive sp-h1; then bad "holder_alive returns 1 after release" "returned 0"
else ok "holder_alive returns 1 after release"; fi

# ======================================================================================
# 8. A dead holder's pid frees the bead. This is the liveness contract: if the
#    holding process dies, the next sweep finds /proc/<pid> gone and reports the
#    bead unheld.
# ======================================================================================
echo
echo "dead holder frees the bead:"

seed sp-h2 open
# Start a short-lived background process to act as the holder.
sleep 999 &
holder_bg=$!
bash "$HERE/hold.sh" sp-h2 --pid "$holder_bg" >/dev/null 2>&1
if holder_alive sp-h2; then ok "hold is alive while holder lives"
else bad "hold is alive while holder lives" "returned 1"; fi

# Kill the holder.
kill "$holder_bg" 2>/dev/null; wait "$holder_bg" 2>/dev/null || true
sleep 0.1
if holder_alive sp-h2; then bad "hold is dead after holder dies" "returned 0"
else ok "hold is dead after holder dies"; fi

# Clean up — kill the heartbeat before removing files.  The heartbeat sleeps for
# SPIRA_HOLD_HEARTBEAT seconds between liveness checks, so removing the pidfile
# alone leaves it alive in the suite's process group until the harness kills it,
# which the harness counts as a test defect even when all assertions pass.
# Wait until the heartbeat has actually exited (not just received the signal),
# then give its sleep child — killed by the heartbeat's trap — time to exit too.
# Without this wait, suites.sh's kill -0 -- -pgid check after the test exits
# finds the heartbeat or its sleep child still in the process group.
hbpid2="$(cat "$SPIRA_RUN/hold-sp-h2.hb" 2>/dev/null)"
if [ -n "$hbpid2" ]; then
    kill "$hbpid2" 2>/dev/null || true
    while [ -d "/proc/$hbpid2" ]; do sleep 0.05; done
    sleep 0.05
fi
rm -f "$SPIRA_RUN/hold-sp-h2.pid" "$SPIRA_RUN/hold-sp-h2.hb"

# ======================================================================================
# 9. spira_holder_witnesses respects a hold. This is the two-witness system: the
#    destruction chokepoint must refuse while a hold is live.
# ======================================================================================
echo
echo "spira_holder_witnesses with a hold:"

seed sp-h3 in_progress
echo $$ > "$SPIRA_RUN/hold-sp-h3.pid"
witness="$(spira_holder_witnesses sp-h3)"
wrc=$?
is "witnesses say held"  0 "$wrc"
rm -f "$SPIRA_RUN/hold-sp-h3.pid"

# ======================================================================================
# SUMMARY
# ======================================================================================
echo
total=$((pass + fail))
tl_summary
