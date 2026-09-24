#!/usr/bin/env bash
# test-gate-locks.sh — gate-locks.sh reports HELD/FREE correctly.
#
# THREE SCENARIOS:
#   1. FREE: lock file exists, no flock held → gate-locks reports FREE.
#   2. HELD with live holder: flock held by background process, holder file with live PID
#      → gate-locks reports HELD and names the live PID.
#   3. HELD via inherited fd: holder file names a dead PID but live PGID → gate-locks
#      reports HELD, not STALE.  This is the false-answer the bead tests for: the PID in
#      /proc/locks may be dead even when the lock is genuinely held by a child.
#
# covers: spira/gate-locks.sh spira/gate.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "must not contain [$2]" ;; *) ok "$1"; esac; }

GATE_LOCKS="$HERE/gate-locks.sh"
if [ ! -x "$GATE_LOCKS" ]; then
    bad "prerequisite: gate-locks.sh is executable" "not found at $GATE_LOCKS"
    printf '\n%d passed, %d failed\n' "$pass" "$fail"; exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
WDIR="$TMP/run/worktree"
mkdir -p "$WDIR"

_run() {
    bash "$GATE_LOCKS" "$TMP/run" 2>/dev/null
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
# Brief wait for the flock to settle.
_i=0
while ! flock -n "$LOCKF" /bin/true 2>/dev/null && [ "$_i" -lt 20 ]; do
    sleep 0.05; _i=$((_i+1))
done

# Positive control: flock is actually held before we run gate-locks.
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
_i=0
while ! flock -n "$LOCKF" /bin/true 2>/dev/null && [ "$_i" -lt 20 ]; do
    sleep 0.05; _i=$((_i+1))
done

# Positive control: the flock is genuinely held.
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
printf 'test-gate-locks: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
