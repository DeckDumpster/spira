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
# the count of open/in_progress spira-poison beads in the fixture — the population the old
# query could never have seen.
#
# EVERY CASE IS A PAIR (law-absence-needs-a-positive-control). The negative control
# (no poisoned beads) proves the check can read a true zero without returning ?. The positive
# control (at least one incident-labelled poisoned bead) proves it reads the real count
# rather than asking a question that cannot return a positive answer.
#
# SPIRA_BDJSON_FIXTURE, NOT A REAL STORE (docs/test-plan/cockpit-observability.md coverage
# row 13). sphere_keys issues two `list` calls (label=spira-poison, and label=<scope>,plan)
# against the same underlying bead set; bdsim.py's list filter reproduces bd's own AND-label
# and default-exclude-closed semantics so one fixture array answers both. The query shape
# itself is covered once, against real bd, in test-cockpit-bd-contract.sh.
#
# defect: sp-b3ub
# covers: spira/cockpit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

SCOPE_LABEL="spherescope"
RUN="$TMP/run"; mkdir -p "$RUN"
sphere() {    # sphere <fixture-file>
    env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
        SPIRA_BDJSON_FIXTURE="$1" \
        SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-goal SPIRA_FAYTHS=t \
        SPIRA_SCOPE_LABEL="$SCOPE_LABEL" \
        SPIRA_ASK_LABEL=needs-ryan \
        bash "$HERE/cockpit.sh" sphere 2>/dev/null
}
field() { printf '%s\n' "$1" | sed -n "s/^$2=//p" | head -1; }

echo "test-cockpit-sphere.sh"

# ======================================================================================
# NEGATIVE CONTROL: empty database — no poisoned beads, SP_POISON should be 0, not ?.
# A counter that reads ? on an empty database is a broken probe, not a working one that
# found nothing (law-absence-needs-a-positive-control).
# ======================================================================================
echo
echo "empty database — no poisoned beads:"
printf '[]' > "$TMP/empty.json"
out="$(sphere "$TMP/empty.json")"
want   "SP_POISON is present in output"          "SP_POISON="  "$out"
nowant "SP_POISON is not ? with no beads"        "SP_POISON=?" "$out"
is     "SP_POISON is 0 with no poisoned beads"   "0" "$(field "$out" SP_POISON)"

# ======================================================================================
# POSITIVE CONTROL: incident-labelled poisoned bead (no plan label).
# This is the population the old query never saw. Two open beads carry spira-poison
# without plan; SP_POISON must read 2, the real count, not the sphere-grid's 0.
# ======================================================================================
echo
echo "incident-labelled poisoned bead (no plan label):"
cat > "$TMP/incident.json" <<JSON
[
  {"id":"sp-inc1","title":"incident bead, poisoned","status":"open","issue_type":"task","labels":["$SCOPE_LABEL","incident","spira-poison"],"updated_at":"2026-09-08T00:00:00Z"},
  {"id":"sp-inc2","title":"second incident bead, poisoned","status":"open","issue_type":"task","labels":["$SCOPE_LABEL","incident","spira-poison"],"updated_at":"2026-09-08T00:00:00Z"},
  {"id":"sp-inc3","title":"incident bead, not poisoned","status":"open","issue_type":"task","labels":["$SCOPE_LABEL","incident"],"updated_at":"2026-09-08T00:00:00Z"}
]
JSON
out="$(sphere "$TMP/incident.json")"
want   "SP_POISON is present in output"                    "SP_POISON="  "$out"
nowant "SP_POISON is not ? with incident-poisoned beads"   "SP_POISON=?" "$out"
is     "SP_POISON counts the two poisoned incident beads"  "2" "$(field "$out" SP_POISON)"

# Verify the plan query still works independently: plan beads are counted for OPEN/INPROG,
# and SP_POISON is unaffected because the plan beads carry no spira-poison.
cat > "$TMP/incident-plus-plan.json" <<JSON
[
  {"id":"sp-inc1","title":"incident bead, poisoned","status":"open","issue_type":"task","labels":["$SCOPE_LABEL","incident","spira-poison"],"updated_at":"2026-09-08T00:00:00Z"},
  {"id":"sp-inc2","title":"second incident bead, poisoned","status":"open","issue_type":"task","labels":["$SCOPE_LABEL","incident","spira-poison"],"updated_at":"2026-09-08T00:00:00Z"},
  {"id":"sp-plan1","title":"plan bead, open","status":"open","issue_type":"task","labels":["$SCOPE_LABEL","plan"],"updated_at":"2026-09-08T00:00:00Z"},
  {"id":"sp-plan2","title":"plan bead, in progress","status":"in_progress","issue_type":"task","labels":["$SCOPE_LABEL","plan"],"updated_at":"2026-09-08T00:00:00Z"}
]
JSON
out="$(sphere "$TMP/incident-plus-plan.json")"
want "SP_OPEN counts plan beads"   "SP_OPEN=2"  "$out"
want "SP_INPROG counts in-progress plan beads" "SP_INPROG=1" "$out"
is   "SP_POISON still counts only poisoned beads (still 2)" "2" "$(field "$out" SP_POISON)"

# ======================================================================================
# CLOSED POISONED BEAD: must NOT be counted in SP_POISON.
# ======================================================================================
echo
echo "closed poisoned bead:"
cat > "$TMP/closed.json" <<JSON
[
  {"id":"sp-closed","title":"closed poisoned bead","status":"closed","issue_type":"task","labels":["$SCOPE_LABEL","incident","spira-poison"],"updated_at":"2026-09-08T00:00:00Z"},
  {"id":"sp-open-p","title":"open poisoned bead","status":"open","issue_type":"task","labels":["$SCOPE_LABEL","incident","spira-poison"],"updated_at":"2026-09-08T00:00:00Z"}
]
JSON
out="$(sphere "$TMP/closed.json")"
is "closed poisoned bead is excluded; only open one counts" "1" "$(field "$out" SP_POISON)"

# ======================================================================================
# PLAN-LABELLED POISONED BEAD: must still be counted — the fix is broader scope, not
# exclusion of plan beads. A plan bead that somehow reaches the poison valve must appear.
# ======================================================================================
echo
echo "plan-labelled poisoned bead is also counted:"
cat > "$TMP/plan-poisoned.json" <<JSON
[{"id":"sp-plan-p","title":"plan bead that is also poisoned","status":"open","issue_type":"task","labels":["$SCOPE_LABEL","plan","spira-poison"],"updated_at":"2026-09-08T00:00:00Z"}]
JSON
out="$(sphere "$TMP/plan-poisoned.json")"
is "a plan-labelled poisoned bead is counted in SP_POISON" "1" "$(field "$out" SP_POISON)"

echo
printf 'test-cockpit-sphere: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
