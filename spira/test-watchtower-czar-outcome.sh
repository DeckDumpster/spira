#!/usr/bin/env bash
#
# test-watchtower-czar-outcome.sh — --czar-outcome-check escalates unclaimed and
#   not-cleared czar-trigger beads; --queue-checks (the old path) cannot.
#
# WHAT THIS SUITE IS FOR
# ----------------------
# The czar closes the bead it was summoned to handle. That is not evidence the
# condition cleared — it is evidence that the czar ran (law-measure-the-outcome).
# A new bead for the same class after the outcome window means the condition returned.
# This suite verifies that --czar-outcome-check detects both UNCLAIMED and NOT_CLEARED
# beads, deduplicates escalations per bead, and skips when the world is halted.
#
# POSITIVE CONTROL COMES FIRST (law-absence-needs-a-positive-control). Each group
# plants a fixture the detector is meant to find and requires it to fire before any
# absence assertion is believed. The first group also runs --queue-checks against the
# same fixture to show that path cannot catch czar outcomes — the new subcommand is
# the only mechanism.
#
# TEST AGAINST THE REAL DEPENDENCY (law-prefer-the-real-dependency). Beads are written
# through bd import and queried through bd list --json, exactly as the production path
# does. A hand-written stub of bd's JSON output would reproduce only the fields we
# remembered; the seam between writer and reader is what the test exists to cover.
#
# covers: spira/watchtower.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-watchtower-czar-outcome.sh"

testdb_require test-watchtower-czar-outcome
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonspiaqct \
    || { echo "test-watchtower-czar-outcome: could not build fixture database"; exit 1; }

NOW="$(date +%s)"
minsago() { date -u -d "@$(( NOW - ($1 * 60) ))" +%Y-%m-%dT%H:%M:%SZ; }
AGO5="$(minsago 5)"
AGO15="$(minsago 15)"
AGO35="$(minsago 35)"
AGO45="$(minsago 45)"
AGO50="$(minsago 50)"

# Mock incident.sh: bakes absolute capture-file paths into the script so that the
# env -i subprocess writes to them without inheriting any ambient variable.
MOCK_INC="$TMP/mock-inc.sh"
{
    printf '#!/usr/bin/env bash\n'
    printf 'printf "%%s\\n" "$2" >> "%s"\n'              "$TMP/inc-subjects"
    printf 'printf "%%s\\n" "${SPIRA_INCIDENT_REF:-}" >> "%s"\n' "$TMP/inc-refs"
    printf 'cat > /dev/null\n'
} > "$MOCK_INC"
chmod +x "$MOCK_INC"

fresh() {
    rm -rf "$TMP/run"
    mkdir -p "$TMP/run"
    rm -f "$TMP/inc-subjects" "$TMP/inc-refs"
    testdb_reset
}

# Run --czar-outcome-check in an explicit minimal environment. SPIRA_PATH is forwarded
# so that conf.sh's PATH rebuild (which reads SPIRA_PATH) does not lose the bd-embedded
# shim that testdb_up prepended.
wt_co() {   # wt_co [VAR=val ...]
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_BD="$SPIRA_BD" \
        SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_INCIDENT_SH="$MOCK_INC" \
        "$@" bash "$HERE/watchtower.sh" --czar-outcome-check 2>/dev/null
}

# Run --queue-checks in the same environment (for the "old path cannot catch this" control).
wt_qc() {   # wt_qc [VAR=val ...]
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_BD="$SPIRA_BD" \
        SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_INCIDENT_SH="$MOCK_INC" \
        SPIRA_QUEUE_LOG="$TMP/run/landing.log" \
        SPIRA_QUEUE_CHECK_MARKER="$TMP/run/queue-check.swept" \
        "$@" bash "$HERE/watchtower.sh" --queue-checks 2>/dev/null
}

# ======================================================================================
echo
echo "positive control — UNCLAIMED: open bead past threshold fires escalation:"
# ======================================================================================
# Plant an open czar-trigger bead created 15 minutes ago with the unclaimed threshold
# at 10 minutes. The detector must fire before any absence assertion is believed.
# Also show --queue-checks against the same fixture produces no czar-outcome escalation
# — the old path has no mechanism to detect this.
fresh
testdb_seed <<JSONL
{"id":"sp-czoc1","title":"CZAR: queue-deadlock-batch-open","status":"open","issue_type":"task","labels":["czar-trigger","spira"],"external_ref":"incident:queue-deadlock-batch-open","created_at":"$AGO15"}
JSONL

wt_co SPIRA_CZAR_UNCLAIMED_MINS=10
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "UNCLAIMED: fires on bead past threshold"        "CZAR:" "$subjects"
want "UNCLAIMED: subject names the bead id"           "sp-czoc1" "$subjects"
want "UNCLAIMED: subject names the class"             "unclaimed" "$subjects"

# --queue-checks against the same fixture: no czar-outcome escalation.
# This is the "fails against the old path" control: the queue-checks subcommand has
# no mechanism for czar outcomes, so the fixture that must escalate does not.
rm -f "$TMP/inc-subjects"
wt_qc SPIRA_CZAR_UNCLAIMED_MINS=10
qc_subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "--queue-checks cannot detect unclaimed czar beads" "CZAR:" "$qc_subjects"

# ======================================================================================
echo
echo "positive control — NOT_CLEARED: condition returned after czar closed its bead:"
# ======================================================================================
# Two beads with the same external_ref (same class):
#   A — closed 45 minutes ago (outcome window of 30 minutes has elapsed)
#   B — opened 35 minutes ago, AFTER A was closed (condition returned)
# The detector must identify A as NOT_CLEARED.
fresh
testdb_seed <<JSONL
{"id":"sp-czoc2a","title":"CZAR: queue-attribution-failed-requeue","status":"closed","issue_type":"task","labels":["czar-trigger","spira"],"external_ref":"incident:queue-attribution-failed-requeue","created_at":"$AGO50","closed_at":"$AGO45"}
{"id":"sp-czoc2b","title":"CZAR: queue-attribution-failed-requeue (recurrence)","status":"open","issue_type":"task","labels":["czar-trigger","spira"],"external_ref":"incident:queue-attribution-failed-requeue","created_at":"$AGO35"}
JSONL

wt_co SPIRA_CZAR_OUTCOME_MINS=30
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "NOT_CLEARED: fires when condition returned after outcome window" "CZAR:" "$subjects"
want "NOT_CLEARED: subject names the closed bead id"                  "sp-czoc2a" "$subjects"
want "NOT_CLEARED: subject names the class"                           "not-cleared" "$subjects"

# ======================================================================================
echo
echo "within threshold — open bead younger than unclaimed threshold: no escalation:"
# ======================================================================================
fresh
testdb_seed <<JSONL
{"id":"sp-czoc3","title":"CZAR: queue-sort-failed-ranking","status":"open","issue_type":"task","labels":["czar-trigger","spira"],"external_ref":"incident:queue-sort-failed-ranking","created_at":"$AGO5"}
JSONL

wt_co SPIRA_CZAR_UNCLAIMED_MINS=10
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "5-minute-old bead below 10-minute threshold: no escalation" "CZAR:" "$subjects"

# ======================================================================================
echo
echo "closed, no recurrence — czar handled it and the condition did not return:"
# ======================================================================================
# Bead closed 35 minutes ago, outcome window of 30 minutes elapsed, but no newer bead
# for the same class. The detector must stay silent.
fresh
testdb_seed <<JSONL
{"id":"sp-czoc4","title":"CZAR: queue-loop-stalled","status":"closed","issue_type":"task","labels":["czar-trigger","spira"],"external_ref":"incident:queue-loop-stalled","created_at":"$AGO50","closed_at":"$AGO35"}
JSONL

wt_co SPIRA_CZAR_OUTCOME_MINS=30
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "closed bead with no recurrence: no not-cleared escalation" "CZAR:" "$subjects"

# ======================================================================================
echo
echo "outcome window not yet elapsed — closed bead less than OUTCOME_MINS old:"
# ======================================================================================
# Bead closed 15 minutes ago with the same class's recurrence at 5 minutes ago.
# The outcome window is 30 minutes. Because 15 < 30, the window has not elapsed and
# no escalation fires — the czar still has time to act.
fresh
testdb_seed <<JSONL
{"id":"sp-czoc5a","title":"CZAR: queue-deadlock-batch-open","status":"closed","issue_type":"task","labels":["czar-trigger","spira"],"external_ref":"incident:queue-deadlock-batch-open","created_at":"$AGO50","closed_at":"$AGO15"}
{"id":"sp-czoc5b","title":"CZAR: queue-deadlock-batch-open (recurrence)","status":"open","issue_type":"task","labels":["czar-trigger","spira"],"external_ref":"incident:queue-deadlock-batch-open","created_at":"$AGO5"}
JSONL

wt_co SPIRA_CZAR_OUTCOME_MINS=30
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "outcome window not elapsed: no not-cleared escalation" "CZAR:" "$subjects"

# ======================================================================================
echo
echo "halted world — no escalations when world.halted exists:"
# ======================================================================================
# A deliberately halted world must produce no incidents. The same UNCLAIMED fixture
# that fires above must be silent when world.halted is present.
fresh
testdb_seed <<JSONL
{"id":"sp-czoc6","title":"CZAR: queue-deadlock-batch-open","status":"open","issue_type":"task","labels":["czar-trigger","spira"],"external_ref":"incident:queue-deadlock-batch-open","created_at":"$AGO15"}
JSONL
printf 'halted\n' > "$TMP/run/world.halted"

wt_co SPIRA_CZAR_UNCLAIMED_MINS=10
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "halted world: no escalations" "CZAR:" "$subjects"

# Positive control for the halt guard: same fixture without world.halted still fires.
rm -f "$TMP/run/world.halted" "$TMP/inc-subjects"
wt_co SPIRA_CZAR_UNCLAIMED_MINS=10
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "running world: the same fixture escalates" "CZAR:" "$subjects"

# ======================================================================================
echo
echo "incident refs are per-bead and stable across runs:"
# ======================================================================================
# SPIRA_INCIDENT_REF must be the same on every call for the same bead so that
# incident.sh dedup bumps a recurrence rather than filing a new bead on each pass.
fresh
testdb_seed <<JSONL
{"id":"sp-czoc7","title":"CZAR: queue-deadlock-batch-open","status":"open","issue_type":"task","labels":["czar-trigger","spira"],"external_ref":"incident:queue-deadlock-batch-open","created_at":"$AGO15"}
JSONL

wt_co SPIRA_CZAR_UNCLAIMED_MINS=10
wt_co SPIRA_CZAR_UNCLAIMED_MINS=10
refs="$(cat "$TMP/inc-refs" 2>/dev/null || echo "")"
unique="$(printf '%s\n' "$refs" | sort -u | grep -c . 2>/dev/null || echo 0)"
is   "two runs produce the same incident ref for the same bead"  "1" "$unique"
want "ref names the bead id (incident:czar-unclaimed-sp-czoc7)" "czar-unclaimed" "$refs"
want "ref contains the bead id"                                  "sp-czoc7"       "$refs"

# ======================================================================================
echo
echo "non-czar-trigger labels are ignored:"
# ======================================================================================
# A bead with a different label (incident) but the same external_ref prefix must not
# trigger an escalation — only czar-trigger labelled beads are in scope.
fresh
testdb_seed <<JSONL
{"id":"sp-czoc8","title":"some incident","status":"open","issue_type":"task","labels":["incident","spira"],"external_ref":"incident:queue-deadlock-batch-open","created_at":"$AGO15"}
JSONL

wt_co SPIRA_CZAR_UNCLAIMED_MINS=10
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "non-czar-trigger label not escalated" "CZAR:" "$subjects"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
