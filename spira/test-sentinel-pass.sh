#!/usr/bin/env bash
#
# test-sentinel-pass.sh — real end-to-end sentinel.sh passes: CHECK 7 (summon) runs
#   before the Sending, the fill loop exhausts the task pool, the base-unchanged stamp
#   skips a redundant walk, and a database sentinel cannot reach exits 1 without ever
#   reporting 'goal reached'.
#
#   ./test-sentinel-pass.sh
#
# HOST OF test-sentinel-order.sh (dispatch.md D8, sp-9ce60.5), which also absorbs
# test-sentinel-capacity.sh. Cut from 9 real passes total (5 + 2 + 2, across the three
# merged files) to 2 Dolt-backed passes plus one DB-unreadable check that needs no
# database at all, now that the per-case arithmetic those passes used to be the ONLY
# coverage for has its own T1 tables:
#   - the fill loop's pool math and per-persona cap  -> ck7_pool/ck7_fill_cap (G2, G3),
#     tested directly in test-watchtower-throttle.sh
#   - lane rotation order                            -> lane_rotate (G1),
#     tested directly in test-lane-ceiling.sh
#   - CHECK 8's firing predicate                      -> check8_should_judge (G15),
#     tested directly in test-check8-progressed.sh
# What real passes still have to prove, and nothing else can: that sentinel.sh actually
# wires CHECK 7 to run before the Sending, that a fresh base walks and a matching stamp
# skips, and that the top-of-file `bdq list` probe really does fail the way the harness
# assumes when bd cannot reach the database.
#
# TWO DOLT PASSES, ONE FIXTURE, REUSED RUN DIR:
#   pass 1 "fill": pool=3 + 5 ready builder beads, fresh run dir (no stamp yet) -> 3
#     summons, sending.sh called (no stamp -> walks, which also writes the base stamp).
#     This is also the positive control that the database IS reachable (no "DATABASE
#     UNREADABLE"), which used to need its own pass in test-sentinel-capacity.sh.
#   pass 2 "order+stamp-skip": same run dir, same fixture -> the base-unchanged stamp
#     pass 1 wrote now matches, so sending.sh is not called and the log says so. The skip
#     path is also the only one that logs a literal "sending:" line, so the CHECK7-
#     before-sending order assertion runs against this pass, not pass 1's walk.
#
# ONE NO-DATABASE CHECK: SPIRA_BD points at a shim that always fails, so `bdq list` fails
# the way a real outage would without paying for a Dolt fixture at all (UC-dispatch-19).
#
# POSITIVE CONTROLS FIRST throughout (law-a-regression-test-must-be-seen-to-fail).
#
# defect: sp-len2q sp-4fss
# tier: T3
# covers: spira/sentinel.sh spira/lib.sh UC-dispatch-14 UC-dispatch-19
# hermetic-ok: uses a fixture database; systemd/gh/network reached through
#   SPIRA_LAUNCH, SPIRA_SUMMON and SPIRA_SYSTEMCTL seams pointed at stubs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
before() {
    local a="$1" b="$2" out="$3"
    local pa pb
    pa="$(grep -n "$a" <<< "$out" | head -1 | cut -d: -f1)"
    pb="$(grep -n "$b" <<< "$out" | head -1 | cut -d: -f1)"
    [ -n "$pa" ] && [ -n "$pb" ] && [ "$pa" -lt "$pb" ] \
        && ok "$a appears before $b in log" \
        || bad "$a before $b" "pa=[${pa:-missing}] pb=[${pb:-missing}]"
}

TMP="$(mktemp -d)"; trap 'testdb_drop 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM

STUBS="$TMP/stubs"
mkdir -p "$STUBS"
for _s in pilgrimage.sh strand.sh reflect.sh; do
    printf '#!/bin/sh\n' > "$STUBS/$_s"; chmod +x "$STUBS/$_s"
done
printf '#!/bin/sh\necho inactive\n'  > "$STUBS/mock-systemctl"; chmod +x "$STUBS/mock-systemctl"
printf '#!/bin/sh\nexit 0\n'         > "$STUBS/mock-launch";    chmod +x "$STUBS/mock-launch"
printf '#!/bin/sh\nexit 0\n'         > "$STUBS/mock-notify";    chmod +x "$STUBS/mock-notify"
touch "$STUBS/repo-map"   # empty: no repos, Sending finds nothing to walk
# THE REAL CHAMBER, symlinked once up front: `ln -sf` onto a directory that already
# exists creates the link INSIDE it instead of replacing it, so this must run before
# anything else ever creates $STUBS/chamber as a plain directory.
ln -s "$HERE/chamber" "$STUBS/chamber"

echo "test-sentinel-pass.sh"

# ======================================================================================
echo
echo "DB unreadable — no fixture needed, a failing SPIRA_BD shim fails fast (UC-dispatch-19):"
# ======================================================================================
# THIS NEEDS NO testdb_up AT ALL: the shim exits nonzero unconditionally, exactly the
# shape of "bd cannot reach the database", without spending a Dolt build to prove it.
FAILING_BD="$TMP/failing-bd"
printf '#!/bin/sh\nexit 1\n' > "$FAILING_BD"; chmod +x "$FAILING_BD"
_run_unreadable="$TMP/run-unreadable"; mkdir -p "$_run_unreadable"
out_unreadable="$(env -i \
    PATH="$PATH" HOME="$HOME" \
    SPIRA_HOME="$STUBS" \
    SPIRA_RUN="$_run_unreadable" \
    SPIRA_DB="$TMP/irrelevant-db" \
    SPIRA_BD="$FAILING_BD" \
    SPIRA_GOAL="sp-goal1" \
    SPIRA_FAYTHS="" \
    SPIRA_LAND_STALE=999999 \
    SPIRA_SYSTEMCTL="$STUBS/mock-systemctl" \
    SPIRA_LAUNCH="$STUBS/mock-launch" \
    SPIRA_NOTIFY="$STUBS/mock-notify" \
    bash "$HERE/sentinel.sh" 2>&1)"
rc=$?
is   "exits 1 when bd cannot reach the database"  "1" "$rc"
want "reports DATABASE UNREADABLE"                 "DATABASE UNREADABLE" "$out_unreadable"
lack "does NOT report 'goal reached'"              "goal reached" "$out_unreadable"

# ======================================================================================
echo
echo "real passes — fill, order and the sending stamp (needs a Dolt fixture):"
# ======================================================================================
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-sentinel-pass
testdb_up sentinel_pass || { echo "test-sentinel-pass: could not build fixture"; exit 1; }
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-goal1","title":"goal","status":"open","issue_type":"epic","labels":["plan"]}
{"id":"sp-b1","title":"bead 1","status":"open","issue_type":"task","labels":["plan"]}
{"id":"sp-b2","title":"bead 2","status":"open","issue_type":"task","labels":["plan"]}
{"id":"sp-b3","title":"bead 3","status":"open","issue_type":"task","labels":["plan"]}
{"id":"sp-b4","title":"bead 4","status":"open","issue_type":"task","labels":["plan"]}
{"id":"sp-b5","title":"bead 5","status":"open","issue_type":"task","labels":["plan"]}
JSONL

SUMMON_LOG="$TMP/summon.log"
SENDING_LOG="$TMP/sending.log"
# sending.sh records each invocation so assertions can verify whether it was called.
printf '#!/bin/sh\necho called >> "$SENDING_LOG"\n' > "$STUBS/sending.sh"; chmod +x "$STUBS/sending.sh"
# mock-summon records each summon so the fill assertion can count them.
printf '#!/bin/sh\necho summoned >> "$SUMMON_LOG"\n' > "$STUBS/mock-summon"; chmod +x "$STUBS/mock-summon"

# run_pass <run-dir> [KEY=VAL ...] → combined stdout+stderr of one sentinel pass.
# SPIRA_RUN is the caller-supplied dir (allows pre-populating and reusing stamp files).
run_pass() {
    local run="$1"; shift
    mkdir -p "$run"
    env -i \
        PATH="$PATH" HOME="$HOME" \
        SPIRA_HOME="$STUBS" \
        SPIRA_RUN="$run" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_BD="$SPIRA_BD" \
        SPIRA_PATH="$SPIRA_PATH" \
        SPIRA_GOAL="sp-goal1" \
        SPIRA_SKIP_RECLAIM=1 \
        SPIRA_LAND_STALE=999999 \
        SPIRA_SYSTEMCTL="$STUBS/mock-systemctl" \
        SPIRA_LAUNCH="$STUBS/mock-launch" \
        SPIRA_SUMMON="$STUBS/mock-summon" \
        SPIRA_NOTIFY="$STUBS/mock-notify" \
        SUMMON_LOG="$SUMMON_LOG" \
        SENDING_LOG="$SENDING_LOG" \
        SPIRA_FAYTHS=builder SPIRA_SCOPE_LABEL= SPIRA_MAX_AEONS=3 \
        "$@" \
        bash "$HERE/sentinel.sh" 2>&1
}

_run="$TMP/run-pass"
rm -f "$SUMMON_LOG" "$SENDING_LOG"
pass1_out="$(run_pass "$_run")"
is   "pass 1 fill: pool=3 + 5 ready beads -> 3 summons" \
     "3" "$(grep -c . "$SUMMON_LOG" 2>/dev/null || echo 0)"
want "pass 1 order: CHECK7 line appears in log" "CHECK7" "$pass1_out"
is   "pass 1 sending: no stamp yet -> sending.sh called" \
     "1" "$(grep -c . "$SENDING_LOG" 2>/dev/null || echo 0)"
lack "pass 1 sending: log does NOT say base unchanged" "base unchanged" "$pass1_out"
lack "pass 1: does NOT report DATABASE UNREADABLE (positive control: DB is readable)" \
     "DATABASE UNREADABLE" "$pass1_out"

# pass 1's walk just wrote the base stamp, so pass 2 (same run dir, same fixture) takes
# the skip path — which is also the only path that logs a literal "sending:" line, so the
# CHECK7-before-sending order check runs here rather than against pass 1's walk.
rm -f "$SUMMON_LOG" "$SENDING_LOG"
pass2_out="$(run_pass "$_run")"
is   "pass 2 stamp-skip: base unchanged -> sending.sh not called" \
     "0" "$(grep -c . "$SENDING_LOG" 2>/dev/null || echo 0)"
want "pass 2 stamp-skip: log says base unchanged" "base unchanged" "$pass2_out"
want   "pass 2 order: sending: line appears in log" "sending:" "$pass2_out"
before "CHECK7" "sending:" "$pass2_out"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
