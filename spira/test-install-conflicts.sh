#!/usr/bin/env bash
#
# test-install-conflicts.sh — install.sh's five conflict-predicate functions, called
# directly. install.sh defines _conflict_foreign, _conflict_aeon, _conflict_lock,
# _conflict_instance and _conflict_dolt before its argument parsing and returns without
# running when sourced (BASH_SOURCE[0] != $0), so this suite never runs doctor.sh, never
# renders a unit, and never spawns a process or binds a port — _conflict_aeon and
# _conflict_dolt take an injectable /proc root, and _conflict_dolt's TCP probe is its own
# function so a test can redefine it instead of binding one.
#
#   ./test-install-conflicts.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. FOREIGN HARNESS: installed unit's ExecStart resolves to a different SPIRA_HOME
#    → exit 5, names the foreign path, names the remedy and the override.
# 2. LIVE AEON: a fixture /proc entry whose cmdline is exactly <home>/aeon.sh
#    → exit 5, names "aeon", names the remedy.
# 3. LANDING IN FLIGHT: gate tree lock is held (real flock, no install)
#    → exit 5, names "landing", names the remedy.
# 4. INSTANCE MISMATCH: (a) argument disagrees with config SPIRA_INSTANCE,
#    (b) another instance's sentinel unit already points at this SPIRA_RUN
#    → each exits 5, names the disagreement, names the remedy.
# 5. DOLT PORT COLLISION: probe reports listening, fixture /proc shows a sql-server
#    process serving a different data_dir → exit 5, names "Dolt", names the remedy.
# Each predicate also has a CLEAR case proving it does not fire on an unremarkable
# fixture, so a predicate that always returned 5 would be caught as readily as one
# that never did.
#
# FAIL-FIRST: every predicate's offending case runs before its clear case, so a
# predicate that can never fire is caught by the first assertion, not hidden behind
# a clear case that would pass regardless (law-a-regression-test-must-be-seen-to-fail).
#
# tier: T1
# covers: install.sh UC-instance-lifecycle-16
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-install-conflicts.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# Sourcing install.sh (not executing it) defines the five _conflict_* functions and
# returns before argument parsing, doctor.sh or any phase runs.
. "$HERE/../install.sh"

# ===========================================================================
echo
echo "CONFLICT 1: foreign harness owns the installed sentinel unit"
# ===========================================================================
UNITDIR1="$TMP/unitdir1"
FOREIGN_HOME="$TMP/foreign-home"
mkdir -p "$UNITDIR1" "$FOREIGN_HOME"
cat > "$UNITDIR1/spira-sentinel-prod.service" <<EOF
[Service]
ExecStart=$FOREIGN_HOME/sentinel.sh
EOF

_f1_out="$(_conflict_foreign "$UNITDIR1" "$TMP/our-home" prod /nonexistent/units.sh 2>&1)"
_f1_rc=$?
wantrc "foreign: exits 5"                "5" "$_f1_rc"
want   "foreign: names the foreign path" "$FOREIGN_HOME" "$_f1_out"
want   "foreign: names the remedy"       "remedy" "$_f1_out"
want   "foreign: names the override"     "SPIRA_INSTALL_CONFLICT_CONSIDERED" "$_f1_out"

_f1c_out="$(_conflict_foreign "$TMP/empty-unitdir" "$TMP/our-home" prod /nonexistent/units.sh 2>&1)"
_f1c_rc=$?
wantrc "foreign-clear: exits 0 with no installed unit" "0" "$_f1c_rc"
nowant "foreign-clear: no CONFLICT report"             "CONFLICT" "$_f1c_out"

# ===========================================================================
echo
echo "CONFLICT 2: live aeon under this installation"
# ===========================================================================
PROC2="$TMP/proc2"
mkdir -p "$PROC2/4242"
printf '%s\0sleep\0600\0' "$TMP/aeon-home/aeon.sh" > "$PROC2/4242/cmdline"

_a1_out="$(_conflict_aeon "$TMP/aeon-home" "$PROC2" 2>&1)"
_a1_rc=$?
wantrc "aeon: exits 5"          "5" "$_a1_rc"
want   "aeon: names 'aeon'"     "aeon" "$_a1_out"
want   "aeon: names the pid"    "4242" "$_a1_out"
want   "aeon: names the remedy" "remedy" "$_a1_out"

_a1c_out="$(_conflict_aeon "$TMP/aeon-home" "$TMP/empty-proc" 2>&1)"
_a1c_rc=$?
wantrc "aeon-clear: exits 0 with no matching process" "0" "$_a1c_rc"
nowant "aeon-clear: no CONFLICT report"                "CONFLICT" "$_a1c_out"

# ===========================================================================
echo
echo "CONFLICT 3: landing pass in flight (gate tree lock held)"
# ===========================================================================
GATE_LOCK="$TMP/gate.lock"
touch "$GATE_LOCK"
(
    exec 9>"$GATE_LOCK"
    flock 9
    sleep 5
) &
_lock_holder=$!
# Poll for the lock instead of a fixed sleep: the holder subshell needs a moment
# to acquire flock before this process's own probe would see it as free.
for _i in 1 2 3 4 5 6 7 8 9 10; do
    flock -n "$GATE_LOCK" true 2>/dev/null || break
    sleep 0.05
done

_l1_out="$(_conflict_lock "$GATE_LOCK" 2>&1)"
_l1_rc=$?
kill "$_lock_holder" 2>/dev/null; wait "$_lock_holder" 2>/dev/null || true
wantrc "lock: exits 5 while held"  "5" "$_l1_rc"
want   "lock: names 'landing'"     "landing" "$_l1_out"
want   "lock: names the remedy"    "remedy" "$_l1_out"

_l1c_out="$(_conflict_lock "$TMP/no-such-lock" 2>&1)"
_l1c_rc=$?
wantrc "lock-clear: exits 0 with no lock file" "0" "$_l1c_rc"
nowant "lock-clear: no CONFLICT report"        "CONFLICT" "$_l1c_out"
unset _i

# ===========================================================================
echo
echo "CONFLICT 4a: instance argument disagrees with config SPIRA_INSTANCE"
# ===========================================================================
CONF4="$TMP/spira.conf"
cat > "$CONF4" <<EOF
SPIRA_INSTANCE = prod
EOF

_i1_out="$(_conflict_instance "$TMP/empty-unitdir4a" test "$CONF4" "$TMP/rundir4a" /nonexistent/units.sh 2>&1)"
_i1_rc=$?
wantrc "instance-mismatch: exits 5"          "5" "$_i1_rc"
want   "instance-mismatch: names disagreement" "disagrees" "$_i1_out"
want   "instance-mismatch: names the remedy"  "remedy" "$_i1_out"

_i1c_out="$(_conflict_instance "$TMP/empty-unitdir4a" prod "$CONF4" "$TMP/rundir4a" /nonexistent/units.sh 2>&1)"
_i1c_rc=$?
wantrc "instance-match: exits 0 when argument agrees" "0" "$_i1c_rc"

# ===========================================================================
echo
echo "CONFLICT 4b: another instance's units already point at this SPIRA_RUN"
# ===========================================================================
UNITDIR4B="$TMP/unitdir4b"
mkdir -p "$UNITDIR4B"
OUR_RUN="$TMP/our-run4b"
mkdir -p "$OUR_RUN"
cat > "$UNITDIR4B/spira-sentinel-staging.service" <<EOF
[Service]
StandardOutput=append:$OUR_RUN/sentinel.log
EOF

_i2_out="$(_conflict_instance "$UNITDIR4B" prod "" "$OUR_RUN" /nonexistent/units.sh 2>&1)"
_i2_rc=$?
wantrc "instance-collision: exits 5"           "5" "$_i2_rc"
want   "instance-collision: names other instance" "staging" "$_i2_out"
want   "instance-collision: names SPIRA_RUN"    "$OUR_RUN" "$_i2_out"

OTHER_RUN="$TMP/other-run4b"
mkdir -p "$OTHER_RUN"
_i2c_out="$(_conflict_instance "$UNITDIR4B" prod "" "$OTHER_RUN" /nonexistent/units.sh 2>&1)"
_i2c_rc=$?
wantrc "instance-distinct: exits 0 when SPIRA_RUN differs" "0" "$_i2c_rc"

# ===========================================================================
echo
echo "CONFLICT 5: Dolt server listening with a different data_dir"
# ===========================================================================
OUR_DOLT="$TMP/our-dolt"
OTHER_DOLT="$TMP/other-dolt"
mkdir -p "$OUR_DOLT" "$OTHER_DOLT"
cat > "$OUR_DOLT/dolt-server.yaml" <<EOF
listener:
  port: 4242
data_dir: "$OUR_DOLT"
EOF
PROC5="$TMP/proc5"
mkdir -p "$PROC5/9001"
printf 'dolt\0sql-server\0--data-dir\0%s\0' "$OTHER_DOLT" > "$PROC5/9001/cmdline"

# Stub the TCP probe rather than binding a real port — this is the one place the
# prior T2 suite depended on a fixed port (19877) shared with another suite, a
# flake source under parallel batch (gap G14 for this predicate).
_conflict_dolt_probe() { return 0; }

_d1_out="$(_conflict_dolt "$OUR_DOLT" "$PROC5" 2>&1)"
_d1_rc=$?
wantrc "dolt: exits 5"           "5" "$_d1_rc"
want   "dolt: names 'Dolt'"      "Dolt" "$_d1_out"
want   "dolt: names other datadir" "$OTHER_DOLT" "$_d1_out"
want   "dolt: names the remedy"  "remedy" "$_d1_out"

_d1c_out="$(_conflict_dolt "$OTHER_DOLT" "$PROC5" 2>&1)"
_d1c_rc=$?
wantrc "dolt-clear: exits 0 when the listening server IS ours" "0" "$_d1c_rc"

_conflict_dolt_probe() { return 1; }
_d1n_out="$(_conflict_dolt "$OUR_DOLT" "$PROC5" 2>&1)"
_d1n_rc=$?
wantrc "dolt-no-listener: exits 0 when nothing is listening" "0" "$_d1n_rc"

tl_summary
