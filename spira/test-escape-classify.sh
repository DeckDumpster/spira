#!/usr/bin/env bash
#
# test-escape-classify.sh — escape-classify.sh classifies a round red attributed to a
# member as MAPPING GAP, GATE GAP or ENVIRONMENT GAP (sp-6vd2s), and a MAPPING GAP files
# (or bumps the recurrence on) exactly one test-plan-correction bead per suite.
#
# WHAT THIS PROVES
#   1. MAPPING GAP: the red suite's own # covers: glob matches none of the paths the
#      member changed -> mapping_gap, regardless of any gate evidence (gate-log is passed
#      as /nonexistent for this case).
#   2. POSITIVE CONTROL (law-absence-needs-a-positive-control): the same member, a suite
#      whose covers: glob DOES match what it changed, is never classed mapping_gap — a
#      matcher that always said mapping_gap would pass case 1 for the wrong reason.
#   3. GATE GAP: covers matches, but gate.log carries no verdict for the branch at all —
#      the gate never even ran it.
#   4. GATE GAP: covers matches, gate.log's row for the branch is rc=$SPIRA_GATE_NOVERDICT
#      (a timeout) — sp-xethq's own fixture from the bead (four rc=75 runs, no verdict).
#   5. ENVIRONMENT GAP: covers matches, gate.log's row for the branch is rc=0 (a real
#      pass) — the gate cleared it and the round still went red.
#   6. The LAST row for the branch governs: a later timeout after an earlier pass reverts
#      the classification to gate_gap, since the pass no longer describes the tree that
#      was actually gated last.
#   7. MAPPING GAP FILING DEDUPES: calling escape_record for the same suite twice files
#      exactly one open test-plan-gap bead, with the second call recorded as a recurrence,
#      never a second bead — the mechanism this bead calls "reruns do not double-file",
#      reusing incident.sh's own dedupe rather than a second implementation of it.
#
# Driven through the REAL escape-classify.sh and incident.sh against a REAL bd on a
# throwaway fixture database (law-prefer-the-real-dependency) — no stubs.
#
# tier: T2
# requires: testenv
# covers: spira/escape-classify.sh spira/incident.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-escape-classify.sh"

# shellcheck disable=SC1090
. "$HERE/conf.sh"

TMP="$(mktemp -d)"; trap 'testdb_drop 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM

# ============================================================================
# FIXTURE — a base commit with two suites, and a member branch that touches
# exactly one path.
# ============================================================================
REPO="$TMP/repo"
git init -q --initial-branch=master "$REPO"
git -C "$REPO" config user.email "test@spira.local"
git -C "$REPO" config user.name "Spira Test"
mkdir -p "$REPO/spira"
printf 'base\n' > "$REPO/spira/placeholder.sh"

cat > "$REPO/spira/test-esc-touch.sh" << 'EOF'
#!/usr/bin/env bash
# covers: spira/touched.sh
set -uo pipefail
exit 1
EOF

cat > "$REPO/spira/test-esc-miss.sh" << 'EOF'
#!/usr/bin/env bash
# covers: spira/nomatch.sh
set -uo pipefail
exit 1
EOF

git -C "$REPO" add -A
git -C "$REPO" commit -q -m base
BASE_SHA="$(git -C "$REPO" rev-parse HEAD)"

git -C "$REPO" checkout -q -b spira/mem-a
printf 'changed\n' > "$REPO/spira/touched.sh"
git -C "$REPO" add -A
git -C "$REPO" commit -q -m "mem-a: touch spira/touched.sh"
MEM_A_TIP="$(git -C "$REPO" rev-parse HEAD)"
git -C "$REPO" checkout -q master

# shellcheck disable=SC1090
. "$HERE/escape-classify.sh"

# ============================================================================
# 1-2. MAPPING GAP, with its positive control
# ============================================================================
CLASS_MISS="$(escape_classify "$REPO" "$BASE_SHA" "$MEM_A_TIP" "$REPO/spira/test-esc-miss.sh" /nonexistent "spira/mem-a")"
is "uncovered suite -> mapping_gap" "mapping_gap" "$CLASS_MISS"

CLASS_TOUCH_NOGATE="$(escape_classify "$REPO" "$BASE_SHA" "$MEM_A_TIP" "$REPO/spira/test-esc-touch.sh" /nonexistent "spira/mem-a")"
if [ "$CLASS_TOUCH_NOGATE" != mapping_gap ]; then
    ok "positive control: covered suite is never classed mapping_gap"
else
    bad "MUST-FAIL CHECK: a suite covering the changed path was still called mapping_gap"
fi

# ============================================================================
# 3-6. GATE GAP / ENVIRONMENT GAP, driven by gate.log
# ============================================================================
is "no gate.log entry for the branch -> gate_gap" "gate_gap" "$CLASS_TOUCH_NOGATE"

GATE_LOG="$TMP/gate.log"
printf '2026-09-28T00:00:00Z spira spira/mem-a waited=0s ran=2700s rc=75 timeout\n' > "$GATE_LOG"
CLASS_TIMEOUT="$(escape_classify "$REPO" "$BASE_SHA" "$MEM_A_TIP" "$REPO/spira/test-esc-touch.sh" "$GATE_LOG" "spira/mem-a")"
is "gate.log rc=\$SPIRA_GATE_NOVERDICT (timeout) -> gate_gap" "gate_gap" "$CLASS_TIMEOUT"

printf '2026-09-28T01:00:00Z spira spira/mem-a waited=0s ran=90s rc=0\n' >> "$GATE_LOG"
CLASS_PASS="$(escape_classify "$REPO" "$BASE_SHA" "$MEM_A_TIP" "$REPO/spira/test-esc-touch.sh" "$GATE_LOG" "spira/mem-a")"
is "gate.log rc=0 (a real pass) -> environment_gap" "environment_gap" "$CLASS_PASS"

printf '2026-09-28T02:00:00Z spira spira/mem-a waited=0s ran=2700s rc=75 harness-fault\n' >> "$GATE_LOG"
CLASS_LATEST="$(escape_classify "$REPO" "$BASE_SHA" "$MEM_A_TIP" "$REPO/spira/test-esc-touch.sh" "$GATE_LOG" "spira/mem-a")"
is "the most recent gate.log row governs, not the first" "gate_gap" "$CLASS_LATEST"

# ============================================================================
# 7. MAPPING GAP FILING DEDUPES — real bd, real incident.sh, throwaway database
# ============================================================================
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-escape-classify
testdb_up escape || { echo "test-escape-classify: could not build a fixture database"; exit 1; }

REPO_MAP="$TMP/repo-map"
printf 'spira|%s\n' "$SPIRA_DB" > "$REPO_MAP"

RUN="$TMP/run"; mkdir -p "$RUN"
# count_open <ref> -> open/in_progress beads carrying ref:<hash-of-ref> — the exact label
# incident.sh's own dedupe (_dedup_incident, sub-path A) keys on, so this counts precisely
# what the dedupe considers "already filed", independent of which labels this suite chose.
count_open() {
    local ref_label; ref_label="ref:$(printf '%s' "$1" | sha256sum | cut -c1-8)"
    bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --label "$ref_label" --json 2>/dev/null \
      | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    print(0); sys.exit(0)
items = data if isinstance(data, list) else data.get("issues", data.get("items", []))
print(len(items))
'
}

file_gap() {
    env -i HOME="$HOME" PATH="$PATH" SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$TMP/nonexistent.conf" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_SPOOL="$RUN/spool" \
        SPIRA_INCIDENT_LOG="$RUN/incident.log" \
        SPIRA_INCIDENT_LOCK="$RUN/incident.lock" \
        SPIRA_RUN="$RUN" \
        SPIRA_HOME="$HERE" \
        SPIRA_MAIL="$TMP/mail" \
        SPIRA_REPO_MAP="$REPO_MAP" \
        SPIRA_INCIDENT_REPO="spira" \
        escape-classify.sh record --member mem-a --suite test-esc-miss.sh \
            --class mapping_gap --paths spira/touched.sh --evidence "test fixture" \
            >/dev/null 2>&1
}

file_gap
REF="test-plan-gap:test-esc-miss.sh"
N1="$(count_open "$REF")"
is "first mapping-gap escape files exactly one open bead" "1" "$N1"

file_gap
N2="$(count_open "$REF")"
is "second mapping-gap escape for the same suite does not double-file" "1" "$N2"

tl_summary
