#!/usr/bin/env bash
#
# test-census-pipeline.sh — census.sh's decision logic, table-tested on canned rows.
#
#   ./test-census-pipeline.sh
#
# WHAT THIS TESTS
# ---------------
# The python files under census/ (extracted from census.sh's former heredocs) on
# canned stdin/argv, with no database:
#   cluster.py          — raw event rows -> ranked "<causal> <victims> <detections> <class>"
#                          lines, clustering same-class events less than the gap apart
#                          into one causal event (sp-jcd0e, Concierge decision sp-h2hpl/
#                          sp-yojbh)
#   cluster_merge.py     — all-time + since-watermark cluster.py files -> ranked merged
#                          lines
#   covers.py           — open-remedy bead JSON -> covered classes (with fold-map)
#   covers_closed.py   — closed-remedy bead JSON -> "<bead-id> <class>" within the window
#
# and census.sh's own bash decision logic (watermark fallback/stderr, suppression
# state, orphan-vs-in-flight annotation) end-to-end against a scripted `bd`, never a
# real Dolt store — the property under test is the decision, not the database read,
# which UC-ops-detection-remediation-12 covers with 3 real rows in test-census.sh.
#
# tier: T1
# covers: spira/census/cluster.py spira/census/cluster_merge.py spira/census/covers.py spira/census/covers_closed.py census/src/* spira/chamber/maechen.md UC-ops-detection-remediation-11 UC-ops-detection-remediation-13 UC-ops-detection-remediation-14 UC-ops-detection-remediation-15
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
lack() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1" ;; esac; }

CENSUS_DIR="$HERE/census"
CENSUS="$(command -v census)"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

echo "test-census-pipeline.sh"

# ==============================================================================
echo
echo "cluster.py — cause-suffix mapping per event_type, one event per class (UC-11)"
# ==============================================================================
# Input rows are now raw (event_type, new_value, issue_id, unix-ts) — cluster.py, not the
# SQL, does the aggregating. One row per class here just proves the class-name mapping
# survived the move; clustering itself is exercised further down.
cluster_of() { python3 "$CENSUS_DIR/cluster.py" <<<"$1"; }

want "requeued+cause -> sp-requeue-<cause>" "1 1 1 sp-requeue-prod-dirty" \
    "$(cluster_of 'requeued | prod-dirty | bead1 | 1000')"
want "recurred+cause -> sp-recur-<cause>" "1 1 1 sp-recur-suite-red" \
    "$(cluster_of 'recurred | suite-red | bead1 | 1000')"
want "reclaimed+no cause -> bare sp-reclaim" "1 1 1 sp-reclaim" \
    "$(cluster_of 'reclaimed |  | bead1 | 1000')"
lack "bare sp-reclaim has no trailing dash-suffix" "sp-reclaim-" \
    "$(cluster_of 'reclaimed |  | bead1 | 1000')"
want "reclaimed+cause -> sp-reclaim-<cause>" "1 1 1 sp-reclaim-timeout" \
    "$(cluster_of 'reclaimed | timeout | bead1 | 1000')"
want "lapsed+cause -> sp-lapsed-<cause>" "1 1 1 sp-lapsed-cause-x" \
    "$(cluster_of 'lapsed | cause-x | bead1 | 1000')"
want "reopen (already SQL-folded) + cause -> sp-reopen-<cause>" "1 1 1 sp-reopen-rebase-conflict" \
    "$(cluster_of 'reopen | rebase-conflict | bead1 | 1000')"
want "empty cause -> sp-reopen-unrecorded, not a bare class" "1 1 1 sp-reopen-unrecorded" \
    "$(cluster_of 'reopened |  | bead1 | 1000')"
want "unknown event_type is silently ignored (positive control: known type is not)" \
    "" "$(cluster_of 'mystery | x | bead1 | 1000')"
is "malformed row (wrong column count) is skipped, not fatal" "" \
    "$(cluster_of 'recurred | x | bead1')"

# ==============================================================================
echo
echo "cluster.py — no double-count across a requeue+reopen pair"
# ==============================================================================
# The requeue/reopen fold (sp-requeue-merge-conflict -> sp-reopen-rebase-conflict) happens
# in the SQL (lib.sh:_census_events_sql / _census_event_rows_sql), never in cluster.py:
# cluster.py sees only whatever rows the query emits. This proves cluster.py keeps each
# row's class independent — if the SQL fold ever regressed and re-emitted the raw pair,
# cluster.py would report two classes rather than silently merging or losing one.
pair_out="$(printf 'requeued | merge-conflict | bead1 | 1000\nreopen | rebase-conflict | bead2 | 1000\n' \
    | python3 "$CENSUS_DIR/cluster.py")"
want "raw pair: requeue class present" "1 1 1 sp-requeue-merge-conflict" "$pair_out"
want "raw pair: reopen class present, not merged into the requeue line" \
    "1 1 1 sp-reopen-rebase-conflict" "$pair_out"
is "raw pair: exactly two class lines (no cross-class bleed)" "2" "$(printf '%s\n' "$pair_out" | grep -c .)"

# ==============================================================================
echo
echo "cluster.py — clustering: same-class events inside the gap are one causal event (sp-jcd0e)"
# ==============================================================================
# Pinned to a NON-DEFAULT gap (100s, not the 300s default) so the assertion exercises the
# configured value rather than a literal that would pass even if the env var were ignored.
cluster_gap() { SPIRA_CENSUS_CLUSTER_GAP_S=100 python3 "$CENSUS_DIR/cluster.py" <<<"$1"; }

# Three events of one class: bead1@1000, bead2@1050 (50s later, inside the 100s gap — same
# causal event), bead3@1200 (150s after bead2, at/beyond the gap — a new causal event).
tight_out="$(cluster_gap "$(printf 'recurred | tight | bead1 | 1000\nrecurred | tight | bead2 | 1050\nrecurred | tight | bead3 | 1200\n')")"
want "burst across the gap: two causal events, three victims, three detections" \
    "2 3 3 sp-recur-tight" "$tight_out"

# A gap of exactly the threshold is NOT "less than" it, so it still splits.
exact_out="$(cluster_gap "$(printf 'recurred | exact | bead1 | 1000\nrecurred | exact | bead2 | 1100\n')")"
want "gap exactly at the threshold splits into two causal events" \
    "2 2 2 sp-recur-exact" "$exact_out"

# A gap one second under the threshold merges.
under_out="$(cluster_gap "$(printf 'recurred | under | bead1 | 1000\nrecurred | under | bead2 | 1099\n')")"
want "gap one second under the threshold merges into one causal event" \
    "1 2 2 sp-recur-under" "$under_out"

# Input order does not matter: cluster.py sorts by timestamp before clustering.
unordered_out="$(cluster_gap "$(printf 'recurred | tight | bead3 | 1200\nrecurred | tight | bead1 | 1000\nrecurred | tight | bead2 | 1050\n')")"
want "unordered input clusters identically to sorted input" \
    "2 3 3 sp-recur-tight" "$unordered_out"

# ==============================================================================
echo
echo "cluster.py — the ask's own positive control: three timelines, ranked by causal event (sp-jcd0e/sp-yojbh)"
# ==============================================================================
# The exact fixture from the Concierge decision (sp-h2hpl, ratified sp-yojbh): three
# classes tied at 5 victims each, distinguishable only once the causal-event count is
# read. Default gap (SPIRA_CENSUS_CLUSTER_GAP_S unset -> 300s = 5 minutes).
#
#   sp-reopen-eject:    5 events, gaps of 5/67/30/40 min (all >= 5 min) -> 5 causal events
#   sp-requeue-sweepy:  6 events, gaps of 0.1/0.1/0.1/210.2/0.1 min     -> 2 causal events
#   sp-reclaim:         5 events, gaps of 3.6/3.3/3.4/3.6 min (all < 5) -> 1 causal event
_min() { awk -v m="$1" 'BEGIN{printf "%d", m*60}'; }
_BASE=2000000000

_eject_t0=$_BASE
_eject_t1=$((_eject_t0 + $(_min 5)))
_eject_t2=$((_eject_t1 + $(_min 67)))
_eject_t3=$((_eject_t2 + $(_min 30)))
_eject_t4=$((_eject_t3 + $(_min 40)))
_eject_rows="$(printf 'reopen | eject | e0 | %d\nreopen | eject | e1 | %d\nreopen | eject | e2 | %d\nreopen | eject | e3 | %d\nreopen | eject | e4 | %d\n' \
    "$_eject_t0" "$_eject_t1" "$_eject_t2" "$_eject_t3" "$_eject_t4")"

_sweep_t0=$_BASE
_sweep_t1=$((_sweep_t0 + $(_min 0.1)))
_sweep_t2=$((_sweep_t1 + $(_min 0.1)))
_sweep_t3=$((_sweep_t2 + $(_min 0.1)))
_sweep_t4=$((_sweep_t3 + $(_min 210.2)))
_sweep_t5=$((_sweep_t4 + $(_min 0.1)))
# s4 reuses s0's bead (a bead the second sweep re-detected) so the class lands on 5
# distinct victims across 6 events, matching the ask's own measurement exactly.
_sweep_rows="$(printf 'requeued | sweepy | s0 | %d\nrequeued | sweepy | s1 | %d\nrequeued | sweepy | s2 | %d\nrequeued | sweepy | s3 | %d\nrequeued | sweepy | s0 | %d\nrequeued | sweepy | s5 | %d\n' \
    "$_sweep_t0" "$_sweep_t1" "$_sweep_t2" "$_sweep_t3" "$_sweep_t4" "$_sweep_t5")"

_ghost_t0=$_BASE
_ghost_t1=$((_ghost_t0 + $(_min 3.6)))
_ghost_t2=$((_ghost_t1 + $(_min 3.3)))
_ghost_t3=$((_ghost_t2 + $(_min 3.4)))
_ghost_t4=$((_ghost_t3 + $(_min 3.6)))
_ghost_rows="$(printf 'reclaimed |  | g0 | %d\nreclaimed |  | g1 | %d\nreclaimed |  | g2 | %d\nreclaimed |  | g3 | %d\nreclaimed |  | g4 | %d\n' \
    "$_ghost_t0" "$_ghost_t1" "$_ghost_t2" "$_ghost_t3" "$_ghost_t4")"

three_timelines_out="$(printf '%s\n%s\n%s\n' "$_eject_rows" "$_sweep_rows" "$_ghost_rows" | python3 "$CENSUS_DIR/cluster.py")"
want "eject: 5 independent events -> ranks 1st with 5 causal events" \
    "5 5 5 sp-reopen-eject" "$three_timelines_out"
want "sweepy: one burst + one late straggler -> 2 causal events, 5 victims" \
    "2 5 6 sp-requeue-sweepy" "$three_timelines_out"
want "ghost: one tight sweep -> 1 causal event, 5 victims" \
    "1 5 5 sp-reclaim" "$three_timelines_out"
ranked_classes="$(printf '%s\n' "$three_timelines_out" | awk '{print $4}')"
is "ranked strictly by causal event count: eject(5) > sweepy(2) > ghost(1), NOT by the tied victim count (5 each)" \
    "$(printf 'sp-reopen-eject\nsp-requeue-sweepy\nsp-reclaim\n')" "$ranked_classes"

# ==============================================================================
echo
echo "cluster.py — ranking: causal events decide rank, victims are secondary (never the rank)"
# ==============================================================================
# classA: 3 well-separated events (3 causal events), 3 victims.
# classB: one 3-event burst (1 causal event), 5 victims — MORE victims than classA.
# If victims still decided the rank, classB (5) would outrank classA (3). The ratified
# rule requires the opposite.
rank_rows="$(printf 'recurred | few-victims-many-events | va | 1000\nrecurred | few-victims-many-events | vb | 2000\nrecurred | few-victims-many-events | vc | 3000\nrecurred | many-victims-one-sweep | wa | 1000\nrecurred | many-victims-one-sweep | wb | 1010\nrecurred | many-victims-one-sweep | wc | 1020\nrecurred | many-victims-one-sweep | wd | 1030\nrecurred | many-victims-one-sweep | we | 1040\n')"
rank_out="$(printf '%s' "$rank_rows" | python3 "$CENSUS_DIR/cluster.py")"
first_cls="$(printf '%s\n' "$rank_out" | head -1 | awk '{print $4}')"
is "3-causal-event class ranks first despite fewer victims than the 1-event burst" \
    "sp-recur-few-victims-many-events" "$first_cls"
want "the burst's larger victim count is visible but did not win the rank" \
    "1 5 5 sp-recur-many-victims-one-sweep" "$rank_out"

# ==============================================================================
echo
echo "cluster.py — header/separator lines and the empty store"
# ==============================================================================
sql_shape="$(printf '+------+\n| event_type | new_value | issue_id | ts |\n+------+\n| recurred | shape-test | bead1 | 1000 |\n+------+\n')"
want "realistic bd-sql framing: header/separator ignored, data row counted" \
    "1 1 1 sp-recur-shape-test" "$(python3 "$CENSUS_DIR/cluster.py" <<<"$sql_shape")"
is "empty store: no input produces no output" "" "$(printf '' | python3 "$CENSUS_DIR/cluster.py")"

# ==============================================================================
echo
echo "cluster_merge.py — since-watermark ranking, with all-time carried alongside (UC-13)"
# ==============================================================================
printf '2 4 5 sp-recur-old-class\n1 1 1 sp-recur-new-class\n' > "$T/all_time.txt"
printf '0 0 0 sp-recur-old-class\n1 1 1 sp-recur-new-class\n' > "$T/since_wm.txt"
merge_out="$(python3 "$CENSUS_DIR/cluster_merge.py" "$T/all_time.txt" "$T/since_wm.txt")"
first_merge="$(printf '%s\n' "$merge_out" | head -1 | awk '{print $2}')"
is "live-since class ranks first despite a smaller all-time count" "sp-recur-new-class" "$first_merge"
want "stale class shows 0 since, all-time preserved" \
    "0 sp-recur-old-class (0 victims, 0 detections, 2 all-time)" "$merge_out"
want "live class shows since count with all-time alongside" \
    "1 sp-recur-new-class (1 victims, 1 detections, 1 all-time)" "$merge_out"

# A class present only since the watermark (no all-time row — cannot happen from one
# query, but cluster_merge.py must not crash reading two independently-produced files).
printf '' > "$T/all_time_empty.txt"
printf '1 1 1 sp-recur-fresh\n' > "$T/since_only.txt"
want "since-only class: all-time reads as 0, not a crash" \
    "1 sp-recur-fresh (1 victims, 1 detections, 0 all-time)" \
    "$(python3 "$CENSUS_DIR/cluster_merge.py" "$T/all_time_empty.txt" "$T/since_only.txt")"

# Malformed line ignored rather than raising.
printf 'garbage line with no count\n1 1 1 sp-recur-ok\n' > "$T/malformed.txt"
printf '' > "$T/empty2.txt"
want "malformed all-time line is skipped, not fatal" "0 sp-recur-ok (0 victims, 0 detections, 1 all-time)" \
    "$(python3 "$CENSUS_DIR/cluster_merge.py" "$T/malformed.txt" "$T/empty2.txt")"

# ==============================================================================
echo
echo "census.sh — watermark fallback and stderr note (UC-13, no Dolt: fake bd)"
# ==============================================================================
FAKE_BD="$T/fake-bd"
cat > "$FAKE_BD" <<'STUB'
#!/usr/bin/env bash
[ "${1:-}" = "-C" ] && shift 2
case "${1:-}" in
    sql)
        case "${2:-}" in
            *utc_fn*)
                cat "${CENSUS_SKEW_FILE:-/dev/null}"
                exit "${CENSUS_SKEW_RC:-0}"
                ;;
            *)
                cat "${CENSUS_SQL_FILE:-/dev/null}"
                exit "${CENSUS_SQL_RC:-0}"
                ;;
        esac
        ;;
    list)
        [ -n "${CENSUS_LIST_ARGS_FILE:-}" ] && echo "$*" >> "$CENSUS_LIST_ARGS_FILE"
        # The remedies are one content query (`--all`, sp-mve9i): every covers: bead, open
        # and closed alike; which is which is the lifecycle stub's answer below.
        python3 - "${CENSUS_OPEN_JSON:-}" "${CENSUS_CLOSED_JSON:-}" <<'PY'
import json, sys
rows = []
for f in sys.argv[1:]:
    try:
        rows += json.load(open(f)) if f else []
    except Exception:
        pass
print(json.dumps(rows))
PY
        exit 0
        ;;
    *) exit 0 ;;
esac
STUB
chmod +x "$FAKE_BD"

CENSUS_SQL_FILE="$T/sql_one_class.txt"
printf 'recurred | fallback-test | fallback-bead | 1000\n' > "$CENSUS_SQL_FILE"

# The skew guard now compares the substrate's UTC_TIMESTAMP() against the HOST's own
# UTC clock (sp-9b8py), not against the substrate's NOW(). census.sh reads that clock as
# ${SPIRA_NOW:-$(date -u +%s)} — the same test-clock override every other suite in this
# tree pins a clock with (lib.sh:6120, watchd, test-archivist.sh, ...) — so the
# fixtures below are relative to CENSUS_FIXED_HOST_EPOCH, a fixed epoch this suite pins
# via SPIRA_NOW, not a fixed calendar date the real clock would eventually catch up to.
CENSUS_FIXED_HOST_EPOCH=1781000000

# _skew_fixture <file> <offset-seconds-from-the-fixed-host-clock> — writes the fake bd's
# answer to "SELECT DATE_FORMAT(UTC_TIMESTAMP(), ...) AS utc_fn" (same 3-row header/
# separator/data shape census.sh's own sed -n '3p' expects) as an exact number of
# seconds away from CENSUS_FIXED_HOST_EPOCH, so the guard's computed skew is exact.
_skew_fixture() {
    local file="$1" offset="$2" val
    val="$(date -u -d "@$(( CENSUS_FIXED_HOST_EPOCH + offset ))" '+%Y-%m-%d %H:%M:%S')"
    printf 'utc_fn\n------\n%s\n' "$val" > "$file"
}

# Default: substrate agrees with the host clock, so tests unrelated to the skew guard
# see it pass through.
CENSUS_SKEW_FILE="$T/skew_in_sync.txt"
_skew_fixture "$CENSUS_SKEW_FILE" 0
RUN_NO_WM="$T/run-no-wm"; mkdir -p "$RUN_NO_WM"

# SPIRA_REPO_MAP points at a scratch path that never exists: census.sh sources lib.sh,
# which runs a containment check against every registered repo the moment SPIRA_REPO_MAP
# resolves to a real file. Pointing it nowhere makes the check a no-op, same as
# SPIRA_CONF's nonexistent path above — a real map is ambient configuration this suite
# must not depend on.
# A REMEDY'S STATE IS ITS LIFECYCLE ROW (sp-mve9i, design §3.4): census splits the covers:
# beads by `spira-lc list`, and asks `spira-lc state <id>` for LANDED (sp-oqf8c). The stub
# models the fixture's two files in lifecycle terms: a bead in CENSUS_OPEN_JSON is READY
# (nobody has handed it on), one in CENSUS_CLOSED_JSON is SUBMITTED (handed on, not landed)
# unless $LCSTATE/<id> names its state (LANDED).
LCSTATE="$T/lcstate"; mkdir -p "$LCSTATE"
cat > "$T/spira-lc-stub" <<STUB
#!/usr/bin/env bash
case "\${1:-}" in
    state) if [ -s "$LCSTATE/\${2:-}" ]; then cat "$LCSTATE/\${2:-}"; else echo SUBMITTED; fi ;;
    list) python3 - "\${CENSUS_OPEN_JSON:-}" "\${CENSUS_CLOSED_JSON:-}" "$LCSTATE" <<'PY'
import json, os, sys
open_f, closed_f, lcdir = sys.argv[1:4]
def ids(f):
    try:
        return [r["id"] for r in json.load(open(f)) if r.get("id")] if f else []
    except Exception:
        return []
def state(i, default):
    p = os.path.join(lcdir, i)
    return open(p).read().strip() if os.path.isfile(p) and os.path.getsize(p) else default
rows = [{"bead_id": i, "state": state(i, "READY")} for i in ids(open_f)]
rows += [{"bead_id": i, "state": state(i, "SUBMITTED")} for i in ids(closed_f)]
print(json.dumps(rows))
PY
    ;;
    facts-query) exit 0 ;;
    facts) echo '[]' ;;
    *) exit 2 ;;
esac
STUB
chmod +x "$T/spira-lc-stub"

run_census_fake() {   # run_census_fake <SPIRA_RUN> [census-args...]
    local rundir="$1"; shift
    tl_config SPIRA_BD="$FAKE_BD" SPIRA_DB="$T/fixture.db" \
        SPIRA_MAECHEN_REMEDY_LABEL=maechen-remedy \
        SPIRA_RUN="$rundir" SPIRA_REPO_MAP="$T/no-repo-map" \
        SPIRA_CENSUS_CLOCK_SKEW_TOLERANCE_S="${SPIRA_CENSUS_CLOCK_SKEW_TOLERANCE_S:-120}"
    env SPIRA_NOW="$CENSUS_FIXED_HOST_EPOCH" \
        SPIRA_LC_BIN="$T/spira-lc-stub" \
        SPIRA_CONF="$T/no-conf" \
        SPIRA_HOME="$HERE" \
        SPIRA_REPO="${CENSUS_REPO:-$T/norepo}" \
        CENSUS_SQL_FILE="$CENSUS_SQL_FILE" \
        CENSUS_SQL_RC="${CENSUS_SQL_RC:-0}" \
        CENSUS_SKEW_FILE="$CENSUS_SKEW_FILE" \
        CENSUS_SKEW_RC="${CENSUS_SKEW_RC:-0}" \
        CENSUS_LIST_ARGS_FILE="${CENSUS_LIST_ARGS_FILE:-}" \
        CENSUS_OPEN_JSON="${CENSUS_OPEN_JSON:-}" \
        CENSUS_CLOSED_JSON="${CENSUS_CLOSED_JSON:-}" \
        "$CENSUS" "$@"
}

nowm_out="$(run_census_fake "$RUN_NO_WM" 2>"$T/nowm.stderr")"
want "no watermark file: falls back to all-time output" "1 sp-recur-fallback-test (1 victims, 1 detections)" "$nowm_out"
want "no watermark file: stderr says so" "no watermark file" "$(cat "$T/nowm.stderr")"

RUN_BAD_WM="$T/run-bad-wm"; mkdir -p "$RUN_BAD_WM"
printf 'not-a-number' > "$RUN_BAD_WM/maechen.watermark"
badwm_out="$(run_census_fake "$RUN_BAD_WM" 2>"$T/badwm.stderr")"
want "unreadable watermark: falls back to all-time output" "1 sp-recur-fallback-test (1 victims, 1 detections)" "$badwm_out"
want "unreadable watermark: stderr says so" "unreadable" "$(cat "$T/badwm.stderr")"

RUN_WM="$T/run-wm"; mkdir -p "$RUN_WM"
printf '1000000000\n' > "$RUN_WM/maechen.watermark"
wm_out="$(run_census_fake "$RUN_WM" 2>"$T/wm.stderr")"
want "valid watermark: since-watermark format used (cluster_merge.py path)" \
    "sp-recur-fallback-test (1 victims, 1 detections, 1 all-time)" "$wm_out"
lack "valid watermark: no fallback stderr note" "falling back" "$(cat "$T/wm.stderr")"

CENSUS_SQL_RC=1
unreachable_out="$(run_census_fake "$RUN_NO_WM" 2>"$T/unreach.stderr")"; unreachable_rc=$?
CENSUS_SQL_RC=0
is "unreachable substrate: exits non-zero, not a silent empty census" "1" "$unreachable_rc"
is "unreachable substrate: no output" "" "$unreachable_out"
want "unreachable substrate: stderr names it" "unreachable" "$(cat "$T/unreach.stderr")"

# ==============================================================================
echo
echo "census.sh — clock skew guard: refuse a windowed ranking over a skewed clock (sp-ohd6h)"
# ==============================================================================
# POSITIVE CONTROL (law-absence-needs-a-positive-control, law-a-regression-test-must-be-
# seen-to-fail): the in-sync fixture used by every test above must itself pass, proving
# the guard can say yes before trusting it to say no.
insync_out="$(run_census_fake "$RUN_NO_WM" 2>"$T/insync.stderr")"; insync_rc=$?
is "clock in sync: exits zero" "0" "$insync_rc"
want "clock in sync: ranking still produced" "1 sp-recur-fallback-test (1 victims, 1 detections)" "$insync_out"
lack "clock in sync: no skew refusal on stderr" "clock skew" "$(cat "$T/insync.stderr")"

# UNFIXED SIGNATURE: skew far outside tolerance, rc=0 from the skew query, ranking would
# have been returned unfixed. FIXED SIGNATURE: refusal, no ranking, skew named on stderr.
CENSUS_SKEW_FILE="$T/skew_skewed.txt"
_skew_fixture "$CENSUS_SKEW_FILE" 25200
skewed_out="$(run_census_fake "$RUN_NO_WM" 2>"$T/skewed.stderr")"; skewed_rc=$?
is "skew beyond tolerance: exits non-zero" "1" "$skewed_rc"
is "skew beyond tolerance: no ranking emitted" "" "$skewed_out"
want "skew beyond tolerance: measured skew named on stderr" "25200" "$(cat "$T/skewed.stderr")"
want "skew beyond tolerance: substrate clock value named on stderr" \
    "$(date -u -d "@$(( CENSUS_FIXED_HOST_EPOCH + 25200 ))" '+%Y-%m-%d %H:%M:%S')" \
    "$(cat "$T/skewed.stderr")"

# A negative skew (substrate behind UTC) refuses on magnitude, not sign.
CENSUS_SKEW_FILE="$T/skew_negative.txt"
_skew_fixture "$CENSUS_SKEW_FILE" -9000
neg_out="$(run_census_fake "$RUN_NO_WM" 2>"$T/neg.stderr")"; neg_rc=$?
is "negative skew beyond tolerance: exits non-zero" "1" "$neg_rc"
is "negative skew beyond tolerance: no ranking emitted" "" "$neg_out"
want "negative skew: measured skew named on stderr" "-9000" "$(cat "$T/neg.stderr")"

# Tolerance is configurable: the same skew that refuses at the default passes when the
# tolerance is widened past it.
SPIRA_CENSUS_CLOCK_SKEW_TOLERANCE_S=30000
tolerant_out="$(run_census_fake "$RUN_NO_WM" 2>"$T/tolerant.stderr")"; tolerant_rc=$?
SPIRA_CENSUS_CLOCK_SKEW_TOLERANCE_S=120
is "widened tolerance: same skew now passes" "0" "$tolerant_rc"
want "widened tolerance: ranking produced" "1 sp-recur-fallback-test (1 victims, 1 detections)" "$tolerant_out"

# ------------------------------------------------------------------------------
# REGRESSION (sp-9b8py): the guard used to measure the substrate's NOW() against its own
# UTC_TIMESTAMP() — a proxy for host timezone, not for the window invariant. On any box
# whose system timezone is not UTC (this suite's fixture forces PDT below) those two
# always disagree by the offset, so the guard refused permanently. The property that
# matters is whether UTC_TIMESTAMP() agrees with the HOST's true UTC clock; NOW() must be
# irrelevant to the refusal.
# ------------------------------------------------------------------------------

# COMPANION CASE (must rank and exit 0): substrate UTC_TIMESTAMP() agrees with the host's
# true UTC clock while the substrate is configured for a non-UTC system timezone (so its
# own NOW() would read hours off, exactly as fb8f68151 tripped on this box). Since the
# fixed query no longer asks for NOW() at all, TZ=America/Los_Angeles proves the guard
# cannot be reading the substrate's local wall clock by any path.
CENSUS_SKEW_FILE="$T/skew_in_sync.txt"
pdt_insync_out="$(TZ=America/Los_Angeles run_census_fake "$RUN_NO_WM" 2>"$T/pdt_insync.stderr")"
pdt_insync_rc=$?
is "PDT box, substrate clock in sync with true UTC: exits zero" "0" "$pdt_insync_rc"
want "PDT box, substrate clock in sync with true UTC: ranking produced" \
    "1 sp-recur-fallback-test (1 victims, 1 detections)" "$pdt_insync_out"
lack "PDT box, substrate clock in sync with true UTC: no refusal on stderr" \
    "clock skew" "$(cat "$T/pdt_insync.stderr")"

# REGRESSION CASE (must be seen to fail before the fix — see the bead's close notes for
# the pre-fix run against the unfixed guard): the substrate's clock is genuinely wrong,
# disagreeing with the host's true UTC by 600s, well past the default 120s tolerance.
# This is exactly the fault this guard exists to catch, and it must still catch it once
# NOW() is out of the picture.
CENSUS_SKEW_FILE="$T/skew_real_fault.txt"
_skew_fixture "$CENSUS_SKEW_FILE" 600
realfault_out="$(run_census_fake "$RUN_NO_WM" 2>"$T/realfault.stderr")"; realfault_rc=$?
is "substrate clock genuinely 600s off true UTC: exits non-zero" "1" "$realfault_rc"
is "substrate clock genuinely 600s off true UTC: no ranking emitted" "" "$realfault_out"
want "substrate clock genuinely 600s off true UTC: measured skew named on stderr" \
    "600" "$(cat "$T/realfault.stderr")"

# The refusal condition itself no longer references NOW() anywhere in the source.
# `census` is a compiled binary now (sp-yyk47); the clock-skew guard's own source lives in
# census/src/real.rs (the crate this checkout's census.sh was retired into), not in a
# script this suite can `cat`.
CENSUS_CLOCK_SRC="$(cd "$HERE/.." && pwd -P)/census/src/real.rs"
if [ -f "$CENSUS_CLOCK_SRC" ]; then
    lack "skew guard source no longer references NOW()" "NOW()" "$(cat "$CENSUS_CLOCK_SRC")"
else
    echo "  SKIP  census/src/real.rs not found at $CENSUS_CLOCK_SRC — cannot check the clock-skew guard's own source"
fi

# A control that cannot check must refuse (law-a-control-that-cannot-check-must-refuse):
# the skew query itself failing is not read as "in sync".
CENSUS_SKEW_FILE="$T/skew_in_sync.txt"
CENSUS_SKEW_RC=1
unverifiable_out="$(run_census_fake "$RUN_NO_WM" 2>"$T/unverifiable.stderr")"; unverifiable_rc=$?
CENSUS_SKEW_RC=0
is "skew check unreachable: exits non-zero" "1" "$unverifiable_rc"
is "skew check unreachable: no ranking emitted" "" "$unverifiable_out"
want "skew check unreachable: stderr says so" "cannot verify" "$(cat "$T/unverifiable.stderr")"

# ==============================================================================
echo
echo "covers.py / covers_closed.py — open and closed remedy suppression (UC-14)"
# ==============================================================================
covers_of() {   # covers_of <fold-map-file> <json>
    printf '%s' "$2" | python3 "$CENSUS_DIR/covers.py" "$1"
}
printf '' > "$T/nofold.txt"
printf 'sp-requeue-merge-conflict sp-reopen-rebase-conflict\n' > "$T/fold.txt"

want "covers.py: open bead's covers: label extracted" "sp-recur-open-class" \
    "$(covers_of "$T/nofold.txt" '[{"labels":["spira","maechen-remedy","covers:sp-recur-open-class"]}]')"
want "covers.py: fold-map resolves a pre-fold alias to the canonical class" \
    "sp-reopen-rebase-conflict" \
    "$(covers_of "$T/fold.txt" '[{"labels":["covers:sp-requeue-merge-conflict"]}]')"
lack "covers.py: non-covers labels produce no class" "spira" \
    "$(covers_of "$T/nofold.txt" '[{"labels":["spira","maechen-remedy"]}]')"
want "covers.py: multiple beads each contribute their class" "sp-recur-a" \
    "$(covers_of "$T/nofold.txt" '[{"labels":["covers:sp-recur-a"]},{"labels":["covers:sp-recur-b"]}]')"
want "covers.py: multiple beads, second class also present" "sp-recur-b" \
    "$(covers_of "$T/nofold.txt" '[{"labels":["covers:sp-recur-a"]},{"labels":["covers:sp-recur-b"]}]')"
is "covers.py: malformed JSON exits cleanly with no output" "" \
    "$(covers_of "$T/nofold.txt" 'not json')"

closed_of() {   # closed_of <window-days> <json>
    printf '%s' "$2" | SPIRA_REMEDY_WINDOW="$1" python3 "$CENSUS_DIR/covers_closed.py" "$T/nofold.txt"
}
recent="$(date -u -d '5 days ago' '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || date -u -v-5d '+%Y-%m-%dT%H:%M:%SZ')"
stale="$(date -u -d '40 days ago' '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || date -u -v-40d '+%Y-%m-%dT%H:%M:%SZ')"
want "covers_closed.py: bead closed within window is included" "sp-r1 sp-recur-x" \
    "$(closed_of 30 "[{\"id\":\"sp-r1\",\"closed_at\":\"$recent\",\"labels\":[\"covers:sp-recur-x\"]}]")"
is "covers_closed.py: bead closed beyond the window is excluded" "" \
    "$(closed_of 30 "[{\"id\":\"sp-r2\",\"closed_at\":\"$stale\",\"labels\":[\"covers:sp-recur-x\"]}]")"
is "covers_closed.py: a bead missing an id is skipped even with a covers: label" "" \
    "$(closed_of 30 "[{\"closed_at\":\"$recent\",\"labels\":[\"covers:sp-recur-x\"]}]")"

# ==============================================================================
echo
echo "census.sh — suppression states end-to-end against the scripted bd (UC-14)"
# ==============================================================================
CENSUS_OPEN_JSON="$T/open.json"
CENSUS_CLOSED_JSON="$T/closed.json"

# Open remedy suppresses.
printf '[{"id":"open-remedy","labels":["maechen-remedy","covers:sp-recur-fallback-test"]}]' > "$CENSUS_OPEN_JSON"
printf '[]' > "$CENSUS_CLOSED_JSON"
RUN_OPEN="$T/run-open"; mkdir -p "$RUN_OPEN"
open_out="$(run_census_fake "$RUN_OPEN")"
lack "open remedy: class suppressed by default" "sp-recur-fallback-test" "$open_out"
want "open remedy: --with-suppressed annotates it" "[suppressed]" \
    "$(run_census_fake "$RUN_OPEN" --with-suppressed)"

# covers: alone is the key: a remedy filed by any persona suppresses, and the bd query
# selects on covers:*, never on the maechen-remedy provenance label.
printf '[{"id":"open-delivers","labels":["delivers:action","covers:sp-recur-fallback-test"]}]' > "$CENSUS_OPEN_JSON"
CENSUS_LIST_ARGS_FILE="$T/list-args"; : > "$CENSUS_LIST_ARGS_FILE"
lack "open bead with covers: and no maechen-remedy suppresses the class" "sp-recur-fallback-test" \
    "$(run_census_fake "$RUN_OPEN")"
# One content query for every remedy, open or closed (sp-mve9i): no bd status filter.
want "the remedy query selects on covers:*" "1" "$(grep -c -- '--label-pattern covers:\*' "$CENSUS_LIST_ARGS_FILE")"
lack "no bd list query filters on bd status" "--status" "$(cat "$CENSUS_LIST_ARGS_FILE")"
lack "no bd list query filters on --label" "--label maechen" "$(cat "$CENSUS_LIST_ARGS_FILE")"
unset CENSUS_LIST_ARGS_FILE

# Unrelated covers: label does not suppress.
printf '[{"id":"open-unrelated","labels":["maechen-remedy","covers:sp-recur-unrelated"]}]' > "$CENSUS_OPEN_JSON"
want "unrelated covers: label does not suppress the class" "sp-recur-fallback-test" \
    "$(run_census_fake "$RUN_OPEN")"
printf '[]' > "$CENSUS_OPEN_JSON"

# Closed remedy, in-flight branch (named after the bead) -> still suppressed.
CENSUS_REPO="$T/fixture-repo"
git init -q "$CENSUS_REPO"
GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=t@t \
    git -C "$CENSUS_REPO" commit --allow-empty -q -m initial
printf '[{"id":"sp-inflight","closed_at":"%s","labels":["maechen-remedy","covers:sp-recur-fallback-test"]}]' \
    "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" > "$CENSUS_CLOSED_JSON"
git -C "$CENSUS_REPO" branch "spira/sp-inflight" >/dev/null 2>&1
RUN_CLOSED="$T/run-closed"; mkdir -p "$RUN_CLOSED"
closed_out="$(run_census_fake "$RUN_CLOSED")"
lack "closed remedy, branch in flight: still suppressed" "sp-recur-fallback-test" "$closed_out"
want "closed remedy, branch in flight: annotated closed-not-landed" \
    "[suppressed: remedy closed, not landed]" "$(run_census_fake "$RUN_CLOSED" --with-suppressed)"

# A commit naming the bead on the base is not a landing; the lifecycle record's LANDED is.
GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=t@t \
    git -C "$CENSUS_REPO" commit --allow-empty -q -m "spira: land sp-inflight"
lack "naming commit alone: still suppressed" "sp-recur-fallback-test" "$(run_census_fake "$RUN_CLOSED")"
echo LANDED > "$LCSTATE/sp-inflight"
landed_out="$(run_census_fake "$RUN_CLOSED")"
want "landed remedy: class reappears" "sp-recur-fallback-test" "$landed_out"
lack "landed remedy: no suppression annotation" "[suppressed" "$landed_out"

# Closed remedy, no branch naming the bead -> orphaned, unsuppressed and annotated.
git -C "$CENSUS_REPO" branch -D "spira/sp-inflight" >/dev/null 2>&1
printf '[{"id":"sp-orphan","closed_at":"%s","labels":["maechen-remedy","covers:sp-recur-fallback-test"]}]' \
    "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" > "$CENSUS_CLOSED_JSON"
RUN_ORPHAN="$T/run-orphan"; mkdir -p "$RUN_ORPHAN"
orphan_out="$(run_census_fake "$RUN_ORPHAN")"
want "orphaned remedy: class appears (nothing in flight)" "sp-recur-fallback-test" "$orphan_out"
want "orphaned remedy: annotated with the orphan marker and bead id" \
    "[orphaned remedy sp-orphan: closed, nothing in flight]" "$orphan_out"
lack "orphaned remedy: not marked closed-not-landed" "[suppressed: remedy closed" "$orphan_out"
unset CENSUS_REPO CENSUS_OPEN_JSON CENSUS_CLOSED_JSON

# ==============================================================================
echo
echo "census_events_run_sql — retries transient failure, surfaces the driver error (UC-15)"
# ==============================================================================
# Moved from test-census-events.sh: already T1-shaped (a fake bd, no database) — it
# belongs with the rest of the census decision logic, not in the real-Dolt suite.
_PRE_LIB_SPIRA_DB="${SPIRA_DB:-}"
# shellcheck disable=SC1090
. "$HERE/lib.sh"
SPIRA_DB="$_PRE_LIB_SPIRA_DB"; unset _PRE_LIB_SPIRA_DB

_fake_dir="$(mktemp -d)"
_calls_file="$_fake_dir/calls"
printf '0' > "$_calls_file"

cat > "$_fake_dir/bd" <<END
#!/bin/sh
n=\$(cat '$_calls_file' 2>/dev/null || printf 0)
n=\$((n+1))
printf '%d' "\$n" > '$_calls_file'
if [ "\$n" -lt 3 ]; then
    printf 'read tcp: i/o timeout\n' >&2
    exit 1
fi
exit 0
END
chmod +x "$_fake_dir/bd"
printf '#!/bin/sh\n[ "$1" = facts ] && echo "[]"\nexit 0\n' > "$_fake_dir/spira-lc"; chmod +x "$_fake_dir/spira-lc"

_retry_rc=0
tl_config SPIRA_BD="$_fake_dir/bd" SPIRA_DB="$_fake_dir"
SPIRA_LC_BIN="$_fake_dir/spira-lc" CENSUS_RETRY_DELAY_S=0 \
    census_events_run_sql >/dev/null 2>/dev/null || _retry_rc=$?
is "retry: succeeds after 2 failures" "0" "$_retry_rc"
is "retry: exactly 3 bd calls made" "3" "$(cat "$_calls_file")"

cat > "$_fake_dir/bd_fail" <<'FAKEFAIL'
#!/bin/sh
printf 'read tcp: i/o timeout\n' >&2
exit 1
FAKEFAIL
chmod +x "$_fake_dir/bd_fail"

_fail_err=""
_fail_rc=0
tl_config SPIRA_BD="$_fake_dir/bd_fail" SPIRA_DB="$_fake_dir"
_fail_err="$(SPIRA_LC_BIN="$_fake_dir/spira-lc" CENSUS_RETRY_DELAY_S=0 \
    census_events_run_sql 2>&1 >/dev/null)" || _fail_rc=$?
is    "all-fail: returns non-zero" "1" "$_fail_rc"
want  "all-fail: driver error in final message" "i/o timeout" "$_fail_err"

# ==============================================================================
echo
echo "census_event_rows_run_sql — shares the retry path with census_events_run_sql (sp-jcd0e)"
# ==============================================================================
# The two runners share _census_sql_retry (lib.sh); this proves the raw-rows query built
# for clustering gets the same retry behaviour as the aggregated one, not a second
# hand-written copy that could drift.
printf '0' > "$_calls_file"
_retry_rc=0
tl_config SPIRA_BD="$_fake_dir/bd" SPIRA_DB="$_fake_dir"
CENSUS_RETRY_DELAY_S=0 \
    census_event_rows_run_sql >/dev/null 2>/dev/null || _retry_rc=$?
is "raw-rows retry: succeeds after 2 failures" "0" "$_retry_rc"
is "raw-rows retry: exactly 3 bd calls made" "3" "$(cat "$_calls_file")"

_fail_err=""
_fail_rc=0
tl_config SPIRA_BD="$_fake_dir/bd_fail" SPIRA_DB="$_fake_dir"
_fail_err="$(CENSUS_RETRY_DELAY_S=0 \
    census_event_rows_run_sql 2>&1 >/dev/null)" || _fail_rc=$?
is    "raw-rows all-fail: returns non-zero" "1" "$_fail_rc"
want  "raw-rows all-fail: driver error in final message" "i/o timeout" "$_fail_err"
want  "raw-rows all-fail: error names the raw-rows caller, not the aggregated one" \
    "census_event_rows_run_sql" "$_fail_err"

rm -rf "$_fake_dir"

echo
tl_summary
