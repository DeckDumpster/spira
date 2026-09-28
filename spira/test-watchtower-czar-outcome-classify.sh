#!/usr/bin/env bash
#
# test-watchtower-czar-outcome-classify.sh — watchtower-czar-outcome.py, the pure
#   function that decides UNCLAIMED and NOT_CLEARED from a JSON bead list.
#
#   ./test-watchtower-czar-outcome-classify.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# The czar closes the bead it was summoned to handle. That is not evidence the
# condition cleared — it is evidence that the czar ran (law-measure-the-outcome).
# A new bead for the same class after the outcome window means the condition returned.
# This suite verifies watchtower-czar-outcome.py identifies both UNCLAIMED and
# NOT_CLEARED beads from canned JSON — no bd, no watchtower.sh process launch. The one
# bd round-trip proving the real query feeds this script correctly is
# test-watchtower-czar-outcome.sh.
#
# POSITIVE CONTROL COMES FIRST (law-absence-needs-a-positive-control). Each group
# plants a fixture the classifier is meant to find and requires it to fire before any
# absence assertion is believed.
#
# tier: T1
# covers: spira/watchtower-czar-outcome.py UC-ops-detection-remediation-25
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

NOW="$(date +%s)"
minsago() { date -u -d "@$(( NOW - ($1 * 60) ))" +%Y-%m-%dT%H:%M:%SZ; }
AGO5="$(minsago 5)"
AGO15="$(minsago 15)"
AGO35="$(minsago 35)"
AGO45="$(minsago 45)"
AGO50="$(minsago 50)"

classify() {   # classify <outcome-mins> <unclaimed-mins> <json> -> UNCLAIMED/NOT_CLEARED lines
    local outcome_mins="$1" unclaimed_mins="$2" json="$3"
    printf '%s' "$json" > "$TMP/beads.json"
    python3 "$HERE/watchtower-czar-outcome.py" "$NOW" "$outcome_mins" "$unclaimed_mins" "$TMP/beads.json" \
        2>/dev/null
}

echo "test-watchtower-czar-outcome-classify.sh"

# ======================================================================================
echo
echo "positive control — UNCLAIMED: open bead past threshold fires:"
# ======================================================================================
out="$(classify 30 10 '[
  {"id":"sp-czoc1","status":"open","external_ref":"incident:queue-deadlock-batch-open","created_at":"'"$AGO15"'"}
]')"
want "UNCLAIMED: fires on bead past threshold" "UNCLAIMED sp-czoc1" "$out"

# ======================================================================================
echo
echo "positive control — NOT_CLEARED: condition returned after czar closed its bead:"
# ======================================================================================
# A — closed 45 minutes ago (outcome window of 30 minutes has elapsed)
# B — opened 35 minutes ago, AFTER A was closed (condition returned), and is itself
#     closed so the newest-bead-open `continue` does not skip the NOT_CLEARED check.
out="$(classify 30 10 '[
  {"id":"sp-czoc2a","status":"closed","external_ref":"incident:queue-attribution-failed-requeue","created_at":"'"$AGO50"'","closed_at":"'"$AGO45"'"},
  {"id":"sp-czoc2b","status":"closed","external_ref":"incident:queue-attribution-failed-requeue","created_at":"'"$AGO35"'","closed_at":"'"$AGO15"'"}
]')"
want "NOT_CLEARED: fires when condition returned after outcome window" "NOT_CLEARED sp-czoc2a" "$out"

# ======================================================================================
echo
echo "within threshold — open bead younger than unclaimed threshold: no escalation:"
# ======================================================================================
out="$(classify 30 10 '[
  {"id":"sp-czoc3","status":"open","external_ref":"incident:queue-sort-failed-ranking","created_at":"'"$AGO5"'"}
]')"
is "5-minute-old bead below 10-minute threshold: no escalation" "" "$out"

# ======================================================================================
echo
echo "closed, no recurrence — czar handled it and the condition did not return:"
# ======================================================================================
out="$(classify 30 10 '[
  {"id":"sp-czoc4","status":"closed","external_ref":"incident:queue-loop-stalled","created_at":"'"$AGO50"'","closed_at":"'"$AGO35"'"}
]')"
is "closed bead with no recurrence: no not-cleared escalation" "" "$out"

# ======================================================================================
echo
echo "outcome window not yet elapsed — closed bead less than OUTCOME_MINS old:"
# ======================================================================================
out="$(classify 30 10 '[
  {"id":"sp-czoc5a","status":"closed","external_ref":"incident:queue-deadlock-batch-open","created_at":"'"$AGO50"'","closed_at":"'"$AGO15"'"},
  {"id":"sp-czoc5b","status":"open","external_ref":"incident:queue-deadlock-batch-open","created_at":"'"$AGO5"'"}
]')"
is "outcome window not elapsed: no not-cleared escalation" "" "$out"

# ======================================================================================
echo
echo "a ref not shaped like incident:queue- is ignored:"
# ======================================================================================
# The bd query scopes this to czar-trigger-labelled beads (proven by the one bd
# round-trip in test-watchtower-czar-outcome.sh); the classifier's own half of that
# contract is the ref-prefix filter, which is what this asserts directly.
out="$(classify 30 10 '[
  {"id":"sp-czoc8","status":"open","external_ref":"incident:queue-deadlock-batch-open-other","created_at":"'"$AGO15"'"}
]')"
want "a queue- prefixed ref still fires (positive control for the row below)" "UNCLAIMED sp-czoc8" "$out"
out="$(classify 30 10 '[
  {"id":"sp-czoc9","status":"open","external_ref":"some-other-ref","created_at":"'"$AGO15"'"}
]')"
is "a non-queue- ref is ignored" "" "$out"

tl_summary
