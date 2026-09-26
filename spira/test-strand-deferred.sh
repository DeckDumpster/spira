#!/usr/bin/env bash
#
# test-strand-deferred.sh — the deferred-unescalated check exempts beads whose blocking
#   deps include an open or in-progress bead, and fires for beads with all blockers closed.
#
#   ./test-strand-deferred.sh
#
# WHY THIS EXISTS. sp-hg8q: the check escalated deferred beads gated behind a live blocking
# edge — beads that are doing exactly what the DAG requires. The false positive landed on the
# operator's pane as a P1 decision with a default that would undo correct sequencing. The fix:
# skip a deferred bead when at least one of its "blocks" deps is open or in_progress; escalate
# when all known blockers are closed or absent.
#
# FIVE CASES, ALL SIDES EXERCISED (law-absence-needs-a-positive-control):
#
#   1. LIVE BLOCKER (in_progress dep) — NOT escalated.
#      Without this half, a classifier that never fires looks correct.
#
#   2. DEAD BLOCKER (all deps closed) — IS escalated.
#      Without this half, a classifier that always exempts looks correct.
#
#   3. NO BLOCKERS (no deps) — IS escalated.
#      A deferred bead with no reason is the original defect this check exists for.
#
#   4. MIXED BLOCKERS (one live + one closed) — NOT escalated.
#      One live blocker is sufficient; closed ones do not revoke the exemption.
#
#   5. ASK-LABELLED (carries ask label) — NOT escalated regardless of blockers.
#      Already in the operator's queue; double-reporting adds noise.
#
# EXERCISED VIA FIXTURE FILES, not a live database. strand-classify.py reads BEADS_FILE and
# READY_FILE; the classifier is what is under test, and the DB-to-JSON representation is a
# separate seam verified by test-check2-reclaim / test-check5-invariant.
#
# defect: sp-hg8q
# tier: T1
# covers: spira/strand-classify.py
# hermetic-ok: no database, no systemd, no network
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

classify() {
    local beads_json="$1" ready_json="${2:-[]}"
    printf '%s' "$beads_json" > "$TMP/beads.json"
    printf '%s' "$ready_json" > "$TMP/ready.json"
    BEADS_FILE="$TMP/beads.json" \
    READY_FILE="$TMP/ready.json" \
    HOLDERS="" LIVE=0 GHOST_GRACE=300 \
    SPIRA_ASK_LABEL=needs-operator \
        python3 "$HERE/strand-classify.py"
}

echo "test-strand-deferred.sh"

# ======================================================================================
echo
echo "case 1 — live blocker: deferred bead with an in_progress dep is NOT escalated:"
# ======================================================================================
# sp-blocked is deferred with no ask label. Its dep sp-blocker is in_progress — an aeon is
# actively working it. The DAG is doing its job; the check must not page the operator.
out="$(classify '[
  {"id":"sp-blocker","title":"live work","status":"in_progress","labels":["spira","plan"]},
  {"id":"sp-blocked","title":"waiting downstream","status":"deferred","labels":["spira","plan"],
   "dependencies":[{"issue_id":"sp-blocked","depends_on_id":"sp-blocker","type":"blocks"}]}
]')"
nowant "live blocker: deferred-unescalated NOT raised" "deferred-unescalated" "$out"

# ======================================================================================
echo
echo "case 2 — dead blocker: deferred bead with all blockers closed IS escalated:"
# ======================================================================================
# sp-blocker is now closed. sp-blocked is still deferred with no ask label and no live
# blocker to justify its status. The check must escalate.
out="$(classify '[
  {"id":"sp-blocker","title":"done work","status":"closed","labels":["spira","plan"]},
  {"id":"sp-blocked","title":"waiting downstream","status":"deferred","labels":["spira","plan"],
   "dependencies":[{"issue_id":"sp-blocked","depends_on_id":"sp-blocker","type":"blocks"}]}
]')"
want "dead blocker: deferred-unescalated IS raised" "deferred-unescalated" "$out"
want "dead blocker: sp-blocked named" "sp-blocked" "$out"

# ======================================================================================
echo
echo "case 3 — no blockers: deferred bead with no deps IS escalated:"
# ======================================================================================
# No deps at all — the original defect this check exists for. Must escalate.
out="$(classify '[
  {"id":"sp-naked","title":"naked deferred","status":"deferred","labels":["spira","plan"]}
]')"
want "no blockers: deferred-unescalated IS raised" "deferred-unescalated" "$out"
want "no blockers: sp-naked named" "sp-naked" "$out"

# ======================================================================================
echo
echo "case 4 — mixed blockers: one open + one closed dep → NOT escalated:"
# ======================================================================================
# One live blocker is sufficient; a closed dep alongside it does not revoke the exemption.
out="$(classify '[
  {"id":"sp-live1","title":"still open","status":"open","labels":["spira","plan"]},
  {"id":"sp-done1","title":"already closed","status":"closed","labels":["spira","plan"]},
  {"id":"sp-mixed","title":"waiting on two","status":"deferred","labels":["spira","plan"],
   "dependencies":[
     {"issue_id":"sp-mixed","depends_on_id":"sp-live1","type":"blocks"},
     {"issue_id":"sp-mixed","depends_on_id":"sp-done1","type":"blocks"}
   ]}
]')"
nowant "mixed blockers: deferred-unescalated NOT raised" "deferred-unescalated" "$out"

# ======================================================================================
echo
echo "case 5 — ask-labelled: deferred bead carrying ask label is exempt regardless:"
# ======================================================================================
# Already in the operator's queue. Double-reporting it here adds noise.
out="$(classify '[
  {"id":"sp-asked","title":"operator decision","status":"deferred","labels":["spira","plan","needs-operator"]}
]')"
nowant "ask-labelled: deferred-unescalated NOT raised" "deferred-unescalated" "$out"

tl_summary
