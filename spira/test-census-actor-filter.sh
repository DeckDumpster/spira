#!/usr/bin/env bash
#
# test-census-actor-filter.sh — census ranks only harness/aeon-* written events.
#
#   ./test-census-actor-filter.sh
#
# WHAT THIS SUITE GUARDS
# ----------------------
# sp-m4xp9: an operator zeroing an attempt-ledger offset writes a requeued event by
# hand (actor=overseer, new_value='unjudged-<cause>'). _census_events_sql had no
# actor predicate, so that hand-written row ranked as a failure class no harness code
# path could ever stop emitting — sp-requeue-unjudged-branch-collision reached #2 in
# a real Maechen census with 6 distinct beads. Fixed by an actor predicate
# (actor = 'harness' OR actor LIKE 'aeon-%') on the ranked query; excluded rows are
# listed separately by census_handwritten_run_sql / handwritten.py.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control, law-a-regression-test-must-
# be-seen-to-fail): the discriminating fact is the actor, not the cause string, so
# both rows below carry the IDENTICAL cause and only the actor differs. Run against
# the unfixed lib.sh, this suite failed:
#   "ranked block includes the aeon-written class: wanted [sp-requeue-same-cause] in []"
# — the unfixed query has no GROUP BY/actor split, so it folds the aeon row and the
# overseer row into ONE class count and (transiently, depending on grouping order)
# can drop or double list either; the fix makes the actor split observable and
# deterministic. Verified against this tree's pre-fix lib.sh before the fix landed.
#
# A REAL bd ON A THROWAWAY DATABASE (law-prefer-the-real-dependency). Requires server
# mode: census_events_run_sql/census_handwritten_run_sql use bd sql, which bd-embedded
# refuses. Skips when server testdb is not available.
#
# tier: T2
# covers: census/src/* spira/lib.sh spira/census/handwritten.py spira/census/classmap.py spira/census/count.py
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-census-actor-filter
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — census_events_run_sql/census_handwritten_run_sql use bd sql directly
export SPIRA_TESTDB_MODE=server
testdb_up census-actor-filter || {
    printf 'SKIP test-census-actor-filter: server testdb not available\n' >&2
    exit 77
}
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-census-actor-filter.sh"

census_out() {
    SPIRA_DB="$TESTDB_DIR" census --with-suppressed 2>/dev/null
}

# ======================================================================================
echo
echo "sp-m4xp9: aeon-written row ranked, overseer-written row (same cause) is not"
# ======================================================================================
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-af1","title":"aeon-written bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-26T00:00:00Z"}
{"id":"sp-af2","title":"hand-written bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-26T00:00:00Z"}
JSONL
BEADS_ACTOR="aeon-test" bump_requeue "sp-af1" same-cause >/dev/null 2>&1
BEADS_ACTOR="overseer"  bump_requeue "sp-af2" same-cause >/dev/null 2>&1

out="$(census_out)"
_ranked="$(printf '%s\n' "$out" | grep -v '^hand-written' || true)"
_handwritten="$(printf '%s\n' "$out" | grep '^hand-written' || true)"

want   "ranked block includes the aeon-written class"     "1 sp-requeue-same-cause" "$_ranked"
nowant "ranked block does not carry the overseer bead's count" "2 sp-requeue-same-cause" "$_ranked"
want   "hand-written heading names sp-requeue-same-cause"  "sp-requeue-same-cause"   "$_handwritten"
want   "hand-written heading names the actor"              "actor overseer"          "$_handwritten"
nowant "hand-written heading does not name the aeon actor" "actor aeon-test"         "$_handwritten"

# ======================================================================================
echo
echo "sp-m4xp9: without --with-suppressed the hand-written heading is gone, the ranked count still excludes the overseer row"
# ======================================================================================
_default_out="$(SPIRA_DB="$TESTDB_DIR" census 2>/dev/null)"
nowant "default output carries no hand-written heading" "hand-written" "$_default_out"
want   "default output still ranks the aeon-written bead alone" "1 sp-requeue-same-cause" "$_default_out"
nowant "default output does not fold in the overseer bead's count" "2 sp-requeue-same-cause" "$_default_out"

# ======================================================================================
echo
echo "sp-m4xp9: real classes with wide aeon authorship are unaffected — sp-reopen-rebase-conflict, sp-recur-unclaimable"
# ======================================================================================
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-af3","title":"rebase conflict bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-26T00:00:00Z"}
{"id":"sp-af4","title":"unclaimable recur bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-26T00:00:00Z"}
JSONL
BEADS_ACTOR="aeon-yojimbo" bump_requeue "sp-af3" rebase-conflict >/dev/null 2>&1
BEADS_ACTOR="aeon-anima"   bump_recur   "sp-af4" unclaimable     >/dev/null 2>&1

out="$(census_out)"
want "sp-reopen-rebase-conflict still ranked with unchanged count" "1 sp-reopen-rebase-conflict" "$out"
want "sp-recur-unclaimable still ranked with unchanged count"      "1 sp-recur-unclaimable"       "$out"

echo
tl_summary
