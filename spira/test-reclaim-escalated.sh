#!/usr/bin/env bash
#
# test-reclaim-escalated.sh — end-to-end: an escalated bead (IN_PROGRESS, waiting on an
#   unanswered operator ask dep) is not reclaimed by either the time-based reaper (CHECK 2)
#   or the ghost check (CHECK 2b).
#
#   ./test-reclaim-escalated.sh
#
# WHY THIS EXISTS. sp-9zpm: sp-mfa4 (IN_PROGRESS, waiting on sp-rmnw, an unanswered
#   needs-ryan ask) was reclaimed six times — the time-based reaper and the /proc ghost
#   check each treated it as a dead worker. Two fixes closed both paths:
#     - sp-rzyl: check2_protect_waiting applies SPIRA_RECLAIM_SKIP_LABEL to a work bead
#       whose only open dep carries the ask label; bdq reclaim --exclude-label skips it.
#     - sp-qsa1: strand-classify.py exempts beads carrying either the ask or skip label
#       from ghost classification.
#
#   test-check2-reclaim.sh verifies the skip label is applied/removed by protect_waiting.
#   test-strand-partition.sh's ghost-classifier cases verify strand-classify.py respects
#   the labels in isolation (D7: merged from the now-deleted test-reclaim-needs-ryan.sh).
#   THIS TEST verifies the chain end-to-end: protect_waiting labels the bead in the real
#   database, the label is present in the data extracted from the database and fed to the
#   ghost check, and the ghost check does not raise ghost for the labeled bead.
#
# THREE CASES (all sides exercised — law-absence-needs-a-positive-control). The plain
# dead-worker positive control lives with the rest of the classifier table in
# test-strand-partition.sh (D7) — this file keeps only the chain a hermetic classifier
# fixture cannot exercise: a real database write reaching the classifier's input.
#
#   1. CHAIN — BEFORE PROTECTION: work bead has expired lease + ask dep, but protect_waiting
#      has not run yet (no SKIP label on the bead). Ghost IS raised — proving the fix was
#      necessary and that a stale-lease work bead is ghost-eligible by default.
#
#   2. CHAIN — PROTECTED: protect_waiting applies SKIP to the work bead (the ask dep is
#      still open). The same bead, re-extracted from the real database, is NOT ghost. This
#      is the core assertion: the real-database label from protect_waiting propagates
#      correctly through to the ghost-check input.
#
#   3. CHAIN — AFTER ANSWER: the ask dep closes. protect_waiting removes SKIP. The bead,
#      re-extracted, IS ghost again — confirming the protection is lifted once an answer
#      arrives and the bead is returned to the reaper for normal reclaim.
#
# A real fixture database (testdb.sh) drives protect_waiting; JSON extracted from it feeds
# strand-classify.py. A past lease is injected into the extracted JSON so the time condition
# in the ghost check fires; the rest of the data is live from the DB.
#
# tier: T2
# defect: sp-9zpm
# covers: spira/lib.sh spira/strand-classify.py UC-dispatch-21
# hermetic-ok: uses a fixture database, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-reclaim-escalated
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up reclaimescalated || { echo "test-reclaim-escalated: could not build fixture database"; exit 1; }

# Stub what sentinel.sh defines but lib.sh needs.
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
acted=0; progressed=0
act()      { acted=$((acted+1)); }
progress() { progressed=$((progressed+1)); act "$@"; }
log()      { : ; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"

B() { bd -C "$SPIRA_DB" "$@"; }

# The ask and skip labels for these tests (fixture configuration).
ASK="${SPIRA_ASK_LABEL:-needs-operator}"
SKIP="${SPIRA_RECLAIM_SKIP_LABEL:-spira-waiting-operator}"

# Past timestamp: any bead with this lease is long expired.
PAST="2020-01-01T00:00:00Z"

# classify_with_lease — run strand-classify.py against a JSON array of beads, injecting a
# past lease_expires_at into every bead so the ghost check's time condition fires. The rest
# of the bead data (labels, status, assignee) is whatever the caller passes in — typically
# live data extracted from the fixture database.
classify_with_lease() {
    python3 -c '
import sys, json
data = json.loads(sys.argv[1])
if not isinstance(data, list): data = [data]
for b in data: b["lease_expires_at"] = sys.argv[2]
print(json.dumps(data))
' "$1" "$PAST" > "$TMP/beads.json"
    printf '[]' > "$TMP/ready.json"
    BEADS_FILE="$TMP/beads.json" \
    READY_FILE="$TMP/ready.json" \
    HOLDERS="" LIVE=1 GHOST_GRACE=0 \
    SPIRA_ASK_LABEL="$ASK" \
    SPIRA_RECLAIM_SKIP_LABEL="$SKIP" \
        python3 "$HERE/strand-classify.py"
}

echo "test-reclaim-escalated.sh"

# ======================================================================================
echo
echo "case 1 — chain before protection: work bead without skip label IS ghost:"
# ======================================================================================
# This is the exact pre-fix state: the work bead is IN_PROGRESS with an ask dep, but
# neither the ask label nor the skip label appears on the work bead itself. The ghost
# classifier sees only a stale-lease in_progress bead and raises ghost — exactly the
# behaviour that produced the sp-mfa4 respawn loop. This case proves the fix is necessary.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-ask0","title":"unanswered decision","status":"open","issue_type":"decision","labels":["$ASK","plan","spira"],"assignee":""}
{"id":"sp-work0","title":"work waiting for answer","status":"in_progress","issue_type":"task","labels":["plan","spira"],"assignee":"aeon-pre","dependencies":[{"depends_on_id":"sp-ask0","type":"blocks"}]}
JSONL
# Extract live bead data from the real database. No SKIP label on sp-work0 yet.
raw_json="$(bdjson list --all --status in_progress --limit 0)"
out="$(classify_with_lease "$raw_json")"
want   "before protection: ghost raised"  "ghost"    "$out"
want   "before protection: bead named"   "sp-work0"  "$out"

# ======================================================================================
echo
echo "case 2 — chain after protection: protect_waiting applies skip → NOT ghost:"
# ======================================================================================
# protect_waiting runs (ask dep is still open). It labels sp-work0 with SKIP.
# We re-extract from the real database — the SKIP label is now present in the live data.
# strand-classify.py must not raise ghost: the protection written to the database reaches
# the classifier, which is the end-to-end assertion this test exists to make.
acted=0
check2_protect_waiting
want   "protect_waiting applied skip label"  "$SKIP"  "$(B label list sp-work0 2>/dev/null)"
raw_json="$(bdjson list --all --status in_progress --limit 0)"
out="$(classify_with_lease "$raw_json")"
nowant "after protection: ghost NOT raised"  "ghost"    "$out"
nowant "after protection: bead NOT named"   "sp-work0"  "$out"

# ======================================================================================
echo
echo "case 3 — chain after answer: dep closes, skip removed → ghost IS raised:"
# ======================================================================================
# The operator answers. The ask dep closes. protect_waiting re-runs and removes SKIP
# from sp-work0 because no open dep still carries the ask label.
# Re-extracting from the database now shows no protection labels; the ghost check fires.
# This confirms the bead is correctly returned to the reaper once an answer arrives.
B close sp-ask0 >/dev/null 2>&1
acted=0
check2_protect_waiting
nowant "after answer: skip label removed"  "$SKIP"  "$(B label list sp-work0 2>/dev/null)"
raw_json="$(bdjson list --all --status in_progress --limit 0)"
out="$(classify_with_lease "$raw_json")"
want   "after answer: ghost IS raised"  "ghost"    "$out"
want   "after answer: bead named"      "sp-work0"  "$out"

tl_summary
