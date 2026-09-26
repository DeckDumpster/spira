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
# covers: spira/cockpit.sh spira/cockpit-sparklines.py cockpit/health.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
has() { printf '%s' "$2" | grep -qF "$3" && ok "$1" || bad "$1" "[$3] not found in [$2]"; }

echo "test-beads-sparklines.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# cockpit-sparklines.py is its own file (not a cockpit.sh heredoc extracted by
# indentation), so this suite runs the real script cockpit.sh runs, unmodified.
# ---------------------------------------------------------------------------
SPARKLINES_PY="$HERE/cockpit-sparklines.py"
if [ ! -f "$SPARKLINES_PY" ]; then
    bad "extract" "cockpit-sparklines.py not found next to cockpit.sh"
    echo ""
    echo "RESULTS: $pass passed, $fail failed"
    exit 1
fi
ok "extract: cockpit-sparklines.py present"

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
BEADS_JSON="$(python3 -c "
import json, time, sys

W = $W
def ts(offset):
    return time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(W + offset))

beads = [
    {'id': 'sp-ta', 'status': 'closed', 'issue_type': 'task', 'labels': ['spira', 'plan'],
     'created_at': ts(3600), 'closed_at': ts(14400), 'updated_at': ts(14400)},
    {'id': 'sp-tb', 'status': 'closed', 'issue_type': 'task', 'labels': ['spira', 'plan'],
     'created_at': ts(25200), 'closed_at': ts(48600), 'updated_at': ts(48600)},
    {'id': 'sp-tc', 'status': 'closed', 'issue_type': 'bug', 'labels': ['spira', 'plan'],
     'created_at': ts(56700), 'closed_at': ts(68400), 'updated_at': ts(68400)},
    {'id': 'sp-td', 'status': 'open', 'issue_type': 'task', 'labels': ['spira', 'plan'],
     'created_at': ts(57000), 'closed_at': None, 'updated_at': ts(57000)},
]
print(json.dumps(beads))
" 2>/dev/null)"

if [ -z "$BEADS_JSON" ]; then
    bad "fixture" "failed to generate beads JSON"
    echo ""
    echo "RESULTS: $pass passed, $fail failed"
    exit 1
fi
ok "fixture: generated bead JSON"

# Landing log: 3 landings at bucket 0, bucket 3, bucket 7
LANDING_LOG="$TMP/landing.log"
python3 -c "
import time, sys
W = $W
def ts(offset):
    return time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(W + offset))
# bucket 0 (+5000s), bucket 3 (+38000s), bucket 7 (+80000s)
print('%s spira: landed spira/sp-ta' % ts(5000))
print('%s spira: landed spira/sp-tb' % ts(38000))
print('%s spira: landed spira/sp-tc' % ts(80000))
# a line NOT matching 'spira: landed' — must be ignored
print('%s some other event' % ts(10000))
# a line outside the window — must be ignored
print('%s spira: landed spira/sp-old' % time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(W - 3600)))
" > "$LANDING_LOG" 2>/dev/null
ok "fixture: generated landing.log"

# ---------------------------------------------------------------------------
# Run the real script with fixture data.
# ---------------------------------------------------------------------------
run_block() {
    local landing="${1:-$LANDING_LOG}"
    printf '%s\n' "$BEADS_JSON" | python3 "$SPARKLINES_PY" "$landing" 2>/dev/null
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
# The script reads from stdin (bdjson) and landing.log only. No HIST reference.
# ---------------------------------------------------------------------------
HIST_REF="$(grep -c 'cockpit-history\|HIST_COLS\|append_history' "$SPARKLINES_PY" 2>/dev/null || true)"
is "script has no HIST_COLS/append_history reference" "0" "${HIST_REF:-0}"

# ---------------------------------------------------------------------------
# Bucket-distribution assertion: the fixture's opened timestamps land in known
# buckets (0, 2, 5, 5 — see the offsets above), so the sparkline is not just
# non-empty but shaped like that distribution. spark_str scales linearly
# between the series' own min (0) and max (2 events, in bucket 5): bucket 5
# must render the tallest glyph and an empty bucket (e.g. bucket 1) the
# shortest — proving the bucket index math, not just that something rendered.
# ---------------------------------------------------------------------------
glyph_at() { python3 -c "import sys; print(sys.argv[1][int(sys.argv[2])])" "$1" "$2"; }
tallest="$(glyph_at "$SPARK_O" 5)"
shortest="$(glyph_at "$SPARK_O" 1)"
is "opened bucket 5 (2 events, the max) renders the tallest glyph" "█" "$tallest"
is "opened bucket 1 (0 events, the min) renders the shortest glyph" "▁" "$shortest"

# ---------------------------------------------------------------------------
echo ""
echo "RESULTS: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
