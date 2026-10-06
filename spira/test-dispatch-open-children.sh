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
# THE FIX. CHECK 3c (the sentinel binary, `sentinel --open-children`; sp-du8bv moved it out of
# lib.sh's mark_open_children) applies SPIRA_OPEN_CHILDREN_LABEL to a bead with any non-closed
# child (from the store snapshot), the same out-of-band-label seam mark_queue_waiters
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
# covers: spira/lib.sh sentinel/src/* spira/conf.sh spira-claim/*
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
tl_config SPIRA_RUN="$RUN"
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

# A CHILD'S STATE IS ITS LIFECYCLE ROW (sp-mve9i, design §3.4): CHECK 3c asks `spira-lc list`
# whether a child is still open (not terminal), never bd status. The stand-in on PATH answers
# from the rows seed/kid_closed declare.
lc_path_stub "$TMP/lcbin" "$TMP/lcfix"

# CHECK 3c alone, through the binary on PATH (the tree's own build under testenv).
mark_open_children() { PATH="$TMP/lcbin:$PATH" sentinel --open-children >/dev/null 2>&1; }

seed() {
    testdb_reset
    testdb_seed <<JSONL
{"id":"sp-parent","title":"parent with open children","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-kid1","title":"child one","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-kid1","depends_on_id":"sp-parent","type":"parent-child"}]}
{"id":"sp-kid2","title":"child two","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-kid2","depends_on_id":"sp-parent","type":"parent-child"}]}
{"id":"sp-lonely","title":"no children","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
    rm -rf "$LC_FIX/bead" "$LC_FIX/show"
    for id in sp-parent sp-kid1 sp-kid2 sp-lonely; do lc_bead READY "$id" "" 0; done
}

# kid_closed <n> — child sp-kid<n>'s row as closed: FIXTURE STATE, declared as data (an upsert
# of the same row), never a bd close driven around the lifecycle machine (sp-hyo5e).
kid_closed() {
    testdb_seed <<JSONL
{"id":"sp-kid$1","title":"child $1","status":"closed","closed_at":"$(date -u +%Y-%m-%dT%H:%M:%SZ)","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"$(date -u +%Y-%m-%dT%H:%M:%SZ)","dependencies":[{"issue_id":"sp-kid$1","depends_on_id":"sp-parent","type":"parent-child"}]}
JSONL
    lc_bead LANDED "sp-kid$1" "" 0   # the child is done: its lifecycle row is terminal
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
mark_open_children
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
mark_open_children
has   "5: second mark_open_children call leaves label in place" \
      "$LABEL" "$(labels_of sp-parent)"

# =====================================================================================
echo
echo "assertion 6 — one of two children closes: label stays, ALL children must close:"
# =====================================================================================
kid_closed 1
mark_open_children
has   "6: one child still open — sp-parent stays labeled" \
      "$LABEL" "$(labels_of sp-parent)"

# =====================================================================================
echo
echo "assertions 7-8 — last child closes: label removed, bead is summonable again:"
# =====================================================================================
kid_closed 2
mark_open_children
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
