#!/usr/bin/env bash
#
# test-watchtower-czar-outcome.sh — one bd round-trip proving the real query feeds
#   watchtower's czar_outcome::classify correctly (sp-lnmbq inlined the classifier from
#   watchtower-czar-outcome.py, now deleted; every classification permutation that suite's
#   own test-watchtower-czar-outcome-classify.sh covered — now retired — is
#   czar_outcome::tests in the watchtower crate, table-tested against the same fixtures).
#
# WHAT THIS SUITE IS FOR
# ----------------------
# classify() never sees a label — the query `bd list --label czar-trigger` does that
# filtering before any JSON reaches it. So the two things worth a real bd are: that the
# query's own scoping and the glue around it (SPIRA_INCIDENT_REF construction, the
# world.halted early exit) work end to end. Every UNCLAIMED/NOT_CLEARED decision itself is
# the classifier's own unit tests' job.
#
# TEST AGAINST THE REAL DEPENDENCY (law-prefer-the-real-dependency). Beads are written
# through bd import and queried through bd list --json, exactly as the production path
# does. A hand-written stub of bd's JSON output would reproduce only the fields we
# remembered; the seam between writer and reader is what the test exists to cover.
#
# tier: T2
# covers: watchtower/src/* spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
. "$HERE/testlib.sh"

echo "test-watchtower-czar-outcome.sh"

testdb_require test-watchtower-czar-outcome
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonspiaqct \
    || { echo "test-watchtower-czar-outcome: could not build fixture database"; exit 1; }

NOW="$(date +%s)"
minsago() { date -u -d "@$(( NOW - ($1 * 60) ))" +%Y-%m-%dT%H:%M:%SZ; }
AGO15="$(minsago 15)"

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
        "$@" watchtower --czar-outcome-check 2>/dev/null
}

# ======================================================================================
echo
echo "bd round-trip — the real query scopes to czar-trigger, and the ref is stable:"
# ======================================================================================
# sp-czoc1 (czar-trigger, past the unclaimed threshold) must fire. sp-czoc-other carries
# the same ref prefix but no czar-trigger label — "only czar-trigger-labelled beads are
# in scope" (UC-25) is entirely the query's `--label` argument, invisible to the T1
# classifier suite, which never sees a label at all.
rm -rf "$TMP/run"; mkdir -p "$TMP/run"
testdb_seed <<JSONL
{"id":"sp-czoc1","title":"CZAR: queue-deadlock-batch-open","status":"open","issue_type":"task","labels":["czar-trigger","spira"],"external_ref":"incident:queue-deadlock-batch-open","created_at":"$AGO15"}
{"id":"sp-czoc-other","title":"some incident","status":"open","issue_type":"task","labels":["incident","spira"],"external_ref":"incident:queue-deadlock-batch-open-other","created_at":"$AGO15"}
JSONL

wt_co SPIRA_CZAR_UNCLAIMED_MINS=10
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
want "round-trip: UNCLAIMED fires on the czar-trigger bead" "sp-czoc1" "$subjects"
n_fires="$(printf '%s\n' "$subjects" | grep -c 'CZAR:' || true)"
is "round-trip: only the czar-trigger-labelled bead fired (not the other)" "1" "$n_fires"

# Run again: the ref must be identical, so incident.sh dedups a recurrence rather than
# filing a second bead.
wt_co SPIRA_CZAR_UNCLAIMED_MINS=10
refs="$(cat "$TMP/inc-refs" 2>/dev/null || echo "")"
unique="$(printf '%s\n' "$refs" | sort -u | grep -c . 2>/dev/null || echo 0)"
is "round-trip: two runs produce the same incident ref" "1" "$unique"
want "round-trip: ref names the bead id" "sp-czoc1" "$refs"

# ======================================================================================
echo
echo "halted world — the same fixture is silent when world.halted exists:"
# ======================================================================================
rm -f "$TMP/inc-subjects" "$TMP/inc-refs"
printf 'halted\n' > "$TMP/run/world.halted"
wt_co SPIRA_CZAR_UNCLAIMED_MINS=10
subjects="$(cat "$TMP/inc-subjects" 2>/dev/null || echo "")"
nowant "halted world: no escalations" "CZAR:" "$subjects"

tl_summary
