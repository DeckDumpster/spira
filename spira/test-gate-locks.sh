#!/usr/bin/env bash
# test-gate-locks.sh — gate-locks.sh reports FREE/HELD/STALE correctly.
#
# FOUR SCENARIOS:
#   1. FREE: lock file exists, no flock held → gate-locks reports FREE.
#   2. HELD with live holder: flock held by background process, holder file with live PID
#      → gate-locks reports HELD and names the live PID.
#   3. HELD via inherited fd: holder file names a dead PID but live PGID → gate-locks
#      reports HELD, not STALE. This is a false-answer gate-locks must avoid: the PID in
#      /proc/locks may be dead even when the lock is genuinely held by a child.
#   4. STALE: holder file names a dead PID AND a dead PGID, but flock -n still cannot
#      acquire the lock — held by a process outside the recorded group (gap #11 in
#      docs/test-plan/gate-verdict.md: every prior STALE assertion here was a `nowant`,
#      so nothing ever showed STALE is reported at all).
#
# covers: spira/gate-locks.sh spira/gate.sh UC-gate-verdict-19
# tier: T2
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

GATE_LOCKS="$HERE/gate-locks.sh"
[ -x "$GATE_LOCKS" ] || bail "prerequisite: gate-locks.sh not found or not executable at $GATE_LOCKS"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
WDIR="$TMP/run/worktree"
mkdir -p "$WDIR"

_run() {
    bash "$GATE_LOCKS" "$TMP/run" 2>/dev/null
}

# wait_for_flock <lockfile> — block until an external `flock -n` attempt against the file
# fails, i.e. some other process has acquired it. Caps at 1s so a broken fixture reports a
# clear failure instead of hanging.
wait_for_flock() {
    local lockfile="$1" i=0
    while flock -n "$lockfile" /bin/true 2>/dev/null && [ "$i" -lt 20 ]; do
        sleep 0.05; i=$((i+1))
    done
}

# ──────────────────────────────────────────────────────────────────────────────
echo "1. positive control — script finds the lock directory"

# Positive control: a lock that does not exist must not appear in output.
out="$(_run)"
nowant "1-pos: non-existent lock absent from empty output" \
    "harness.spira-sp-fake" "$out"

# ──────────────────────────────────────────────────────────────────────────────
echo
echo "2. FREE — lock file present, no flock held"

LOCKF="$WDIR/.gate.harness.spira-sp-free.lock"
touch "$LOCKF"
out="$(_run)"
want  "2a: FREE lock appears in output"  "harness.spira-sp-free" "$out"
want  "2b: status is FREE"               "FREE"                   "$out"
nowant "2c: no HELD/STALE for free lock" "HELD"                   "$out"
nowant "2d: no STALE for free lock"      "STALE"                  "$out"
rm -f "$LOCKF"

# ──────────────────────────────────────────────────────────────────────────────
echo
echo "3. HELD — live holder PID in holder file"

LOCKF="$WDIR/.gate.harness.spira-sp-held.lock"
touch "$LOCKF"

# Acquire flock in a background subshell; it runs until we kill it.
( exec 9>"$LOCKF"; flock 9; sleep 3600 ) &
holder_bg=$!
wait_for_flock "$LOCKF"

if ! flock -n "$LOCKF" /bin/true 2>/dev/null; then
    ok "3-pos: flock is held (positive control)"
else
    bad "3-pos: flock is held (positive control)" "flock -n succeeded; lock was not acquired"
fi

# Write a holder file with the background PID and its PGID.
holder_pgid="$(ps -o pgid= -p "$holder_bg" 2>/dev/null | tr -d ' ')" || holder_pgid="?"
printf '%s %s\n' "$holder_bg" "$holder_pgid" > "$LOCKF.holder"

out="$(_run)"
want  "3a: HELD lock appears in output"    "harness.spira-sp-held" "$out"
want  "3b: status is HELD"                 "HELD"                  "$out"
want  "3c: holder PID appears in output"   "$holder_bg"            "$out"
nowant "3d: not reported STALE"            "STALE"                 "$out"

kill "$holder_bg" 2>/dev/null; wait "$holder_bg" 2>/dev/null || true
rm -f "$LOCKF" "$LOCKF.holder"

# ──────────────────────────────────────────────────────────────────────────────
echo
echo "4. HELD via inherited fd — holder PID dead, PGID alive → HELD not STALE"

# This is the scenario described in the bead: /proc/locks shows a dead PID because the
# original acquirer exited, but the fd is still held by a process in the same group.
LOCKF="$WDIR/.gate.harness.spira-sp-inherit.lock"
touch "$LOCKF"

# Hold the flock in a background subshell.
( exec 9>"$LOCKF"; flock 9; sleep 3600 ) &
inherit_bg=$!
wait_for_flock "$LOCKF"

if ! flock -n "$LOCKF" /bin/true 2>/dev/null; then
    ok "4-pos: flock is held (positive control)"
else
    bad "4-pos: flock is held (positive control)" "lock was not acquired"
fi

# Obtain a PID that is guaranteed dead: start a trivial process, note its PID, wait for it.
( exit 0 ) &
dead_pid=$!
wait "$dead_pid" 2>/dev/null || true

live_pgid="$(ps -o pgid= -p "$inherit_bg" 2>/dev/null | tr -d ' ')" || live_pgid="?"
printf '%s %s\n' "$dead_pid" "$live_pgid" > "$LOCKF.holder"

# Positive control: the holder PID really is dead (the false-answer scenario we are
# guarding against: a reporter that trusts the /proc/locks PID would call this STALE).
if [ -d "/proc/$dead_pid" ]; then
    bad "4-pos2: dead_pid ($dead_pid) does not exist (positive control)" \
        "pid is still alive — kernel has not yet reaped it; try re-running"
else
    ok "4-pos2: dead_pid ($dead_pid) does not exist (positive control)"
fi

out="$(_run)"
want  "4a: inherited lock appears"       "harness.spira-sp-inherit" "$out"
want  "4b: status is HELD not STALE"     "HELD"                     "$out"
nowant "4c: not reported as STALE"       "STALE"                    "$out"

kill "$inherit_bg" 2>/dev/null; wait "$inherit_bg" 2>/dev/null || true
rm -f "$LOCKF" "$LOCKF.holder"

# ──────────────────────────────────────────────────────────────────────────────
echo
echo "5. STALE — holder PID and PGID both dead, but the lock is genuinely unacquirable"
# (gap #11) A dead PID/PGID pair in the holder file does not by itself mean the lock is
# free — the real holder here is a process outside that recorded group, so gate-locks
# must not report FREE just because the recorded holder looks dead. This is the actual
# STALE branch in gate-locks.sh: rec_pid dead, and _pgid_live_member(rec_pgid) empty.
LOCKF="$WDIR/.gate.harness.spira-sp-stale.lock"
touch "$LOCKF"

# Hold the flock from a group unrelated to the (fabricated) holder file below.
( exec 9>"$LOCKF"; flock 9; sleep 3600 ) &
stale_holder_bg=$!
wait_for_flock "$LOCKF"

if ! flock -n "$LOCKF" /bin/true 2>/dev/null; then
    ok "5-pos: flock is held (positive control)"
else
    bad "5-pos: flock is held (positive control)" "lock was not acquired"
fi

# A second, already-dead PID/PGID pair — guaranteed unrelated to any live process, so
# neither the PID nor any member of its PGID is alive.
( exit 0 ) &
dead_pid2=$!
wait "$dead_pid2" 2>/dev/null || true
printf '%s %s\n' "$dead_pid2" "$dead_pid2" > "$LOCKF.holder"

out="$(_run)"
want  "5a: STALE lock appears in output"  "harness.spira-sp-stale" "$out"
want  "5b: status is STALE"               "STALE"                  "$out"
nowant "5c: not reported as HELD"         "HELD"                   "$out"
want  "5d: a stale-lock summary is printed" "stale lock"            "$out"

kill "$stale_holder_bg" 2>/dev/null; wait "$stale_holder_bg" 2>/dev/null || true
rm -f "$LOCKF" "$LOCKF.holder"

tl_summary
