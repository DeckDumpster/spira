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
# text: "wanted [sp-reopen-rebase-conflict] in []" and
# "wanted [sp-recur-suite-red] in []".
#
# THREE ACCEPTANCE CRITERIA:
# 1. bump_requeue and bump_recur write events that census.sh counts.
# 2. The class name and occurrence count match the acceptance criteria from sp-2lk.
# 3. bump_reclaim writes events that census.sh counts as sp-reclaim.
#
# A REAL bd ON A THROWAWAY DATABASE (law-prefer-the-real-dependency).
# Requires server mode: census_events_run_sql uses bd sql, and bd-embedded refuses
# bd sql in embedded mode. Skips when SPIRA_TESTDB_DATA is not set.
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
export SPIRA_TESTDB_MODE=server
testdb_up census-events || {
    # Server testdb unavailable (no running Dolt server). Skip rather than fail: the
    # test requires bd sql, which bd-embedded refuses in embedded mode.
    printf 'SKIP test-census-events: server testdb not available\n' >&2
    exit 77
}
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
#   → census must output: 1 sp-reopen-rebase-conflict (2 detections)  and  1 sp-recur-suite-red (1 detections)
seed_bead "sp-c1"
bump_requeue "sp-c1" merge-conflict
bump_requeue "sp-c1" merge-conflict
bump_recur   "sp-c1" suite-red

out="$(census_out)"
want "census reports 1 distinct bead sp-reopen-rebase-conflict" "1 sp-reopen-rebase-conflict" "$out"
want "census shows 2 detections for sp-reopen-rebase-conflict" "sp-reopen-rebase-conflict (2 detections" "$out"
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
want "cross-bead: 2 distinct beads for sp-reopen-rebase-conflict" "2 sp-reopen-rebase-conflict" "$out"
want "cross-bead: 3 total event detections shown" "sp-reopen-rebase-conflict (3 detections" "$out"

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
bead_reopen "sp-f1" rebase-conflict "rebase conflict test" >/dev/null 2>&1
bump_requeue "sp-f1" merge-conflict >/dev/null 2>&1

out="$(census_out)"
want   "landing requeue path produces sp-reopen-rebase-conflict" "1 sp-reopen-rebase-conflict" "$out"
nowant "landing requeue path: sp-requeue-merge-conflict absent" "sp-requeue-merge-conflict" "$out"

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
echo "bead_reopen cause — census classifies harness reopens as sp-reopen-<cause> (sp-0wwcn)"
# ======================================================================================
# bead_reopen <id> <cause> <note> writes event_type='reopen' with new_value=<cause>.
# census.sh must report sp-reopen-<cause> with the correct distinct-bead count.
# POSITIVE CONTROL first (law-absence-needs-a-positive-control): verify absence is detectable.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-g0","title":"no-reopen control","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-16T00:00:00Z"}
JSONL
_pc_out="$(census_out)"
is "positive control: no bead_reopen produces no sp-reopen-*" "" "$(printf '%s' "$_pc_out" | grep sp-reopen || true)"

testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-g1","title":"reopen test","status":"closed","issue_type":"task","labels":["spira"],"updated_at":"2026-09-16T00:00:00Z"}
JSONL
bead_reopen "sp-g1" gate-red "Reopened by test: sp-0wwcn" >/dev/null 2>&1

out="$(census_out)"
want "bead_reopen produces sp-reopen-gate-red in census" "sp-reopen-gate-red" "$out"
want "sp-reopen-gate-red shows 1 distinct bead" "1 sp-reopen-gate-red" "$out"
nowant "no bare sp-reopen class" "sp-reopen " "$out"

# Two different beads, same cause — distinct-bead count is 2.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-g2","title":"reopen multi 1","status":"closed","issue_type":"task","labels":["spira"],"updated_at":"2026-09-16T00:00:00Z"}
{"id":"sp-g3","title":"reopen multi 2","status":"closed","issue_type":"task","labels":["spira"],"updated_at":"2026-09-16T00:00:00Z"}
JSONL
bead_reopen "sp-g2" gate-red "first gate failure" >/dev/null 2>&1
bead_reopen "sp-g3" gate-red "second gate failure" >/dev/null 2>&1

out="$(census_out)"
want "two beads with same cause: 2 distinct beads" "2 sp-reopen-gate-red" "$out"

# Verify the cause is recorded in the events table as event_type='reopen'.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-g4","title":"cause row test","status":"closed","issue_type":"task","labels":["spira"],"updated_at":"2026-09-16T00:00:00Z"}
JSONL
bead_reopen "sp-g4" rebase-conflict "Reopened: conflict" >/dev/null 2>&1
_ev_cause="$("${SPIRA_BD:-bd}" -C "$TESTDB_DIR" sql \
    "SELECT COALESCE(new_value,'') FROM events WHERE issue_id='sp-g4' AND event_type='reopen'" \
    2>/dev/null | sed -n '3p' | tr -d ' ')"
is "bead_reopen writes event_type=reopen with cause in new_value" "rebase-conflict" "$_ev_cause"


# ======================================================================================
echo
echo "sp-vtyo9: NULL-cause reopens — 2 distinct beads, 8 events → 2 sp-reopen (8 detections)"
# ======================================================================================
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail):
# Run against unfixed census.sh (origin/main before sp-vtyo9):
#   FAIL  2 sp-reopen for 2-bead fixture: wanted [2 sp-reopen] in [8 sp-reopen (8 detections, 8 all-time)]
# Empty COALESCE cell shrinks the 4-column row to 3; the 3-column branch reads
# n_beads as n_events (both 8), inflating distinct-bead count to event count.
_write_reopen() {
    local id="$1"
    local uuid
    uuid="$(python3 -c 'import uuid; print(str(uuid.uuid4()))' 2>/dev/null)" || return 1
    bdq sql "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', 'reopened', 'harness', NULL, NOW())" >/dev/null 2>&1 || true
}
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-h1","title":"reopen bead 1","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-17T00:00:00Z"}
{"id":"sp-h2","title":"reopen bead 2","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-17T00:00:00Z"}
JSONL
_write_reopen sp-h1; _write_reopen sp-h1; _write_reopen sp-h1; _write_reopen sp-h1
_write_reopen sp-h2; _write_reopen sp-h2; _write_reopen sp-h2; _write_reopen sp-h2

out="$(census_out)"
want "2 sp-reopen for 2-bead fixture" "2 sp-reopen" "$out"
want "sp-reopen (8 detections" "sp-reopen-unrecorded (8 detections" "$out"

# ======================================================================================
echo
echo "sp-9edq8: bump_requeue merge-conflict + bead_reopen rebase-conflict → one census class"
# ======================================================================================
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail):
# Unfixed: census prints two lines — one sp-requeue-merge-conflict, one sp-reopen-rebase-conflict.
# Fixed: one line, sp-reopen-rebase-conflict with 1 bead. requeues_of still counts the requeue.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-z1","title":"conflict bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-19T00:00:00Z"}
JSONL
bump_requeue "sp-z1" merge-conflict >/dev/null 2>&1
bead_reopen  "sp-z1" rebase-conflict "conflict test" >/dev/null 2>&1

out="$(census_out)"
nowant "sp-requeue-merge-conflict absent: folded into sp-reopen-rebase-conflict" "sp-requeue-merge-conflict" "$out"
want   "sp-reopen-rebase-conflict present for the paired conflict events" "1 sp-reopen-rebase-conflict" "$out"
_conf_lines="$(printf '%s\n' "$out" | grep -c 'rebase-conflict\|merge-conflict' || true)"
is "exactly one conflict class line" "1" "$_conf_lines"
_att="$(attempts_of "sp-z1")"
is "no attempt charged for a conflict requeue" "0" "$_att"
_rqn="$(requeues_of "sp-z1")"
is "requeues_of still counts the requeue event" "1" "$_rqn"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
