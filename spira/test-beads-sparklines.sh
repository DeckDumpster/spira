#!/usr/bin/env bash
#
# test-beads-sparklines.sh — BEADS section sparklines derived from timestamps.
#
# Verifies that the three sparklines (opened, closed, landed) are computed from
# bead timestamps and landing.log alone, with no cockpit-history.csv involved.
#
# Positive controls:
#   - known timestamps produce the expected bucket distribution
#   - zero buckets render ▁, not a dropped point (law-absence-needs-a-positive-control)
#   - an unreadable landing.log source renders ?
#
# No network. No real database. Under a second.
#
# defect: sp-mn8q
# tier: T1
# covers: cockpit-collect/src/probes/core_detail.rs cockpit-collect/src/sparklines.rs cockpit/ops/src/health.rs UC-cockpit-observability-19
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
has() { printf '%s' "$2" | grep -qF "$3" && ok "$1" || bad "$1" "[$3] not found in [$2]"; }

echo "test-beads-sparklines.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

COLLECT="$(command -v cockpit-collect)"
if [ -z "$COLLECT" ]; then
    bad "cockpit-collect" "not on PATH"
    echo ""
    echo "RESULTS: $_TL_PASS passed, $_TL_FAIL failed"
    exit 1
fi
ok "cockpit-collect present"

# ---------------------------------------------------------------------------
# Fixture timestamps: pin to a fixed NOW so bucket boundaries are predictable.
# 24h = 86400s, BUCKETS=8, each bucket = 10800s (3h).
#
# bucket 0: 0–10799s after window start  (hours 24–21 ago)
# bucket 1: 10800–21599s                 (hours 21–18 ago)
# ...
# bucket 7: 75600–86399s                 (hours 3–0 ago)
# ---------------------------------------------------------------------------
NOW_TS="$(date +%s)"
W="$(( NOW_TS - 86400 ))"   # window start epoch

ts_at() {
    # seconds since window start → ISO-8601 UTC
    date -u -d "@$(( W + $1 ))" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null \
    || date -u -r "$(( W + $1 ))" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null
}

# Beads: 4 opened, 3 closed within window.
#   opened at bucket 0 (+3600s), bucket 2 (+25200s), bucket 5 (+56700s) ×2
#   closed at bucket 1 (+14400s), bucket 4 (+48600s), bucket 6 (+68400s)
bead() {   # bead <id> <status> <type> <created-offset> <closed-offset|->
    local closed=null upd; upd="\"$(ts_at "${5/#-/$4}")\""
    [ "$5" != - ] && closed="\"$(ts_at "$5")\""
    printf '{"id":"%s","status":"%s","issue_type":"%s","labels":["spira","plan"],"created_at":"%s","closed_at":%s,"updated_at":%s}' \
        "$1" "$2" "$3" "$(ts_at "$4")" "$closed" "$upd"
}
BEADS_JSON="[$(bead sp-ta closed task 3600 14400),$(bead sp-tb closed task 25200 48600),$(bead sp-tc closed bug 56700 68400),$(bead sp-td open task 57000 -)]"

if [ -z "$BEADS_JSON" ]; then
    bad "fixture" "failed to generate beads JSON"
    echo ""
    echo "RESULTS: $_TL_PASS passed, $_TL_FAIL failed"
    exit 1
fi
ok "fixture: generated bead JSON"

# Landing log: 3 landings at bucket 0, bucket 3, bucket 7
LANDING_LOG="$TMP/landing.log"
{
    echo "$(ts_at 5000) spira: landed spira/sp-ta"
    echo "$(ts_at 38000) spira: landed spira/sp-tb"
    echo "$(ts_at 80000) spira: landed spira/sp-tc"
    echo "$(ts_at 10000) some other event"
    echo "$(ts_at -3600) spira: landed spira/sp-old"
} > "$LANDING_LOG"
ok "fixture: generated landing.log"

# ---------------------------------------------------------------------------
# Run the real script with fixture data.
# ---------------------------------------------------------------------------
run_block() {
    local landing="${1:-$LANDING_LOG}"
    printf '%s\n' "$BEADS_JSON" | "$COLLECT" sparklines "$landing" 2>/dev/null
}

OUT="$(run_block)"
field() { sed -n "s/^$1=//p" <<< "$OUT"; }

# ---------------------------------------------------------------------------
# Summary counts (unchanged from before this bead)
# ---------------------------------------------------------------------------
is "SP_CLOSED_24H=3" "3" "$(field SP_CLOSED_24H)"
is "SP_OPENED_24H=4" "4" "$(field SP_OPENED_24H)"
# kinds: task 2, bug 1
has "SP_CLOSED_KINDS has task 2" "$(field SP_CLOSED_KINDS)" "task 2"

# ---------------------------------------------------------------------------
# Sparkline keys are present and are 8 characters wide (BUCKETS=8)
# ---------------------------------------------------------------------------
SPARK_O="$(field SP_BEADS_SPARK_OPENED)"
SPARK_C="$(field SP_BEADS_SPARK_CLOSED)"
SPARK_L="$(field SP_BEADS_SPARK_LANDED)"
LANDED_N="$(field SP_BEADS_LANDED_24H)"

is "SP_BEADS_SPARK_OPENED present"   "8" "$(printf '%s' "$SPARK_O" | wc -m | tr -d ' ')"
is "SP_BEADS_SPARK_CLOSED present"   "8" "$(printf '%s' "$SPARK_C" | wc -m | tr -d ' ')"
is "SP_BEADS_SPARK_LANDED present"   "8" "$(printf '%s' "$SPARK_L" | wc -m | tr -d ' ')"
is "SP_BEADS_LANDED_24H=3"           "3" "$LANDED_N"

# ---------------------------------------------------------------------------
# Positive control: zero buckets render ▁, not a dropped point.
# The opened series has events in buckets 0, 2, 5, 5. Buckets 1,3,4,6,7 are
# zero. Confirm that the sparkline is not empty (zeros are real measurements).
# ---------------------------------------------------------------------------
[ -n "$SPARK_O" ] && ok "opened sparkline non-empty with zero buckets present" \
                  || bad "opened sparkline non-empty with zero buckets present" "was empty"

# ---------------------------------------------------------------------------
# Unreadable landing.log renders ?, never 0.
# ---------------------------------------------------------------------------
OUT_NOLAND="$(run_block "$TMP/nonexistent.log")"
is "missing landing.log → SP_BEADS_SPARK_LANDED=?" "?" \
   "$(sed -n 's/^SP_BEADS_SPARK_LANDED=//p' <<< "$OUT_NOLAND")"
is "missing landing.log → SP_BEADS_LANDED_24H=?" "?" \
   "$(sed -n 's/^SP_BEADS_LANDED_24H=//p' <<< "$OUT_NOLAND")"
# But the opened/closed sparklines still render (they come from the JSON, not landing.log)
has "missing landing.log → opened sparkline still renders" "$OUT_NOLAND" "SP_BEADS_SPARK_OPENED="

# ---------------------------------------------------------------------------
# cockpit-history.csv is never touched.
# The renderer reads from stdin (bdjson) and landing.log only. No HIST reference.
# ---------------------------------------------------------------------------
HIST_REF="$(grep -c 'cockpit-history\|HIST_COLS\|append_history' "$HERE/../cockpit-collect/src/sparklines.rs" 2>/dev/null || true)"
is "renderer has no HIST_COLS/append_history reference" "0" "${HIST_REF:-0}"

# ---------------------------------------------------------------------------
# Bucket-distribution assertion: the fixture's opened timestamps land in known
# buckets (0, 2, 5, 5 — see the offsets above), so the sparkline is not just
# non-empty but shaped like that distribution. spark_str scales linearly
# between the series' own min (0) and max (2 events, in bucket 5): bucket 5
# must render the tallest glyph and an empty bucket (e.g. bucket 1) the
# shortest — proving the bucket index math, not just that something rendered.
# ---------------------------------------------------------------------------
glyph_at() { printf '%s\n' "$1" | LC_ALL=C.UTF-8 grep -o . | sed -n "$(( $2 + 1 ))p"; }
tallest="$(glyph_at "$SPARK_O" 5)"
shortest="$(glyph_at "$SPARK_O" 1)"
is "opened bucket 5 (2 events, the max) renders the tallest glyph" "█" "$tallest"
is "opened bucket 1 (0 events, the min) renders the shortest glyph" "▁" "$shortest"

# ---------------------------------------------------------------------------
echo ""
tl_summary
