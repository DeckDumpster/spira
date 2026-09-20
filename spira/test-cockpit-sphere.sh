#!/usr/bin/env bash
#
# test-cockpit-sphere.sh — SP_POISON counts all poisoned beads, not just plan-labelled ones.
#
#   ./test-cockpit-sphere.sh
#
# THE DEFECT THIS SUITE EXISTS FOR (sp-b3ub). cockpit.sh computed SP_POISON inside the
# spira,plan-scoped sphere-grid block. Poison is applied only by aeon.sh's closing rule
# when SOP_REQUIRED=1 — which fires for ops and qa personas whose beads carry `incident`,
# not `plan`. So the filter and the population were disjoint by construction, and SP_POISON
# was structurally always zero against eleven open poisoned beads on the day of discovery.
#
# THE FIX: sphere_keys() issues a separate query on the spira-poison label alone (status !=
# closed), independent of the sphere-grid scoping. This suite asserts that the value equals
# the raw count from bd list --label spira-poison on a fixture that carries ONLY incident-
# labelled poisoned beads — the population the old query could never have seen.
#
# EVERY CASE IS A PAIR (law-absence-needs-a-positive-control). The negative control
# (no poisoned beads) proves the check can read a true zero without returning ?. The positive
# control (at least one incident-labelled poisoned bead) proves it reads the real count
# rather than asking a question that cannot return a positive answer.
#
# defect: sp-b3ub
# covers: spira/cockpit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
testdb_require test-cockpit-sphere
TMP="$(mktemp -d)"
testdb_up sphere || { echo "test-cockpit-sphere: could not build a fixture database"; exit 1; }
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# Run sphere_keys under a controlled environment: real bd on the fixture database,
# no inherited configuration, no INVOCATION_ID (unsupervised path).
# $PATH is passed through (not a frozen BASE_PATH): by this point testdb_up has run,
# which sources conf.sh and adds the SPIRA_EXTRA_PATH entries that put bd on PATH.
RUN="$TMP/run"; mkdir -p "$RUN"
sphere() {
    env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
        SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
        SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-goal SPIRA_FAYTHS=t \
        SPIRA_SCOPE_LABEL="$SPIRA_SCOPE_LABEL" \
        SPIRA_ASK_LABEL=needs-ryan \
        bash "$HERE/cockpit.sh" sphere 2>/dev/null
}

B() { "${TESTDB_BD:-bd-embedded}" -C "$SPIRA_DB" "$@"; }

echo "test-cockpit-sphere.sh"

# ======================================================================================
# NEGATIVE CONTROL: empty database — no poisoned beads, SP_POISON should be 0, not ?.
# A counter that reads ? on an empty database is a broken probe, not a working one that
# found nothing (law-absence-needs-a-positive-control).
# ======================================================================================
echo
echo "empty database — no poisoned beads:"

testdb_reset
out="$(sphere)"
want   "SP_POISON is present in output"          "SP_POISON="  "$out"
nowant "SP_POISON is not ? with no beads"        "SP_POISON=?" "$out"
is     "SP_POISON is 0 with no poisoned beads"   "0" \
       "$(printf '%s\n' "$out" | sed -n 's/^SP_POISON=//p' | head -1)"

# ======================================================================================
# POSITIVE CONTROL: incident-labelled poisoned bead (no plan label).
# This is the population the old query never saw. With at least one open bead carrying
# spira-poison and not carrying plan, SP_POISON must equal the bd count.
# ======================================================================================
echo
echo "incident-labelled poisoned bead (no plan label):"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-inc1","title":"incident bead, poisoned","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","incident","spira-poison"],"updated_at":"2026-09-08T00:00:00Z"}
{"id":"sp-inc2","title":"second incident bead, poisoned","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","incident","spira-poison"],"updated_at":"2026-09-08T00:00:00Z"}
{"id":"sp-inc3","title":"incident bead, not poisoned","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","incident"],"updated_at":"2026-09-08T00:00:00Z"}
JSONL

out="$(sphere)"
bd_count="$(B list --all --limit 0 --label spira-poison --json 2>/dev/null \
    | sed -n '/^[[{]/,$p' \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(sum(1 for i in d if i.get("status")!="closed"))')"

want   "SP_POISON is present in output"                    "SP_POISON="  "$out"
nowant "SP_POISON is not ? with incident-poisoned beads"   "SP_POISON=?" "$out"
is     "SP_POISON matches the raw bd count (2)"            "$bd_count" \
       "$(printf '%s\n' "$out" | sed -n 's/^SP_POISON=//p' | head -1)"
is     "raw bd count is 2 (positive control is real)"      "2" "$bd_count"

# Verify the plan query still works independently: plan beads are counted for OPEN/INPROG.
testdb_seed <<JSONL
{"id":"sp-plan1","title":"plan bead, open","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-08T00:00:00Z"}
{"id":"sp-plan2","title":"plan bead, in progress","status":"in_progress","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-08T00:00:00Z"}
JSONL
out="$(sphere)"
want "SP_OPEN counts plan beads"   "SP_OPEN=2"  "$out"
want "SP_INPROG counts in-progress plan beads" "SP_INPROG=1" "$out"
# SP_POISON remains 2 — the plan beads carry no spira-poison.
is   "SP_POISON still counts only poisoned beads (still 2)" "2" \
     "$(printf '%s\n' "$out" | sed -n 's/^SP_POISON=//p' | head -1)"

# ======================================================================================
# CLOSED POISONED BEAD: must NOT be counted in SP_POISON.
# ======================================================================================
echo
echo "closed poisoned bead:"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-closed","title":"closed poisoned bead","status":"closed","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","incident","spira-poison"],"updated_at":"2026-09-08T00:00:00Z"}
{"id":"sp-open-p","title":"open poisoned bead","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","incident","spira-poison"],"updated_at":"2026-09-08T00:00:00Z"}
JSONL
out="$(sphere)"
is "closed poisoned bead is excluded; only open one counts" "1" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_POISON=//p' | head -1)"

# ======================================================================================
# PLAN-LABELLED POISONED BEAD: must still be counted — the fix is broader scope, not
# exclusion of plan beads. A plan bead that somehow reaches the poison valve must appear.
# ======================================================================================
echo
echo "plan-labelled poisoned bead is also counted:"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-plan-p","title":"plan bead that is also poisoned","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan","spira-poison"],"updated_at":"2026-09-08T00:00:00Z"}
JSONL
out="$(sphere)"
is "a plan-labelled poisoned bead is counted in SP_POISON" "1" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_POISON=//p' | head -1)"

echo
printf 'test-cockpit-sphere: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
