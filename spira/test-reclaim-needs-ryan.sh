#!/usr/bin/env bash
#
# test-reclaim-needs-ryan.sh — strand-classify.py's ghost check skips IN_PROGRESS beads
#   that carry the ask label (needs-ryan) or the reclaim-skip label (spira-waiting-operator),
#   and still fires for a plain dead-worker bead.
#
#   ./test-reclaim-needs-ryan.sh
#
# WHY THIS EXISTS. sp-2k5a: the sentinel's CHECK 2 reclaim cadence re-summoned an IN_PROGRESS
# bead (sp-mfa4) six times while it was correctly waiting on an unanswered needs-ryan ask
# (sp-rmnw). The fix for the time-based reaper (sp-rzyl / check2_protect_waiting) applied
# spira-waiting-operator to such beads so the sentinel's --exclude-label flag skipped them.
# But strand-classify.py's ghost case — the /proc-based, faster-acting reaper in CHECK 2b —
# had no equivalent exclusion: it classified ANY in_progress bead with an expired lease as
# a ghost and handed it to `bd reclaim`, bypassing the protection.
#
# THREE CASES, ALL SIDES EXERCISED (law-absence-needs-a-positive-control):
#
#   1. POSITIVE CONTROL (plain dead worker) — ghost IS raised.
#      Without this, an implementation that never raises ghost reads as correct.
#
#   2. PROTECTED (ask label — needs-operator directly on the bead) — ghost NOT raised.
#      The bead is legitimately waiting for an operator decision. Reclaiming it re-summons
#      an aeon that immediately re-derives the same diagnosis (sp-2k5a replay).
#
#   3. PROTECTED (skip label — spira-waiting-operator applied by check2_protect_waiting) —
#      ghost NOT raised. check2_protect_waiting shields beads whose only open dep carries
#      the ask label; the ghost check must honour the same shield.
#
# EXERCISED VIA FIXTURE FILES, not a live database. strand-classify.py reads BEADS_FILE
# and READY_FILE; the classifier is what is under test, and the DB-to-JSON representation
# is a separate seam verified by test-check2-reclaim / test-check5-invariant.
#
# defect: sp-2k5a
# covers: spira/strand-classify.py
# hermetic-ok: no database, no systemd, no network
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# Past timestamp — any lease set to this is long expired.
PAST="2020-01-01T00:00:00Z"

classify() {
    local beads_json="$1"
    printf '%s' "$beads_json" > "$TMP/beads.json"
    printf '[]' > "$TMP/ready.json"
    BEADS_FILE="$TMP/beads.json" \
    READY_FILE="$TMP/ready.json" \
    HOLDERS="" LIVE=1 GHOST_GRACE=0 \
    SPIRA_ASK_LABEL=needs-operator \
    SPIRA_RECLAIM_SKIP_LABEL=spira-waiting-operator \
        python3 "$HERE/strand-classify.py"
}

echo "test-reclaim-needs-ryan.sh"

# ======================================================================================
echo
echo "case 1 — positive control: plain dead worker IS classified ghost:"
# ======================================================================================
# A bead whose aeon died with no special protection. ghost must fire — without this,
# an implementation that never raises ghost is indistinguishable from a correct one.
out="$(classify '[
  {"id":"sp-dead","title":"dead worker","status":"in_progress",
   "labels":["spira","plan"],"assignee":"aeon-dead",
   "lease_expires_at":"'"$PAST"'"}
]')"
want  "plain dead worker: ghost raised"  "ghost"    "$out"
want  "plain dead worker: bead named"    "sp-dead"  "$out"

# ======================================================================================
echo
echo "case 2 — ask label directly: bead with needs-operator is NOT ghost:"
# ======================================================================================
# The exact replay of sp-2k5a: an aeon filed a needs-ryan decision bead and exited.
# The work bead carries needs-operator, its lease is stale, no live aeon holds it.
# The ghost check must skip it — reclaiming re-summons a session that re-derives the
# same diagnosis and exits again (the six-reclaim loop that prompted sp-2k5a).
out="$(classify '[
  {"id":"sp-ask","title":"escalated bead","status":"in_progress",
   "labels":["needs-operator","spira","plan"],"assignee":"aeon-x",
   "lease_expires_at":"'"$PAST"'"}
]')"
nowant "ask-labeled bead: ghost NOT raised"  "ghost"   "$out"
nowant "ask-labeled bead: bead NOT named"    "sp-ask"  "$out"

# ======================================================================================
echo
echo "case 3 — skip label (check2_protect_waiting): bead with spira-waiting-operator is NOT ghost:"
# ======================================================================================
# check2_protect_waiting (sp-rzyl) labels a work bead with spira-waiting-operator when its
# only open dep carries the ask label, so the sentinel's --exclude-label skips it in CHECK 2.
# The ghost check (CHECK 2b) must honour the same exclusion — otherwise strand.sh reclaims
# what CHECK 2 explicitly protected, and the protection is pointless.
out="$(classify '[
  {"id":"sp-protected","title":"waiting for ryan","status":"in_progress",
   "labels":["spira-waiting-operator","spira","plan"],"assignee":"aeon-y",
   "lease_expires_at":"'"$PAST"'"}
]')"
nowant "skip-labeled bead: ghost NOT raised"   "ghost"        "$out"
nowant "skip-labeled bead: bead NOT named"     "sp-protected" "$out"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
