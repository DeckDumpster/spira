#!/usr/bin/env bash
#
# test-census-watermark.sh — census.sh ranks classes by since-watermark count, not all-time.
#
#   ./test-census-watermark.sh
#
# WHAT THIS GUARDS
# ---------------
# census.sh used to count events across all time, so a class with a historically high count
# always outranked a recently active one — even after its cause was fixed. This suite verifies
# that census.sh, when given a watermark, ranks classes by events since the watermark and
# shows both counts in its output (sp-yk35).
#
# THE SCENARIO
# ------------
# Two classes:
#   sp-recur-old-class: 2 events before watermark (all-time=2, since-watermark=0)
#   sp-recur-new-class: 1 event after  watermark (all-time=1, since-watermark=1)
#
# Before fix: census ignores watermark → sp-recur-old-class ranks first (2 all-time > 1)
# After  fix: census ranks by since-watermark → sp-recur-new-class ranks first (1 > 0)
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control, law-a-regression-test-must-be-seen-to-fail)
# ----------------------------------------------------------------------------------------------------
# A no-watermark run confirms both classes are visible all-time before the watermark test runs.
# This guards against an empty result masking as a passing since-watermark check.
#
# The suite was run against the unfixed tree and produced:
#   FAIL — live class ranks first (since-watermark): expected [sp-recur-new-class] got [sp-recur-old-class]
#   FAIL — stale class shows 0 since watermark: wanted [0 sp-recur-old-class] in [2 sp-recur-old-class]
# (the unfixed census ignored the watermark file entirely and ranked by all-time count only)
#
# covers: spira/census.sh spira/lib.sh
# hermetic-ok: uses a fixture database; no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "${2:-}"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want() { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-census-watermark
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — census.sh's watermark path reads events via bd sql, which embedded mode refuses
export SPIRA_TESTDB_MODE=server
testdb_up census-watermark || {
    printf 'SKIP test-census-watermark: server testdb not available\n' >&2
    exit 77
}

CENSUS="$HERE/census.sh"

echo "test-census-watermark.sh"

# --------------------------------------------------------
# Insert an event with an explicit past timestamp (2001).
# BEFORE_TS=1000000000  (2001-09-09 01:46:40 UTC — before any plausible watermark)
# --------------------------------------------------------
_insert_past_event() {   # _insert_past_event <bead_id> <cause>
    local id="$1" cause="$2"
    local uuid
    uuid="$(python3 -c 'import uuid; print(str(uuid.uuid4()))' 2>/dev/null)" || return 1
    local q="INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', 'recurred', 'test', '$cause', FROM_UNIXTIME(1000000000))"
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql "$q" >/dev/null 2>&1
}

# --------------------------------------------------------
# Watermark file: 1500000000 (2017-07-14 02:40:00 UTC)
# All "past" events (ts=1000000000) are BEFORE this.
# All "present" events (from bump_recur, ts≈NOW()) are AFTER this.
# --------------------------------------------------------
WATERMARK_TS=1500000000
mkdir -p "$TMP/run"
printf '%s' "$WATERMARK_TS" > "$TMP/run/maechen.watermark"

run_census() {           # run_census [--with-suppressed]
    env SPIRA_DB="$SPIRA_DB" \
        SPIRA_MAECHEN_REMEDY_LABEL=maechen-remedy \
        SPIRA_CONF="$TMP/no-conf" \
        SPIRA_HOME="$HERE" \
        SPIRA_RUN="$TMP/run" \
        bash "$CENSUS" "$@" 2>/dev/null
}

run_census_no_wm() {     # run without watermark (all-time baseline)
    env SPIRA_DB="$SPIRA_DB" \
        SPIRA_MAECHEN_REMEDY_LABEL=maechen-remedy \
        SPIRA_CONF="$TMP/no-conf" \
        SPIRA_HOME="$HERE" \
        SPIRA_RUN="$TMP/empty-run" \
        bash "$CENSUS" "$@" 2>/dev/null
}

# ==============================================================================
echo
echo "SETUP: seed two classes"
# ==============================================================================
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-wm1","title":"stale bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-13T00:00:00Z"}
{"id":"sp-wm2","title":"live bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2026-09-13T00:00:00Z"}
JSONL

# Stale class: 2 recurred events BEFORE watermark
_insert_past_event "sp-wm1" "old-class"
_insert_past_event "sp-wm1" "old-class"

# Live class: 1 recurred event NOW (AFTER watermark via bump_recur)
# shellcheck disable=SC1090
. "$HERE/lib.sh"
bump_recur "sp-wm2" new-class

# ==============================================================================
echo
echo "POSITIVE CONTROL: all-time census sees both classes"
# ==============================================================================
# Confirms the query reaches the database and both classes are visible before the
# watermark filter is applied. An empty result here means census cannot see anything,
# so a later 'since-watermark = 0' is evidence of absence, not a census failure.
out_alltime="$(run_census_no_wm)"
want "stale class visible all-time (1 distinct bead)" "1 sp-recur-old-class" "$out_alltime"
want "stale class: 2 event detections shown all-time" "sp-recur-old-class (2 detections" "$out_alltime"
want "live class visible all-time (1 distinct bead)"   "1 sp-recur-new-class" "$out_alltime"

first_alltime="$(printf '%s\n' "$out_alltime" | head -1 | awk '{print $2}')"
is "all-time: stale class (2 events) outranks live class (1 event) by tie-break" "sp-recur-old-class" "$first_alltime"

# ==============================================================================
echo
echo "WATERMARK TEST: since-watermark count ranks live class first"
# ==============================================================================
out="$(run_census)"

# Both classes must appear in watermark output (positive control holds for this mode too)
want "live class appears in output"  "sp-recur-new-class" "$out"
want "stale class appears in output" "sp-recur-old-class" "$out"

# Live class must rank first (1 since watermark > 0 since watermark)
first_class="$(printf '%s\n' "$out" | head -1 | awk '{print $2}')"
is "live class ranks first (since-watermark)" "sp-recur-new-class" "$first_class"

# Stale class shows 0 new since watermark
want "stale class shows 0 new since watermark" "0 sp-recur-old-class" "$out"

# Stale class: 0 detections since watermark, 1 all-time bead (2 events buried in history)
want "stale class: 0 since-watermark, 1 all-time bead" "0 sp-recur-old-class (0 detections, 1 all-time)" "$out"

# Live class shows 1 bead since watermark with detection count and all-time bead count
want "live class shows 1 bead since watermark with detections" "1 sp-recur-new-class (1 detections, 1 all-time)" "$out"

# ==============================================================================
echo
echo "FALLBACK: missing watermark file → stderr notice, all-time output"
# ==============================================================================
# When no watermark file exists, census falls back to all-time counts and says so on stderr.
stderr_out="$(env SPIRA_DB="$SPIRA_DB" \
    SPIRA_MAECHEN_REMEDY_LABEL=maechen-remedy \
    SPIRA_CONF="$TMP/no-conf" \
    SPIRA_HOME="$HERE" \
    SPIRA_RUN="$TMP/empty-run" \
    bash "$CENSUS" 2>&1 >/dev/null)"
want "missing watermark: stderr notice" "watermark" "$stderr_out"

out_fallback="$(run_census_no_wm)"
want "missing watermark: stale class ranked by all-time (1 bead, 2 detections)" "1 sp-recur-old-class" "$out_fallback"
want "missing watermark: stale class shows 2 event detections" "sp-recur-old-class (2 detections" "$out_fallback"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
