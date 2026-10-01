#!/usr/bin/env bash
#
# test-census-pipeline.sh — census.sh's decision logic, table-tested on canned rows.
#
#   ./test-census-pipeline.sh
#
# WHAT THIS TESTS
# ---------------
# The four python files under census/ (extracted from census.sh's former heredocs) on
# canned stdin/argv, with no database:
#   count.py          — event rows -> ranked "<beads> <events> <class>" lines
#   merge.py           — all-time + since-watermark count files -> ranked merged lines
#   covers.py           — open-remedy bead JSON -> covered classes (with fold-map)
#   covers_closed.py   — closed-remedy bead JSON -> "<bead-id> <class>" within the window
#
# and census.sh's own bash decision logic (watermark fallback/stderr, suppression
# state, orphan-vs-in-flight annotation) end-to-end against a scripted `bd`, never a
# real Dolt store — the property under test is the decision, not the database read,
# which UC-ops-detection-remediation-12 covers with 3 real rows in test-census.sh.
#
# tier: T1
# covers: spira/census/count.py spira/census/merge.py spira/census/covers.py spira/census/covers_closed.py census/src/* UC-ops-detection-remediation-11 UC-ops-detection-remediation-13 UC-ops-detection-remediation-14 UC-ops-detection-remediation-15
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
echo "count.py — cause-suffix mapping per event_type (UC-11)"
# ==============================================================================
count_of() { python3 "$CENSUS_DIR/count.py" <<<"$1"; }

want "requeued+cause -> sp-requeue-<cause>" "2 3 sp-requeue-prod-dirty" \
    "$(count_of 'requeued | prod-dirty | 2 | 3')"
want "recurred+cause -> sp-recur-<cause>" "1 1 sp-recur-suite-red" \
    "$(count_of 'recurred | suite-red | 1 | 1')"
want "reclaimed+no cause -> bare sp-reclaim" "1 2 sp-reclaim" \
    "$(count_of 'reclaimed |  | 1 | 2')"
lack "bare sp-reclaim has no trailing dash-suffix" "sp-reclaim-" \
    "$(count_of 'reclaimed |  | 1 | 2')"
want "reclaimed+cause -> sp-reclaim-<cause>" "1 3 sp-reclaim-timeout" \
    "$(count_of 'reclaimed | timeout | 1 | 3')"
want "lapsed+cause -> sp-lapsed-<cause>" "1 1 sp-lapsed-cause-x" \
    "$(count_of 'lapsed | cause-x | 1 | 1')"
want "reopen (already SQL-folded) + cause -> sp-reopen-<cause>" "1 1 sp-reopen-rebase-conflict" \
    "$(count_of 'reopen | rebase-conflict | 1 | 1')"
want "unknown event_type is silently ignored (positive control: known type is not)" \
    "" "$(count_of 'mystery | x | 5 | 5')"
want "zero-bead row is skipped" "" "$(count_of 'recurred | x | 0 | 0')"

# ==============================================================================
echo
echo "count.py — NULL cause does not column-shift (D3: db-i0jd)"
# ==============================================================================
# Regression: stripping empty fields BY VALUE (not by position) turns a 4-column row
# ("reopened", "", 2, 8) into 3 columns; the 3-column branch then reads the event count
# (8) as the bead count, reporting "8 sp-reopen" instead of "2 sp-reopen".
nullcause_out="$(count_of 'reopened |  | 2 | 8')"
want "empty cause -> sp-reopen-unrecorded, not a bare class" "2 8 sp-reopen-unrecorded" "$nullcause_out"
lack "distinct-bead count is not misread as event count" "8 8 sp-reopen" "$nullcause_out"

# ==============================================================================
echo
echo "count.py — no double-count across a requeue+reopen pair"
# ==============================================================================
# The requeue/reopen fold (sp-requeue-merge-conflict -> sp-reopen-rebase-conflict) happens
# in the SQL (lib.sh:_census_events_sql), never in count.py: count.py sees only whatever
# rows the query emits. This proves count.py keeps each row's class independent — if the
# SQL fold ever regressed and re-emitted the raw pair, count.py would report two classes,
# each with its own bead, rather than silently merging or losing one.
pair_out="$(printf 'requeued | merge-conflict | 1 | 1\nreopen | rebase-conflict | 1 | 1\n' \
    | python3 "$CENSUS_DIR/count.py")"
want "raw pair: requeue class present" "1 1 sp-requeue-merge-conflict" "$pair_out"
want "raw pair: reopen class present, not merged into the requeue line" \
    "1 1 sp-reopen-rebase-conflict" "$pair_out"
is "raw pair: exactly two class lines (no cross-class bleed)" "2" "$(printf '%s\n' "$pair_out" | grep -c .)"

# Two distinct causes for the same event_type do not bleed into one another's counts.
two_cause_out="$(printf 'reopened |  | 2 | 8\nreopened | gate-red | 1 | 1\n' \
    | python3 "$CENSUS_DIR/count.py")"
want "distinct-cause reopen: unrecorded class correct" "2 8 sp-reopen-unrecorded" "$two_cause_out"
want "distinct-cause reopen: named class correct"      "1 1 sp-reopen-gate-red"   "$two_cause_out"

# ==============================================================================
echo
echo "count.py — ranking: distinct beads first, event count as tie-break"
# ==============================================================================
rank_out="$(printf 'recurred | one-bead | 1 | 9\nrecurred | three-beads | 3 | 3\nrecurred | two-beads | 2 | 2\n' \
    | python3 "$CENSUS_DIR/count.py")"
first_cls="$(printf '%s\n' "$rank_out" | head -1 | awk '{print $3}')"
is "3-bead class ranks first despite fewer events than the 1-bead class" \
    "sp-recur-three-beads" "$first_cls"

# ==============================================================================
echo
echo "count.py — header/separator lines and the empty store"
# ==============================================================================
sql_shape="$(printf '+------+\n| event_type | new_value | n_beads | n_events |\n+------+\n| recurred | shape-test | 1 | 1 |\n+------+\n')"
want "realistic bd-sql framing: header/separator ignored, data row counted" \
    "1 1 sp-recur-shape-test" "$(python3 "$CENSUS_DIR/count.py" <<<"$sql_shape")"
is "empty store: no input produces no output" "" "$(printf '' | python3 "$CENSUS_DIR/count.py")"

# ==============================================================================
echo
echo "merge.py — since-watermark ranking, with all-time carried alongside (UC-13)"
# ==============================================================================
printf '2 5 sp-recur-old-class\n1 1 sp-recur-new-class\n' > "$T/all_time.txt"
printf '0 0 sp-recur-old-class\n1 1 sp-recur-new-class\n' > "$T/since_wm.txt"
merge_out="$(python3 "$CENSUS_DIR/merge.py" "$T/all_time.txt" "$T/since_wm.txt")"
first_merge="$(printf '%s\n' "$merge_out" | head -1 | awk '{print $2}')"
is "live-since class ranks first despite a smaller all-time count" "sp-recur-new-class" "$first_merge"
want "stale class shows 0 since, all-time preserved" \
    "0 sp-recur-old-class (0 detections, 2 all-time)" "$merge_out"
want "live class shows since count with all-time alongside" \
    "1 sp-recur-new-class (1 detections, 1 all-time)" "$merge_out"

# A class present only since the watermark (no all-time row — cannot happen from one
# query, but merge.py must not crash reading two independently-produced files).
printf '' > "$T/all_time_empty.txt"
printf '1 1 sp-recur-fresh\n' > "$T/since_only.txt"
want "since-only class: all-time reads as 0, not a crash" \
    "1 sp-recur-fresh (1 detections, 0 all-time)" \
    "$(python3 "$CENSUS_DIR/merge.py" "$T/all_time_empty.txt" "$T/since_only.txt")"

# Malformed line ignored rather than raising.
printf 'garbage line with no count\n1 1 sp-recur-ok\n' > "$T/malformed.txt"
printf '' > "$T/empty2.txt"
want "malformed all-time line is skipped, not fatal" "0 sp-recur-ok (0 detections, 1 all-time)" \
    "$(python3 "$CENSUS_DIR/merge.py" "$T/malformed.txt" "$T/empty2.txt")"

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
        case " $* " in
            *" --status closed "*) cat "${CENSUS_CLOSED_JSON:-/dev/null}" ;;
            *)                     cat "${CENSUS_OPEN_JSON:-/dev/null}" ;;
        esac
        exit 0
        ;;
    *) exit 0 ;;
esac
STUB
chmod +x "$FAKE_BD"

CENSUS_SQL_FILE="$T/sql_one_class.txt"
printf 'recurred | fallback-test | 1 | 1\n' > "$CENSUS_SQL_FILE"

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
run_census_fake() {   # run_census_fake <SPIRA_RUN> [census-args...]
    local rundir="$1"; shift
    env SPIRA_NOW="$CENSUS_FIXED_HOST_EPOCH" \
        SPIRA_BD="$FAKE_BD" \
        SPIRA_DB="$T/fixture.db" \
        SPIRA_MAECHEN_REMEDY_LABEL=maechen-remedy \
        SPIRA_CONF="$T/no-conf" \
        SPIRA_HOME="$HERE" \
        SPIRA_RUN="$rundir" \
        SPIRA_REPO="${CENSUS_REPO:-$T/norepo}" \
        SPIRA_REPO_MAP="$T/no-repo-map" \
        CENSUS_SQL_FILE="$CENSUS_SQL_FILE" \
        CENSUS_SQL_RC="${CENSUS_SQL_RC:-0}" \
        CENSUS_SKEW_FILE="$CENSUS_SKEW_FILE" \
        CENSUS_SKEW_RC="${CENSUS_SKEW_RC:-0}" \
        SPIRA_CENSUS_CLOCK_SKEW_TOLERANCE_S="${SPIRA_CENSUS_CLOCK_SKEW_TOLERANCE_S:-120}" \
        CENSUS_OPEN_JSON="${CENSUS_OPEN_JSON:-}" \
        CENSUS_CLOSED_JSON="${CENSUS_CLOSED_JSON:-}" \
        "$CENSUS" "$@"
}

nowm_out="$(run_census_fake "$RUN_NO_WM" 2>"$T/nowm.stderr")"
want "no watermark file: falls back to all-time output" "1 sp-recur-fallback-test (1 detections)" "$nowm_out"
want "no watermark file: stderr says so" "no watermark file" "$(cat "$T/nowm.stderr")"

RUN_BAD_WM="$T/run-bad-wm"; mkdir -p "$RUN_BAD_WM"
printf 'not-a-number' > "$RUN_BAD_WM/maechen.watermark"
badwm_out="$(run_census_fake "$RUN_BAD_WM" 2>"$T/badwm.stderr")"
want "unreadable watermark: falls back to all-time output" "1 sp-recur-fallback-test (1 detections)" "$badwm_out"
want "unreadable watermark: stderr says so" "unreadable" "$(cat "$T/badwm.stderr")"

RUN_WM="$T/run-wm"; mkdir -p "$RUN_WM"
printf '1000000000\n' > "$RUN_WM/maechen.watermark"
wm_out="$(run_census_fake "$RUN_WM" 2>"$T/wm.stderr")"
want "valid watermark: since-watermark format used (merge.py path)" \
    "sp-recur-fallback-test (1 detections, 1 all-time)" "$wm_out"
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
want "clock in sync: ranking still produced" "1 sp-recur-fallback-test (1 detections)" "$insync_out"
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
want "widened tolerance: ranking produced" "1 sp-recur-fallback-test (1 detections)" "$tolerant_out"

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
    "1 sp-recur-fallback-test (1 detections)" "$pdt_insync_out"
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
printf '[{"labels":["maechen-remedy","covers:sp-recur-fallback-test"]}]' > "$CENSUS_OPEN_JSON"
printf '[]' > "$CENSUS_CLOSED_JSON"
RUN_OPEN="$T/run-open"; mkdir -p "$RUN_OPEN"
open_out="$(run_census_fake "$RUN_OPEN")"
lack "open remedy: class suppressed by default" "sp-recur-fallback-test" "$open_out"
want "open remedy: --with-suppressed annotates it" "[suppressed]" \
    "$(run_census_fake "$RUN_OPEN" --with-suppressed)"

# Unrelated covers: label does not suppress.
printf '[{"labels":["maechen-remedy","covers:sp-recur-unrelated"]}]' > "$CENSUS_OPEN_JSON"
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

# Land it: a commit naming the bead on the base -> unsuppressed.
GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=t@t \
    git -C "$CENSUS_REPO" commit --allow-empty -q -m "spira: land sp-inflight"
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

_retry_rc=0
CENSUS_RETRY_DELAY_S=0 SPIRA_BD="$_fake_dir/bd" SPIRA_DB="$_fake_dir" \
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
_fail_err="$(CENSUS_RETRY_DELAY_S=0 SPIRA_BD="$_fake_dir/bd_fail" SPIRA_DB="$_fake_dir" \
    census_events_run_sql 2>&1 >/dev/null)" || _fail_rc=$?
is    "all-fail: returns non-zero" "1" "$_fail_rc"
want  "all-fail: driver error in final message" "i/o timeout" "$_fail_err"

rm -rf "$_fake_dir"

echo
tl_summary
