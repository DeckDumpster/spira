#!/usr/bin/env bash
#
# test-dispatch-open-children.sh — a bead with an open parent-child-linked child is not
# selected by the dispatcher's ready query; once every child closes, it is again.
#
# THE PROBLEM. bd refuses a parent-blocks-child dependency (it would cascade the block to
# every descendant, which would then never close), so nothing stops a coordination bead from
# reading as ready while its whole deliverable sits in open children. A dispatcher that
# claims it burns a session for zero actionable work, every time.
#
# THE FIX. mark_open_children (lib.sh) applies SPIRA_OPEN_CHILDREN_LABEL to a bead with any
# non-closed child (via `bd children`), the same out-of-band-label seam mark_queue_waiters
# uses for a predicate bd cannot express as a dependency. ready_shared_exclude carries the
# label into fayth_exclude and strand.sh's classify_one, so every "is this claimable"
# predicate — ready_count, fayth_ready, and the aeon claim built from CLAIM_EXCLUDE — excludes
# it without any change to the claim algorithm itself.
#
# WHAT THIS SUITE CHECKS.
#   1. POSITIVE CONTROL: with no fix applied, bd's own `bd ready` already lists the parent —
#      proving the defect is real before checking that the fix closes it.
#   2. mark_open_children applies the label to a parent with two open children.
#   3. The label is on the bead in the db.
#   4. fayth_ready excludes the labeled parent (count drops to 0).
#   5. Second pass is idempotent (no error, label stays).
#   6. Closing only ONE of two children leaves the label in place — ALL children must close.
#   7. Closing the second child and re-running mark_open_children removes the label.
#   8. fayth_ready counts the parent again once unlabeled.
#   9. A bead with no children is never labeled.
#
# defect: sp-zazkt
# tier: T2
# covers: spira/lib.sh sentinel/src/* spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
has()   { [[ "$3" == *"$2"* ]]   && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
lacks() { [[ "$3" != *"$2"* ]]   && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-dispatch-open-children
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up dispatch_open_children || { echo "test-dispatch-open-children: could not build fixture database"; exit 1; }

RUN="$TMP/run"; mkdir -p "$RUN"
export SPIRA_RUN="$RUN"
export SPIRA_HOME="$HERE"
export SPIRA_CONF="$TMP/no.conf"   # no host config leaking into the suite
log() { :; }                        # suppress log noise
# shellcheck disable=SC1090
. "$HERE/lib.sh"

B() { bd -C "$SPIRA_DB" "$@"; }
labels_of() {
    B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(" ".join(d[0].get("labels") or []))' 2>/dev/null
}
ready_ids() {   # ready_ids -> space-separated ids currently in the builder partition's ready set
    bdjson "${READY_ARGS[@]}" --label "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" \
        --exclude-label "$(ready_shared_exclude)" 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: d = []
print(" ".join(sorted(r.get("id","") for r in (d if isinstance(d, list) else [d]))))' 2>/dev/null
}

LABEL="${SPIRA_OPEN_CHILDREN_LABEL:-spira-open-children}"

seed() {
    testdb_reset
    testdb_seed <<JSONL
{"id":"sp-parent","title":"parent with open children","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-kid1","title":"child one","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-kid1","depends_on_id":"sp-parent","type":"parent-child"}]}
{"id":"sp-kid2","title":"child two","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-kid2","depends_on_id":"sp-parent","type":"parent-child"}]}
{"id":"sp-lonely","title":"no children","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
}

echo "test-dispatch-open-children.sh"

# =====================================================================================
echo
echo "assertion 1 — POSITIVE CONTROL: bd's own ready query lists the parent unfixed:"
# =====================================================================================
seed
has   "1: bd ready lists sp-parent before mark_open_children runs — the defect is real" \
      "sp-parent" "$(ready_ids)"

# =====================================================================================
echo
echo "assertions 2-4 — two open children: label applied, bead excluded from summon:"
# =====================================================================================
mark_open_children 2>/dev/null
has   "2: mark_open_children applies $LABEL to sp-parent" \
      "$LABEL" "$(labels_of sp-parent)"

has   "3: label is on the bead in the db" \
      "$LABEL" "$(B label list sp-parent 2>/dev/null)"

lacks "4: sp-parent no longer in the ready set (fayth_ready excludes it)" \
      "sp-parent" "$(ready_ids)"

# =====================================================================================
echo
echo "assertion 5 — second pass is idempotent:"
# =====================================================================================
mark_open_children 2>/dev/null
has   "5: second mark_open_children call leaves label in place" \
      "$LABEL" "$(labels_of sp-parent)"

# =====================================================================================
echo
echo "assertion 6 — one of two children closes: label stays, ALL children must close:"
# =====================================================================================
B close sp-kid1 --reason "done" >/dev/null 2>&1
mark_open_children 2>/dev/null
has   "6: one child still open — sp-parent stays labeled" \
      "$LABEL" "$(labels_of sp-parent)"

# =====================================================================================
echo
echo "assertions 7-8 — last child closes: label removed, bead is summonable again:"
# =====================================================================================
B close sp-kid2 --reason "done" >/dev/null 2>&1
mark_open_children 2>/dev/null
lacks "7: every child closed — label removed from sp-parent" \
      "$LABEL" "$(labels_of sp-parent)"

has   "8: sp-parent is back in the ready set" \
      "sp-parent" "$(ready_ids)"

# =====================================================================================
echo
echo "assertion 9 — a bead with no children is never labeled:"
# =====================================================================================
lacks "9: sp-lonely (no children) was never labeled" \
      "$LABEL" "$(labels_of sp-lonely)"

tl_summary
