#!/usr/bin/env bash
# tier: T1
# covers: spira/testlib.sh spira/conf.d/SPIRA_TEST_CASE_DEADLINE testenv/src/tap.rs
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/testlib.sh"

SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT INT TERM

# _run <script-body-on-stdin>: runs a fixture suite that sources the real testlib.sh with a
# 1 s case deadline, under an outer timeout well short of the fixture's own sleep.
# Leaves _RUN_OUT, _RUN_RC and _RUN_FILE.
_run() {
    local f="$SCRATCH/fixture-$$-$RANDOM.sh"
    { printf '. "%s/testlib.sh"\ntl_config SPIRA_TEST_CASE_DEADLINE=1\n' "$HERE"; cat; } > "$f"
    _RUN_OUT="$(timeout 5 bash "$f" 2>&1)"; _RUN_RC=$?
    _RUN_FILE="$f"
}

# --- a hanging third case ------------------------------------------------------------
_run <<EOF2
ok "first"
ok "second"
sleep 15 &
echo \$! > "$SCRATCH/child-pid"
wait
ok "third"
touch "$SCRATCH/case-4-ran"
ok "fourth"
tl_summary
EOF2
want   "hang: the hung case is named as case 3" "not ok 3 - case 3 exceeded its 1s deadline" "$_RUN_OUT"
want   "hang: the hung case carries the file:line of the last assertion before it" "# at $_RUN_FILE:" "$_RUN_OUT"
want   "hang: the hung case carries a duration of at least the deadline" "#ms=1" "$_RUN_OUT"
[ -s "$SCRATCH/child-pid" ] && [ ! -d "/proc/$(cat "$SCRATCH/child-pid")" ] && ok "hang: the hung case's child process was killed" || bad "hang: the hung case's child process was killed"
want   "hang: the stream ends in a TAP bail-out naming the case" "Bail out! case 3 exceeded its 1s deadline" "$_RUN_OUT"
nowant "hang: case 4 never ran" "ok 4" "$_RUN_OUT"
[ ! -e "$SCRATCH/case-4-ran" ] && ok "hang: the code after the hung case did not run" || bad "hang: the code after the hung case did not run"
[ "$_RUN_RC" -ne 0 ] && [ "$_RUN_RC" -ne 124 ] && ok "hang: the suite ended itself, not by the outer timeout" || bad "hang: the suite ended itself, not by the outer timeout" "rc=$_RUN_RC"

# --- a failing case stops the suite --------------------------------------------------
_run <<EOF2
ok "first"
bad "second" "wanted x"
touch "$SCRATCH/after-bad-ran"
ok "third"
tl_summary
EOF2
want   "stop: a failing case is reported" "not ok 2 - second" "$_RUN_OUT"
want   "stop: the failing case names its file:line" "# at $_RUN_FILE:4" "$_RUN_OUT"
want   "stop: a TAP bail-out names the failing case" "Bail out! case 2 failed: second" "$_RUN_OUT"
want   "stop: totals and the ASSERTIONS trailer still print" "ASSERTIONS 2" "$_RUN_OUT"
[ ! -e "$SCRATCH/after-bad-ran" ] && ok "stop: nothing after the failing case ran" || bad "stop: nothing after the failing case ran"
wantrc "stop: the suite exits 1, a plain red" 1 "$_RUN_RC"

# --- a failure inside a subshell stops the whole suite ------------------------------
_run <<EOF2
tl_subshell_safe
( bad "in a subshell" )
touch "$SCRATCH/after-subshell-ran"
ok "later"
tl_summary
EOF2
want   "stop: a failure in a subshell is reported" "not ok 1 - in a subshell" "$_RUN_OUT"
[ ! -e "$SCRATCH/after-subshell-ran" ] && ok "stop: a subshell's failure stops the parent suite" || bad "stop: a subshell's failure stops the parent suite"
[ "$_RUN_RC" -ne 0 ] && ok "stop: the parent exits non-zero after a subshell's failure" || bad "stop: the parent exits non-zero after a subshell's failure"

# --- a slow but passing case within its deadline -----------------------------------
_run <<'EOF2'
sleep 0.5
ok "slow but in time"
tl_summary
EOF2
wantrc "deadline: a case inside its deadline passes" 0 "$_RUN_RC"
nowant "deadline: nothing is reported hung" "exceeded" "$_RUN_OUT"

# --- opting out ---------------------------------------------------------------------
_run <<'EOF2'
tl_keep_going
bad "one"
ok "two"
tl_summary
EOF2
want   "keep-going: a suite that opts out runs on past a failure" "ok 2 - two" "$_RUN_OUT"

tl_summary
