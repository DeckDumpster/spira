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
# with a status, a claim and an assignee (law-prefer-the-real-dependency). The
# pure pidfile-liveness logic (hold_alive/holder_alive/spira_holder_witnesses,
# no bd needed) is test-hold-liveness.sh (T1).
#
# defect: sp-oz0b
# tier: T2
# covers: spira/hold.sh spira/unhold.sh spira/lib.sh strand/src/*
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
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-06T00:00:00Z"}
$line
JSONL
}

# ======================================================================================
# 1. hold.sh claims a bead and writes the pidfile.
# ======================================================================================
echo
echo "hold.sh:"

seed sp-h1 open
is "bead starts open" open "$(status_of sp-h1)"

out="$(hold.sh sp-h1 --pid $$ 2>&1)"
rc=$?
is "hold.sh exits 0"        0   "$rc"
is "pidfile exists"          yes "$([ -f "$SPIRA_RUN/hold-sp-h1.pid" ] && echo yes || echo no)"
is "pidfile contains our pid" $$ "$(cat "$SPIRA_RUN/hold-sp-h1.pid" 2>/dev/null)"
is "heartbeat file exists"   yes "$([ -f "$SPIRA_RUN/hold-sp-h1.hb" ] && echo yes || echo no)"
# THE HOLD IS AN OPERATOR HOLD ON spira-lc, NOT A bd claim (sp-rlyl0): bd's own status is
# unmoved by hold.sh now — the pidfile above is what actually keeps the reaper off it, and
# that is what holder_alive/spira_holder_witnesses below are checking.
is "bead stays open — the hold does not touch bd status" open "$(status_of sp-h1)"

# holder_alive must see it.
if holder_alive sp-h1; then ok "holder_alive sees the hold"
else bad "holder_alive sees the hold" "returned 1"; fi

# The heartbeat process should be alive.
hbpid="$(cat "$SPIRA_RUN/hold-sp-h1.hb" 2>/dev/null)"
if [ -n "$hbpid" ] && [ -d "/proc/$hbpid" ]; then ok "heartbeat is alive"
else bad "heartbeat is alive" "pid=${hbpid:-empty} not in /proc"; fi

# ======================================================================================
# 2. hold.sh refuses a bead already held.
# ======================================================================================
echo
echo "hold.sh refuses double hold:"

out="$(hold.sh sp-h1 --pid $$ 2>&1)"
rc=$?
is "hold.sh refuses double hold" 1 "$rc"

# ======================================================================================
# 3. unhold.sh tears down the hold.
# ======================================================================================
echo
echo "unhold.sh:"

out="$(unhold.sh sp-h1 2>&1)"
rc=$?
is "unhold.sh exits 0"          0   "$rc"
is "pidfile is gone"             no  "$([ -f "$SPIRA_RUN/hold-sp-h1.pid" ] && echo yes || echo no)"
is "heartbeat file is gone"      no  "$([ -f "$SPIRA_RUN/hold-sp-h1.hb" ] && echo yes || echo no)"

# The heartbeat process should be dead (give it a moment).
sleep 0.2
if [ -n "$hbpid" ] && [ -d "/proc/$hbpid" ]; then bad "heartbeat is dead" "pid $hbpid still in /proc"
else ok "heartbeat is dead"; fi

# ======================================================================================
# 4. holder_alive returns 1 after release.
# ======================================================================================
echo
echo "holder_alive after release:"

if holder_alive sp-h1; then bad "holder_alive returns 1 after release" "returned 0"
else ok "holder_alive returns 1 after release"; fi

# ======================================================================================
# 5. A dead holder's pid frees the bead. This is the liveness contract: if the
#    holding process dies, the next sweep finds /proc/<pid> gone and reports the
#    bead unheld.
# ======================================================================================
echo
echo "dead holder frees the bead:"

seed sp-h2 open
# Start a short-lived background process to act as the holder.
sleep 999 &
holder_bg=$!
# Kill-on-exit (sp-r70dc): the explicit kill a few lines down only runs if nothing between
# here and there exits first — fold the fixture into the EXIT trap as a backstop so it
# cannot outlive the suite.
trap 'kill "$holder_bg" 2>/dev/null; testdb_drop; rm -rf "$TMP"' EXIT
hold.sh sp-h2 --pid "$holder_bg" >/dev/null 2>&1
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
# SUMMARY
# ======================================================================================
echo
tl_summary
