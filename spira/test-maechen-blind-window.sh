#!/usr/bin/env bash
#
# test-maechen-blind-window.sh — regression for sp-pyzp: maechen-trigger.sh must not
#   advance the watermark before filing the trigger bead. If it does, census.sh reads the
#   advanced watermark and sees no events from the window that caused the trigger to fire.
#
#   ./test-maechen-blind-window.sh
#
# THE DEFECT (sp-pyzp)
# --------------------
# maechen-trigger.sh used to write now_ts to the watermark file before filing the bead.
# census.sh reads that same file. So when the Maechen pass ran:
#   - trigger measured events in [watermark_ts, now_ts]
#   - trigger wrote now_ts to the watermark file
#   - census read now_ts from the file
#   - census queried events since now_ts → found NOTHING
# A pass that fired because 6 events existed reported "0 classes met threshold".
#
# SCENARIO
# --------
# T         = one hour ago (the old watermark)
# T+60      = 3600 - 60 seconds ago — three recurred events inserted here
# trigger runs "now" (well after T) — gap threshold = 1s so it fires immediately
# census.sh runs after the trigger
#
# UNFIXED: trigger writes current time (~T+3600) to watermark; census sees 0 events
# FIXED:   trigger leaves watermark at T;              census sees 3 events ✓
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
# -------------------------------------------------------
# Before the since-watermark run, an all-time run is executed (no watermark file) to
# confirm the 3 events are visible. An empty result there means the fixture is broken,
# not that the fix worked.
#
# SEEN TO FAIL BEFORE THE FIX (law-a-regression-test-must-be-seen-to-fail)
# -------------------------------------------------------------------------
# Run against the unfixed tree (watermark advanced before filing):
#   FAIL — census sees events from trigger window: wanted [3 sp-recur-blind-window-test] in [0 sp-recur-blind-window-test ...]
# The fix (not advancing the watermark in the trigger) makes this pass.
#
# covers: spira/maechen-trigger.sh spira/census.sh spira/lib.sh
# hermetic-ok: uses a fixture database; stub bd for maechen-trigger; no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "${2:-}"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want() { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
lack() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1" ;; esac; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-maechen-blind-window
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export SPIRA_TESTDB_MODE=server
testdb_up maechen-blind-window || {
    printf 'SKIP test-maechen-blind-window: server testdb not available\n' >&2
    exit 77
}

# Source lib.sh for bump_recur. Protect SPIRA_DB since lib.sh re-sources conf.sh.
_PRE_LIB_SPIRA_DB="$SPIRA_DB"
# shellcheck disable=SC1090
. "$HERE/lib.sh"
SPIRA_DB="$_PRE_LIB_SPIRA_DB"; unset _PRE_LIB_SPIRA_DB

CENSUS="$HERE/census.sh"
TRIGSH="$HERE/maechen-trigger.sh"
B() { bd -C "$SPIRA_DB" "$@"; }

# ---------------------------------------------------------------------------
# STUB BD for maechen-trigger.sh: we need the trigger to "succeed" (exit 0)
# without actually writing a bead to the fixture db.  The trigger script calls
# bd list (dedup check) and bd create (file the bead).  The stub returns an
# empty list for list, and exits 0 for create.
# ---------------------------------------------------------------------------
STUB_BD="$TMP/stub-bd"
BD_LOG="$TMP/bd.log"
cat > "$STUB_BD" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$BD_LOG_PATH"
[ "${1:-}" = "-C" ] && shift 2
case "${1:-}" in
    list) printf '[]'; exit 0 ;;
    *)    exit 0 ;;
esac
STUB
chmod +x "$STUB_BD"

# Insert a recurred event with a specific timestamp.
_insert_event_at() {   # _insert_event_at <bead_id> <cause> <unix_ts>
    local id="$1" cause="$2" ts="$3"
    local uuid
    uuid="$(python3 -c 'import uuid; print(str(uuid.uuid4()))' 2>/dev/null)" || return 1
    local q="INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', 'recurred', 'test', '$cause', FROM_UNIXTIME(${ts}))"
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" sql "$q" >/dev/null 2>&1
}

run_census() {
    env SPIRA_DB="$SPIRA_DB" \
        SPIRA_MAECHEN_REMEDY_LABEL=maechen-remedy \
        SPIRA_CONF="$TMP/no-conf" \
        SPIRA_HOME="$HERE" \
        SPIRA_RUN="$TMP/run" \
        bash "$CENSUS" "$@" 2>/dev/null
}

# Run maechen-trigger.sh with the stub bd, gap threshold of 1s (always fires), and
# the given SPIRA_RUN directory so we can inspect the watermark file after.
run_trigger() {
    : > "$BD_LOG"
    env -i HOME="$TMP" PATH="$HERE:/usr/bin:/bin" \
        SPIRA_CONF="$TMP/no-conf" \
        SPIRA_BD="$STUB_BD" \
        BD_LOG_PATH="$BD_LOG" \
        BD_LIST_OUTPUT="[]" \
        SPIRA_DB="$TMP/fixture.db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_REPO="$TMP/norepo" \
        SPIRA_MAECHEN_LABEL="maechen-sweep" \
        SPIRA_SCOPE_LABEL="spira" \
        SPIRA_MAECHEN_MAX_GAP_SECONDS=1 \
        SPIRA_MAECHEN_LANDING_INTERVAL=999 \
        bash "$TRIGSH" 2>&1
}

echo "test-maechen-blind-window.sh"
mkdir -p "$TMP/run"

# ==============================================================================
echo
echo "SETUP: seed three recurred events at T+60 (one hour ago + 60s)"
# ==============================================================================
testdb_reset

# Create a bead the events will reference.
BID="$(B create "blind-window-test-bead" --type task --priority 3 \
    --labels spira --silent 2>/dev/null | tr -d '[:space:]')"
[ -n "$BID" ] \
    || { bad "fixture bead created" "create failed"
         printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"; exit 1; }

# T = one hour ago
T=$(( $(date +%s) - 3600 ))
# Events at T+60 (59 minutes ago — clearly after T, clearly before now)
T_EVENT=$(( T + 60 ))

_insert_event_at "$BID" "blind-window-test" "$T_EVENT"
_insert_event_at "$BID" "blind-window-test" "$T_EVENT"
_insert_event_at "$BID" "blind-window-test" "$T_EVENT"

# ==============================================================================
echo
echo "POSITIVE CONTROL: all-time census (no watermark) sees the 3 events"
# ==============================================================================
# Confirms the events reached the database. If this fails, the fixture is broken.
out_alltime="$(run_census)"
want "all events visible all-time (3 detections for 1 bead)" "sp-recur-blind-window-test (3 detections" "$out_alltime"

# ==============================================================================
echo
echo "MAIN TEST: trigger fires → census must see the events in the trigger's window"
# ==============================================================================
# Set watermark to T (one hour ago) so the trigger fires on elapsed time.
printf '%d\n' "$T" > "$TMP/run/maechen.watermark"
wm_before="$(cat "$TMP/run/maechen.watermark")"

# Run the trigger. It should fire (gap threshold = 1s, elapsed ~= 3600s).
trigger_out="$(run_trigger)"; trigger_rc=$?
is "trigger exits 0 (fires)" 0 "$trigger_rc"
want "trigger logged that it fired" "elapsed" "$trigger_out"

# Capture the watermark after the trigger.
wm_after="$(cat "$TMP/run/maechen.watermark" 2>/dev/null | tr -d '[:space:]' || true)"

# FIXED: the trigger must NOT have advanced the watermark.
# The census needs the old watermark (T) to see the events at T+60.
is "watermark unchanged by trigger (still T)" "$wm_before" "$wm_after"

# Run census.sh — it reads the watermark file and queries events since that timestamp.
out="$(run_census)"

# THE KEY ASSERTION: census must report the 3 events that caused the trigger to fire.
# UNFIXED: census reads the advanced watermark (current time), sees 0 events.
# FIXED:   census reads the original watermark (T), sees 3 events.
want "census sees 3 detections from trigger window (1 distinct bead)" \
    "sp-recur-blind-window-test (3 detections" "$out"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
