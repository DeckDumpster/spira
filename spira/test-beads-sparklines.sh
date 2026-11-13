#!/usr/bin/env bash
#
# test-beads-sparklines.sh — BEADS section sparklines derived from timestamps.
#
# Verifies that the two sparklines (opened, closed) are computed from bead
# timestamps alone, with no cockpit-history.csv involved. The landed count is the
# Lifecycle section's, written by the goodput probe, so this renderer emits none.
#
# Positive controls:
#   - known timestamps produce the expected bucket distribution
#   - zero buckets render ▁, not a dropped point (law-absence-needs-a-positive-control)
#   - the renderer emits no landed series at all
#
# The closed/landed counts come from VictoriaLogs: a loopback fake answers them, and an
# unreachable store must show ?, never 0.
#
# No real database. Under a second.
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

# ---------------------------------------------------------------------------
# Run the real script with fixture data.
# ---------------------------------------------------------------------------
PORTFILE="$TMP/vl.port"
python3 -I -c '
import http.server, os, sys, urllib.parse

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        q = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query).get("query", [""])[0]
        n = 3 if "in(" in q else 1
        body = ("{\"n\":\"%d\"}\n" % n).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

srv = http.server.HTTPServer(("127.0.0.1", 0), H)
open(sys.argv[1] + ".tmp", "w").write(str(srv.server_address[1]))
os.rename(sys.argv[1] + ".tmp", sys.argv[1])
srv.serve_forever()
' "$PORTFILE" &
VL_PID=$!
trap 'kill "$VL_PID" 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM
for _ in $(seq 100); do [ -s "$PORTFILE" ] && break; sleep 0.1; done
VL_URL="http://127.0.0.1:$(cat "$PORTFILE" 2>/dev/null)"

run_block() {   # run_block [victorialogs url]
    local url="${1:-$VL_URL}"
    printf '%s\n' "$BEADS_JSON" \
        | SPIRA_TOML="$(tl_layer SPIRA_VICTORIALOGS_URL="$url")" "$COLLECT" sparklines 2>/dev/null
}

OUT="$(run_block)"
field() { sed -n "s/^$1=//p" <<< "$OUT"; }

# ---------------------------------------------------------------------------
# Counts: closed and landed are the store's counts; opened comes from the bead JSON.
# ---------------------------------------------------------------------------
is "SP_CLOSED_24H is the terminal-transition count" "3" "$(field SP_CLOSED_24H)"
is "SP_OPENED_24H=4" "4" "$(field SP_OPENED_24H)"
has "SP_CLOSED_KINDS has task 2" "$(field SP_CLOSED_KINDS)" "task 2"

# ---------------------------------------------------------------------------
# Sparkline keys are present and are 8 characters wide (BUCKETS=8)
# ---------------------------------------------------------------------------
SPARK_O="$(field SP_BEADS_SPARK_OPENED)"
SPARK_C="$(field SP_BEADS_SPARK_CLOSED)"

is "SP_BEADS_SPARK_OPENED present"   "8" "$(printf '%s' "$SPARK_O" | wc -m | tr -d ' ')"
is "SP_BEADS_SPARK_CLOSED present"   "8" "$(printf '%s' "$SPARK_C" | wc -m | tr -d ' ')"
is "no landed series is emitted" "0" "$(grep -c 'LANDED' <<< "$OUT" || true)"

# ---------------------------------------------------------------------------
# Positive control: zero buckets render ▁, not a dropped point.
# The opened series has events in buckets 0, 2, 5, 5. Buckets 1,3,4,6,7 are
# zero. Confirm that the sparkline is not empty (zeros are real measurements).
# ---------------------------------------------------------------------------
[ -n "$SPARK_O" ] && ok "opened sparkline non-empty with zero buckets present" \
                  || bad "opened sparkline non-empty with zero buckets present" "was empty"

# ---------------------------------------------------------------------------
# Unreadable store: the counts are ?, never 0, and the error is named.
# ---------------------------------------------------------------------------
OUT_DOWN="$(run_block "http://127.0.0.1:1")"
for k in SP_CLOSED_24H SP_CLAIMS_24H; do
    is "unreachable store → $k=?" "?" "$(sed -n "s/^$k=//p" <<< "$OUT_DOWN")"
done
has "unreachable store → the error is named" "$OUT_DOWN" "SP_COUNTS_ERR="
has "unreachable store → opened sparkline still renders" "$OUT_DOWN" "SP_BEADS_SPARK_OPENED="

# ---------------------------------------------------------------------------
# cockpit-history.csv is never touched.
# The renderer reads from stdin (bdjson) only. No HIST reference.
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
