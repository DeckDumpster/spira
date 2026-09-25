#!/usr/bin/env bash
#
# test-check4-events.sh — attempts are computed from the events trail, not from labels.
#
#   ./test-check4-events.sh
#
# THE DEFECT THIS GUARDS. Counter labels (sp-attempt-N, sp-reclaim-N, sp-requeue-N,
# sp-timeout-N, sp-recur-N) polluted the label bag: 195 of 660 distinct labels in the old
# store were counter labels, sp-reclaim alone having 96 forms because it encoded a cause
# suffix. Labels carry no validation, so each form was a new string that could disagree with
# the events trail.
#
# The new design is simpler: an attempt is a status_changed event whose new_value contains
# 'in_progress'. The events table is always populated by bd and cannot disagree with itself.
# The poison decision reads it on demand, never stores it.
#
# TWO ACCEPTANCE CRITERIA, TESTED HERE:
# 1. No counter label (sp-attempt-*, sp-reclaim-*, sp-requeue-*, sp-timeout-*, sp-recur-*) is
#    written by any harness path — asserted structurally on the code.
# 2. A bead cycled N times reports exactly N; unclaimed rows carrying the string do not
#    over-count — asserted against a real database with known event counts.
#
# A third criterion — the poison decision uses the events predicate, via a full sentinel.sh
# CHECK4 pass — used to live here too (D1: sp-eq8a4.2.2). It duplicated test-poison.sh's own
# events-based poison-at-threshold/below-threshold/stale-clear assertions one for one, on a
# second from-scratch sentinel fixture; criterion 2 above already proves attempts_of reads
# events, not labels, without needing a sentinel pass to demonstrate it a second time.
#
# FAIL-FIRST for criterion 2: each assertion below is run against the unfixed code first
# (where attempts_of reads labels), confirmed to fail, then run against the fixed code.
#
# A REAL bd ON A THROWAWAY DATABASE. The events are written by bd itself; a stub would be a
# second implementation of exactly the thing being asked about
# (law-prefer-the-real-dependency).
#
# defect: sp-lzt
# covers: spira/*.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-check4-events
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — sentinel CHECK4 reads its counters via bd sql, which embedded mode refuses
export SPIRA_TESTDB_MODE=server
testdb_up check4-events || {
    printf 'SKIP test-check4-events: server testdb not available\n' >&2
    exit 77
}
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-check4-events.sh"

# --------------------------------------------------------------------------------------
# CRITERION 1: No counter label is written by any harness path.
#
# Structural assertion on the code: grep every non-test, non-lib script for the label
# prefixes. The label-writing functions (bump_counter) are in lib.sh; every caller reaches
# them through the bump_* aliases. The check here is that no script calls bdq label add
# with a counter label directly AND that the bump_* functions do not write labels.
# --------------------------------------------------------------------------------------
echo
echo "criterion 1: no counter label written by any harness path"

BANNED='sp-attempt-|sp-reclaim-|sp-reclaim$|sp-requeue-|sp-requeue$|sp-timeout-|sp-recur-'
# Only scripts that can write — test suites excluded, lib.sh excluded (it defines the fns).
found="$(grep -rlE "label add.*($BANNED)" "$HERE"/*.sh 2>/dev/null \
    | grep -v '/test-' | grep -v '/lib\.sh$' | grep -v '/attempts\.sh$' || true)"
is "no harness script writes counter labels directly" "" "$found"

# The bump_* functions must be no-ops (return without calling bdq label add).
for fn in bump_attempt bump_reclaim bump_requeue bump_timeout bump_recur; do
    body="$(sed -n "/^${fn}()/,/^}/p" "$HERE/lib.sh" 2>/dev/null)"
    has_label_add="$(grep -c 'bdq label add\|bump_counter' <<<"$body" || true)"
    is "$fn does not write a label" "0" "$has_label_add"
done

# --------------------------------------------------------------------------------------
# CRITERION 2: A bead cycled N times reports exactly N via the events trail.
#
# Each bd update to in_progress writes one status_changed event with new_value containing
# 'in_progress'. A claim followed by immediate release and re-claim is two events. An
# unclaimed status row (event_type != status_changed) does not count.
# --------------------------------------------------------------------------------------
echo
echo "criterion 2: attempts_of reads the events trail"

seed() {   # seed <id> — one open, claimable bead
    testdb_reset
    testdb_seed <<JSONL
{"id":"$1","title":"a bead","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-06T00:00:00Z"}
JSONL
}

num() { local v="$1"; printf '%d' "${v:-0}"; }
cycle_to_inprogress() {   # cycle_to_inprogress <id> <n> — n transitions to in_progress
    local id="$1" n="$2" i=0
    while [ "$i" -lt "$n" ]; do
        bdq update "$id" --status in_progress >/dev/null 2>&1
        bdq update "$id" --status open >/dev/null 2>&1
        i=$((i+1))
    done
    # Leave bead in open state; the last update above opens it back.
}

# POSITIVE CONTROL FIRST. A fresh bead with no events has zero attempts.
seed sp-ev1
is "a fresh bead with no status changes has 0 attempts" "0" "$(num "$(attempts_of sp-ev1)")"

# POSITIVE CONTROL: one in_progress transition is one attempt.
seed sp-ev2
bdq update sp-ev2 --status in_progress >/dev/null 2>&1
is "one in_progress transition is one attempt" "1" "$(num "$(attempts_of sp-ev2)")"

# THREE CYCLES: each bead transitions to in_progress N times and reports exactly N.
seed sp-ev3
cycle_to_inprogress sp-ev3 3
is "three cycles report exactly 3"   "3" "$(num "$(attempts_of sp-ev3)")"

seed sp-ev4
cycle_to_inprogress sp-ev4 1
is "one cycle reports exactly 1"     "1" "$(num "$(attempts_of sp-ev4)")"

# NO OVER-COUNT FROM OTHER EVENT TYPES. A label_added event whose comment mentions
# 'in_progress' must not be counted — the filter on event_type='status_changed' prevents it.
# bd label add creates label_added events, not status_changed ones.
seed sp-ev5
bdq label add sp-ev5 "in_progress-fake" >/dev/null 2>&1
is "a label containing 'in_progress' does not count" "0" "$(num "$(attempts_of sp-ev5)")"

# THE KEY ACCEPTANCE: without any sp-attempt-* labels on the bead, three cycles still
# register as three attempts. The unfixed code (reading labels) returns 0 here, which
# is the failure case that this test exists to detect.
seed sp-ev6
cycle_to_inprogress sp-ev6 3
labels="$(bdq label list sp-ev6 2>/dev/null)" || labels=""
nowant "no sp-attempt-* labels exist on the cycled bead" "sp-attempt-" "$labels"
is "yet three cycles still count as 3 attempts" "3" "$(num "$(attempts_of sp-ev6)")"

tl_summary
