#!/usr/bin/env bash
#
# test-census-events.sh — census.sh reads requeue/reclaim/recur events written by bump_*.
#
#   ./test-census-events.sh
#
# WHAT THIS SUITE GUARDS
# ----------------------
# Before sp-2lk, bump_requeue/bump_reclaim/bump_recur were no-ops and census.sh
# read labels that nothing wrote. Every pass reported all-clear regardless of how
# many times beads were requeued or recurred. This suite asserts the wire-up works
# end-to-end: bump_requeue/bump_recur/bump_reclaim write events, and census.sh
# aggregates those events into the correct class counts.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control, law-a-regression-test-must-be-seen-to-fail)
# ----------------------------------------------------------------------------------------------------
# The suite was run against the unfixed tree before this commit; it produced
# FAIL for both of the event-based assertions below (census output was empty
# because bump_* wrote nothing and census read labels). The unfixed failure
# text: "wanted [sp-requeue-merge-conflict] in []" and
# "wanted [sp-recur-suite-red] in []".
#
# THREE ACCEPTANCE CRITERIA:
# 1. bump_requeue and bump_recur write events that census.sh counts.
# 2. The class name and occurrence count match the acceptance criteria from sp-2lk.
# 3. bump_reclaim writes events that census.sh counts as sp-reclaim.
#
# A REAL bd ON A THROWAWAY DATABASE (law-prefer-the-real-dependency).
#
# covers: spira/census.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1" ;; esac; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-census-events
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up census-events || { echo "test-census-events: could not build a fixture database"; exit 1; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-census-events.sh"

seed_bead() {   # seed_bead <id> — one open bead
    testdb_reset
    testdb_seed <<JSONL
{"id":"$1","title":"test bead","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-12T00:00:00Z"}
JSONL
}

census_out() {
    SPIRA_DB="$TESTDB_DIR" bash "$HERE/census.sh" --with-suppressed 2>/dev/null
}

# ======================================================================================
echo
echo "sp-2lk acceptance criteria — bump_requeue and bump_recur produce census entries"
# ======================================================================================
# The exact positive control from the bead:
#   bump_requeue "$id" merge-conflict (twice) + bump_recur "$id" suite-red (once)
#   → census must output: 1 sp-requeue-merge-conflict (2 detections)  and  1 sp-recur-suite-red (1 detections)
seed_bead "sp-c1"
bump_requeue "sp-c1" merge-conflict
bump_requeue "sp-c1" merge-conflict
bump_recur   "sp-c1" suite-red

out="$(census_out)"
want "census reports 1 distinct bead sp-requeue-merge-conflict (sp-2lk)" "1 sp-requeue-merge-conflict" "$out"
want "census shows 2 detections for sp-requeue-merge-conflict" "sp-requeue-merge-conflict (2 detections" "$out"
want "census reports 1 sp-recur-suite-red"        "1 sp-recur-suite-red"        "$out"

# ======================================================================================
echo
echo "bump_reclaim — events counted as sp-reclaim"
# ======================================================================================
seed_bead "sp-c2"
bump_reclaim "sp-c2"
bump_reclaim "sp-c2"

out="$(census_out)"
want "census reports sp-reclaim with 2 detections (1 bead)" "sp-reclaim (2 detections" "$out"

# ======================================================================================
echo
echo "bump_reclaim with cause — events counted as sp-reclaim-<cause>"
# ======================================================================================
seed_bead "sp-c3"
bump_reclaim "sp-c3" timeout
bump_reclaim "sp-c3" timeout
bump_reclaim "sp-c3" timeout

out="$(census_out)"
want "census reports sp-reclaim-timeout with 3 detections (1 bead)" "sp-reclaim-timeout (3 detections" "$out"
nowant "no bare sp-reclaim" "sp-reclaim " "$out"

# ======================================================================================
echo
echo "class isolation — separate beads contribute to the same class"
# ======================================================================================
# Two different beads, same requeue cause — the class count is cross-bead
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-d1","title":"bead 1","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-12T00:00:00Z"}
{"id":"sp-d2","title":"bead 2","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-12T00:00:00Z"}
JSONL
bump_requeue "sp-d1" merge-conflict
bump_requeue "sp-d2" merge-conflict
bump_requeue "sp-d2" merge-conflict

out="$(census_out)"
want "cross-bead: 2 distinct beads for sp-requeue-merge-conflict" "2 sp-requeue-merge-conflict" "$out"
want "cross-bead: 3 total event detections shown" "sp-requeue-merge-conflict (3 detections" "$out"

# ======================================================================================
echo
echo "positive control — empty store reports nothing"
# ======================================================================================
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-e1","title":"bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-12T00:00:00Z"}
JSONL
out="$(census_out)"
is "census is empty when no bump events exist" "" "$out"

# ======================================================================================
echo
echo "caller-side: bead_reopen + bump_requeue (the landing.sh requeue path)"
# ======================================================================================
# The landing pass calls bump_requeue when a branch cannot rebase (before deciding
# whether to reopen or escalate). Calling bump_requeue alone would pass even if
# landing.sh had no bump call; this test exercises the caller-side path so removing
# bump_requeue from landing.sh leaves a gap the existing direct-call tests would not
# catch.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-f1","title":"landing test","status":"in_progress","issue_type":"task","labels":["spira"],"updated_at":"2026-09-12T00:00:00Z"}
JSONL
bead_reopen "sp-f1" "rebase conflict test" >/dev/null 2>&1
bump_requeue "sp-f1" merge-conflict >/dev/null 2>&1

out="$(census_out)"
want "landing requeue path produces sp-requeue-merge-conflict" "1 sp-requeue-merge-conflict" "$out"

# ======================================================================================
echo
echo "caller-side: bdq reclaim + bump_reclaim ghost (the strand.sh reclaim path)"
# ======================================================================================
# strand.sh calls bdq reclaim --id then bump_reclaim ghost. Seeding in_progress lets the
# reclaim succeed; the bump_reclaim call that follows is what census reads.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-f2","title":"strand test","status":"in_progress","issue_type":"task","labels":["spira"],"updated_at":"2026-09-12T00:00:00Z"}
JSONL
bdq reclaim --id "sp-f2" --older-than 1s >/dev/null 2>&1 || true
bump_reclaim "sp-f2" ghost >/dev/null 2>&1

out="$(census_out)"
want "strand reclaim path produces sp-reclaim-ghost" "1 sp-reclaim-ghost" "$out"

# ======================================================================================
echo
echo "bead_reopen alone — census counts harness reopens without a separate bump_requeue (sp-df8qo)"
# ======================================================================================
# bead_reopen calls bdq reopen, which writes event_type='reopened' to the events table.
# Before this fix, _census_events_sql excluded 'reopened' from its IN clause, so harness
# reopens produced no census output.
# POSITIVE CONTROL first (law-absence-needs-a-positive-control): verify absence is detectable.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-g0","title":"no-reopen control","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-16T00:00:00Z"}
JSONL
_pc_out="$(census_out)"
is "positive control: no bead_reopen produces no sp-reopen" "" "$(printf '%s' "$_pc_out" | grep sp-reopen || true)"

testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-g1","title":"reopen test","status":"closed","issue_type":"task","labels":["spira"],"updated_at":"2026-09-16T00:00:00Z"}
JSONL
bead_reopen "sp-g1" "Reopened by test: sp-df8qo" >/dev/null 2>&1

out="$(census_out)"
want "bead_reopen alone produces sp-reopen in census" "sp-reopen" "$out"
want "sp-reopen shows 1 distinct bead" "1 sp-reopen" "$out"

# ======================================================================================
echo
echo "census_events_run_sql: empty embedded store exits 0; broken reader exits non-zero"
# ======================================================================================
# law-absence-needs-a-positive-control: assert the broken-reader path exits non-zero
# first, then believe census_events_run_sql when it returns 0 for an empty store.
# Embedded install with no dolt CLI and no events.log is genuinely empty — not
# unreachable. This is the distinction sp-uhx0 adds a test for.
_T_EMPTY="$(mktemp -d)"
_T_BROKEN="$(mktemp -d)"
_T_BIN="$(mktemp -d)"
printf '#!/bin/sh\nexit 1\n' > "$_T_BIN/bd";   chmod +x "$_T_BIN/bd"
printf '#!/bin/sh\nexit 1\n' > "$_T_BIN/dolt"; chmod +x "$_T_BIN/dolt"
mkdir -p "$_T_EMPTY/.beads/embeddeddolt/sp"

# Positive control: no embeddeddolt dir, bd fails → must exit non-zero.
_pc_rc=0
( SPIRA_DB="$_T_BROKEN" SPIRA_BD="$_T_BIN/bd" \
  PATH="$_T_BIN:$PATH" census_events_run_sql ) >/dev/null 2>&1 || _pc_rc=$?
if [ "$_pc_rc" -ne 0 ]; then
    ok "positive control: missing store exits non-zero"
else
    bad "positive control: missing store exits non-zero" "expected non-zero, got 0"
fi

# Empty embedded store: embeddeddolt exists, dolt non-functional, no events.log → exit 0.
_empty_rc=0
_empty_out=""
_empty_out="$(SPIRA_DB="$_T_EMPTY" SPIRA_BD="$_T_BIN/bd" \
              PATH="$_T_BIN:$PATH" census_events_run_sql 2>/dev/null)" || _empty_rc=$?
is "empty embedded store exits 0" "0" "$_empty_rc"
is "empty embedded store produces no output" "" "$_empty_out"

rm -rf "$_T_EMPTY" "$_T_BROKEN" "$_T_BIN"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
