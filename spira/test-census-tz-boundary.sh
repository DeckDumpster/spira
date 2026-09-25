#!/usr/bin/env bash
#
# test-census-tz-boundary.sh — _census_events_sql watermark boundary is UTC, not local time
#
# WHAT THIS GUARDS
# ---------------
# _census_events_sql used FROM_UNIXTIME(epoch) in the since-watermark predicate.
# Dolt's FROM_UNIXTIME renders the epoch in the server's local timezone while labelling
# the result "+0000 UTC", so on a PDT box the boundary was placed 7h earlier than the
# named watermark. Every event in the 7h preceding the watermark was re-counted by each
# pass — inflating class sizes and manufacturing patterns.
#
# THE SCENARIO
# -----------
# Watermark = 1750000000 (2025-06-16 09:06:40 UTC).
# Event A at watermark-3600 (08:06:40 UTC): should NOT appear in since-watermark results.
# Event B at watermark+3600 (10:06:40 UTC): should appear.
#
# On the unfixed tree under a negative UTC offset (PDT = -0700), FROM_UNIXTIME places
# the boundary at 02:06:40 UTC — 7h early. Event A at 08:06:40 UTC is AFTER that skewed
# boundary, so it is counted. The test asserts exactly 1 since-watermark bead; the unfixed
# tree reports 2.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control, law-a-regression-test-must-be-seen-to-fail)
# ----------------------------------------------------------------------------------------------------
# Part 1 (SQL text): _census_events_sql <epoch> | grep -c FROM_UNIXTIME
#   Unfixed: prints ≥1. Fixed: prints 0.
# Part 2 (behavioural): after seeding, since-watermark bead count is 1 not 2.
#   Unfixed: reports 2 (both events). Fixed: reports 1 (only event B).
#
# The suite was run against the unfixed tree:
#   FAIL — since-watermark SQL uses no FROM_UNIXTIME: expected [0] got [1]
#   FAIL — only event after watermark counted: expected [1] got [2]
#
# covers: spira/lib.sh
# hermetic-ok: part-1 needs no db; part-2 uses a fixture database
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "${2:-}"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want() { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1" ;; esac; }

# shellcheck disable=SC1090
. "$HERE/lib.sh"

WATERMARK_TS=1750000000
BOUNDARY_UTC="$(date -u -d "@${WATERMARK_TS}" '+%Y-%m-%d %H:%M:%S')"
EVENT_BEFORE_UTC="$(date -u -d "@$((WATERMARK_TS - 3600))" '+%Y-%m-%d %H:%M:%S')"
EVENT_AFTER_UTC="$(date -u -d  "@$((WATERMARK_TS + 3600))" '+%Y-%m-%d %H:%M:%S')"

echo "test-census-tz-boundary.sh"

# ==============================================================================
echo
echo "PART 1 — SQL text: since-watermark uses no FROM_UNIXTIME"
# ==============================================================================
_sql="$(_census_events_sql "$WATERMARK_TS")"
_count="$(printf '%s' "$_sql" | grep -c 'FROM_UNIXTIME' || true)"
is "since-watermark SQL uses no FROM_UNIXTIME" "0" "$_count"
want "since-watermark SQL contains UTC boundary string" "'${BOUNDARY_UTC}'" "$_sql"

# Empty / zero watermark must produce no since-clause
_sql_zero="$(_census_events_sql 0)"
_count_zero="$(printf '%s' "$_sql_zero" | grep -c 'FROM_UNIXTIME' || true)"
is "zero watermark: still no FROM_UNIXTIME" "0" "$_count_zero"
nowant "zero watermark: no since-clause in SQL" "'${BOUNDARY_UTC}'" "$_sql_zero"

_sql_empty="$(_census_events_sql "")"
nowant "empty watermark: no since-clause in SQL" "created_at >" "$_sql_empty"

# ==============================================================================
echo
echo "PART 2 — behavioural: boundary excludes event 1h before watermark"
# ==============================================================================
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-census-tz-boundary
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — asserts on census_events_run_sql output (bd sql), which embedded mode refuses
export SPIRA_TESTDB_MODE=server
testdb_up census-tz-boundary || {
    printf 'SKIP test-census-tz-boundary part-2: server testdb not available\n' >&2
    # Still report part-1 results before exiting.
    printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
    [ "$fail" -eq 0 ]
    exit $?
}

_insert_event_at() {   # _insert_event_at <bead_id> <event_type> <cause> <utc_ts>
    local id="$1" etype="$2" cause="$3" ts="$4"
    local uuid
    uuid="$(python3 -c 'import uuid; print(str(uuid.uuid4()))' 2>/dev/null)" || return 1
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql \
        "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', '$etype', 'test', '$cause', '$ts')" \
        >/dev/null 2>&1
}

testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-tz1","title":"before-wm bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2025-06-16T00:00:00Z"}
{"id":"sp-tz2","title":"after-wm bead","status":"open","issue_type":"task","labels":["spira"],"updated_at":"2025-06-16T00:00:00Z"}
JSONL

_insert_event_at "sp-tz1" "recurred" "tz-before" "$EVENT_BEFORE_UTC"
_insert_event_at "sp-tz2" "recurred" "tz-after"  "$EVENT_AFTER_UTC"

# Positive control: both events visible all-time
_all="$(SPIRA_DB="$SPIRA_DB" census_events_run_sql 2>/dev/null)"
want "positive control: before-watermark event visible all-time" "tz-before" "$_all"
want "positive control: after-watermark event visible all-time"  "tz-after"  "$_all"

# Since-watermark query: only the after event should appear
_since="$(SPIRA_DB="$SPIRA_DB" census_events_run_sql "$WATERMARK_TS" 2>/dev/null)"
_before_count="$(printf '%s\n' "$_since" | grep -c 'tz-before' || true)"
_after_count="$(printf '%s\n' "$_since"  | grep -c 'tz-after'  || true)"

is "only event after watermark counted (before absent)" "0" "$_before_count"
is "event after watermark is counted (after present)"   "1" "$_after_count"

# ==============================================================================
echo
echo "PART 3 — write side: bump_requeue stamps created_at in UTC, not server-local"
# ==============================================================================
# sp-yyih8: _bump_write_event_try / bump_reopen used NOW(), which on a box whose dolt
# server has no TZ set returns server-local wall clock while the column is read as UTC.
# UTC_TIMESTAMP() is unaffected by @@system_time_zone. A container's own system tz may
# happen to be UTC, in which case NOW() and UTC_TIMESTAMP() agree there and the
# behavioural check below cannot see the class — so check the source text first, the
# same way PART 1 checks _census_events_sql's text rather than relying on the read side
# landing on a skewed box.
_now_count="$(grep -c "created_at) VALUES.*NOW())" "$HERE/lib.sh" || true)"
is "no write-side event INSERT uses NOW()" "0" "$_now_count"

bump_requeue "sp-tz1" "tz-write-check"
_lag="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql \
    "SELECT ABS(TIMESTAMPDIFF(SECOND, MAX(created_at), MAX(UTC_TIMESTAMP()))) FROM events WHERE issue_id='sp-tz1' AND event_type='requeued' AND new_value='tz-write-check'" \
    2>/dev/null | sed -n '3p' | tr -d ' ')"
if [ -n "$_lag" ] && [ "$_lag" -le 5 ] 2>/dev/null; then
    ok "bump_requeue created_at within 5s of UTC_TIMESTAMP()"
else
    bad "bump_requeue created_at within 5s of UTC_TIMESTAMP()" "lag_s=${_lag:-<empty>}"
fi

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
