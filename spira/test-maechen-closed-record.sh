#!/usr/bin/env bash
# timeout: 150
#
# test-maechen-closed-record.sh — the closed-record INVALID-CLOSED trigger and allowlist.
#
#   ./test-maechen-closed-record.sh
#
# WHAT THIS SUITE GUARDS
# ----------------------
# 1. detect_invalid_closed (lib.sh) honours the allowlist at
#    $SPIRA_RUN/invalid-closed.allow: allowlisted ids are emitted as ALLOWED-IC
#    rows (not counted by the pane) instead of INVALID-CLOSED or UNFILED-FOLLOW.
#
# 2. maechen-trigger.sh fires a third trigger condition when detect_invalid_closed
#    returns INVALID-CLOSED or UNFILED-FOLLOW rows; ALLOWED-IC rows do not trigger.
#    The trigger bead carries the rows in its description.
#
# 3. End-to-end acceptance: fixture store with three closed beads (admission,
#    quotation, unfiled-follow). First trigger run files one bead carrying all
#    three rows. Second run files nothing (dedup). After simulated Maechen pass
#    (reopen, allowlist, update close_reason), detect_invalid_closed returns
#    zero counted rows and one allowed line.
#
# POSITIVE CONTROLS (law-absence-needs-a-positive-control)
# --------------------------------------------------------
# Every absence assertion is paired with a presence assertion that proves the
# check is live. Bead ids are pinned to non-default strings; allowlist state
# is reset between sections.
#
# covers: spira/maechen-trigger.sh spira/lib.sh spira/cockpit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-maechen-closed-record
. "$HERE/testlib.sh"

TRIGSH="$HERE/maechen-trigger.sh"
T="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$T"' EXIT INT TERM
NONE="$T/none.conf"

# ---------------------------------------------------------------------------
# THROWAWAY GIT REPO — required by the trigger's lane check.
# ---------------------------------------------------------------------------
TESTREPO="$T/testrepo"
git init -q "$TESTREPO"
git -C "$TESTREPO" config user.email "test@example.com"
git -C "$TESTREPO" config user.name "Test"
git -C "$TESTREPO" commit --allow-empty -q -m "initial"
mkdir -p "$TESTREPO/.git/refs/remotes/origin"
git -C "$TESTREPO" rev-parse HEAD > "$TESTREPO/.git/refs/remotes/origin/main"
SELFMAP="$T/selfmap"
printf 'testrepo | %s | push | origin/main | | true | self\n' "$TESTREPO" > "$SELFMAP"

RUNDIR="$T/run"
mkdir -p "$RUNDIR"
ALLOW_FILE="$RUNDIR/invalid-closed.allow"
WATERMARK_FILE="$RUNDIR/maechen.watermark"
LASTPASS_FILE="$RUNDIR/maechen.lastpass"
now_ts="$(date +%s)"
printf '%d\n' "$now_ts" > "$WATERMARK_FILE"
printf '%d\n' "$now_ts" > "$LASTPASS_FILE"

# SCOPE labels pinned to non-defaults (law-gates-run-in-a-clean-environment).
ALLW_SCOPE="sptest-cr-allow"

echo "test-maechen-closed-record.sh"

# ==========================================================================================
echo
echo "ALLOWLIST: INVALID-CLOSED id in allowlist → ALLOWED-IC, not counted"
# ==========================================================================================
# Plant a closed bead whose close reason hits RED_FLAGS, then show it clears when
# allowlisted. Uses testdb_seed to set close_reason directly (bd close stores it).
testdb_up maechen_closed_record_allow || { bad "allowlist fixture: testdb_up failed" ""; }

testdb_reset
testdb_seed <<JSONL
{"id":"sp-cr-ic1","title":"admitting workaround","status":"closed","issue_type":"task","labels":["$ALLW_SCOPE","plan","repo:pushrepo"],"close_reason":"Applied a temporary workaround — real fix needed."}
JSONL

run_livelock() {
    env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
        SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$T/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$T" \
        SPIRA_RUN="$RUNDIR" SPIRA_DB="$SPIRA_DB" \
        SPIRA_REPO_MAP="$SELFMAP" SPIRA_GOAL=sp-goal \
        SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=awaiting-ci \
        SPIRA_SPIKE_LABEL=spike \
        SPIRA_SCOPE_LABEL="$ALLW_SCOPE" \
        bash "$HERE/cockpit.sh" livelock 2>/dev/null
}

# Without allowlist: INVALID-CLOSED row expected.
out="$(run_livelock)"
want  "no allowlist: INVALID-CLOSED row present"  "INVALID-CLOSED" "$out"
want  "no allowlist: bead id in row"              "sp-cr-ic1"      "$out"
is "no allowlist: SP_INVALID_CLOSED=1" "1" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_INVALID_CLOSED=//p' | head -1)"

# Add to allowlist.
printf 'sp-cr-ic1 quotation: reason names the flag list, not a remainder\n' > "$ALLOW_FILE"

out="$(run_livelock)"
nowant "allowlisted: no INVALID-CLOSED row"   "INVALID-CLOSED"  "$out"
want   "allowlisted: ALLOWED-IC row present"  "ALLOWED-IC"      "$out"
want   "allowlisted: bead id in allowed row"  "sp-cr-ic1"       "$out"
is "allowlisted: SP_INVALID_CLOSED=0" "0" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_INVALID_CLOSED=//p' | head -1)"

rm -f "$ALLOW_FILE"
testdb_drop

# ==========================================================================================
echo
echo "ALLOWLIST: UNFILED-FOLLOW id in allowlist → ALLOWED-IC, not counted"
# ==========================================================================================
testdb_up maechen_closed_record_uf_allow || { bad "uf-allowlist fixture: testdb_up failed" ""; }

testdb_reset
testdb_seed <<JSONL
{"id":"sp-cr-uf1","title":"unfiled follow bead","status":"closed","issue_type":"task","labels":["$ALLW_SCOPE","plan","repo:pushrepo"],"close_reason":"The real fix is a proper guard. Deployed the stopgap."}
JSONL

run_livelock_uf() {
    env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
        SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$T/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$T" \
        SPIRA_RUN="$RUNDIR" SPIRA_DB="$SPIRA_DB" \
        SPIRA_REPO_MAP="$SELFMAP" SPIRA_GOAL=sp-goal \
        SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=awaiting-ci \
        SPIRA_SPIKE_LABEL=spike \
        SPIRA_SCOPE_LABEL="$ALLW_SCOPE" \
        bash "$HERE/cockpit.sh" livelock 2>/dev/null
}

out="$(run_livelock_uf)"
want "no allowlist: UNFILED-FOLLOW row present" "UNFILED-FOLLOW" "$out"
is "no allowlist: SP_UNFILED_FOLLOW=1" "1" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_UNFILED_FOLLOW=//p' | head -1)"

printf 'sp-cr-uf1 follow-up filed as sp-fake1\n' > "$ALLOW_FILE"
out="$(run_livelock_uf)"
nowant "allowlisted: no UNFILED-FOLLOW row"  "UNFILED-FOLLOW" "$out"
want   "allowlisted: ALLOWED-IC row present" "ALLOWED-IC"     "$out"
want   "allowlisted: bead id in row"         "sp-cr-uf1"      "$out"
is "uf-allowlisted: SP_UNFILED_FOLLOW=0" "0" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_UNFILED_FOLLOW=//p' | head -1)"

rm -f "$ALLOW_FILE"
testdb_drop

# ==========================================================================================
echo
echo "TRIGGER: INVALID-CLOSED rows fire the third trigger condition"
# ==========================================================================================
# Use a real fixture so the --status closed filter in detect_invalid_closed is exercised
# (law-prefer-the-real-dependency). A stub bd cannot distinguish list --status queries.
testdb_up maechen_closed_record_trigger || { bad "trigger fixture: testdb_up failed" ""; }

TR_SCOPE="sptest-cr-trig"
TR_MAE="sptest-cr-trig-mae"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-cr-trig1","title":"admitting close","status":"closed","issue_type":"task","labels":["$TR_SCOPE","plan","repo:pushrepo"],"close_reason":"Applied a temporary workaround, real fix still needed."}
JSONL

# Set lastpass=now, watermark=now so neither time nor landing trigger fires.
printf '%d\n' "$now_ts" > "$WATERMARK_FILE"
printf '%d\n' "$now_ts" > "$LASTPASS_FILE"

_tr_bd="$(command -v "${SPIRA_BD:-bd}" 2>/dev/null || printf '%s' "${SPIRA_BD:-bd}")"
tr_out="$(env -i HOME="$T" \
    PATH="${TESTDB_BIN:+$TESTDB_BIN:}$HERE:/usr/bin:/bin" \
    SPIRA_CONF="$NONE" \
    SPIRA_BD="$_tr_bd" \
    SPIRA_DB="$SPIRA_DB" \
    SPIRA_HOME="$HERE" \
    SPIRA_RUN="$RUNDIR" \
    SPIRA_REPO="$TESTREPO" \
    SPIRA_REPO_MAP="$SELFMAP" \
    SPIRA_MAECHEN_LABEL="$TR_MAE" \
    SPIRA_SCOPE_LABEL="$TR_SCOPE" \
    SPIRA_MAECHEN_MAX_GAP_SECONDS=999999 \
    SPIRA_MAECHEN_LANDING_INTERVAL=999 \
    bash "$TRIGSH" 2>&1)"; tr_rc=$?

is   "invalid-closed trigger: exits 0"             0 "$tr_rc"
want "invalid-closed trigger: log mentions rows"   "invalid-closed trigger:" "$tr_out"

# Verify bead filed with closed-record rows in description.
tr_bead="$("$SPIRA_BD" -C "$SPIRA_DB" list \
    --status open --label "$TR_SCOPE,$TR_MAE" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d[0]["id"] if d else "")' 2>/dev/null \
    || echo "")"
is "invalid-closed trigger: bead filed" "1" "$([ -n "$tr_bead" ] && echo 1 || echo 0)"
tr_desc="$("$SPIRA_BD" -C "$SPIRA_DB" show "$tr_bead" 2>/dev/null || echo "")"
want "trigger bead description: INVALID-CLOSED row" "INVALID-CLOSED"  "$tr_desc"
want "trigger bead description: bead id present"    "sp-cr-trig1"     "$tr_desc"

# ==========================================================================================
echo
echo "TRIGGER: second run files nothing new (dedup)"
# ==========================================================================================
tr_out2="$(env -i HOME="$T" \
    PATH="${TESTDB_BIN:+$TESTDB_BIN:}$HERE:/usr/bin:/bin" \
    SPIRA_CONF="$NONE" \
    SPIRA_BD="$_tr_bd" \
    SPIRA_DB="$SPIRA_DB" \
    SPIRA_HOME="$HERE" \
    SPIRA_RUN="$RUNDIR" \
    SPIRA_REPO="$TESTREPO" \
    SPIRA_REPO_MAP="$SELFMAP" \
    SPIRA_MAECHEN_LABEL="$TR_MAE" \
    SPIRA_SCOPE_LABEL="$TR_SCOPE" \
    SPIRA_MAECHEN_MAX_GAP_SECONDS=999999 \
    SPIRA_MAECHEN_LANDING_INTERVAL=999 \
    bash "$TRIGSH" 2>&1)"; tr_rc2=$?
is   "dedup second run: exits 0"      0          "$tr_rc2"
want "dedup second run: logs skip"    "skipping" "$tr_out2"
tr_count="$("$SPIRA_BD" -C "$SPIRA_DB" list \
    --status open --label "$TR_SCOPE,$TR_MAE" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d))' 2>/dev/null || echo 0)"
is "dedup: still exactly one bead" "1" "${tr_count:-0}"

testdb_drop

# ==========================================================================================
echo
echo "TRIGGER: no fire when all rows are in the allowlist (ALLOWED-IC only)"
# ==========================================================================================
testdb_up maechen_closed_record_nofire || { bad "no-fire fixture: testdb_up failed" ""; }

ANT_SCOPE="sptest-cr-ant"
ANT_MAE="sptest-cr-ant-mae"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-cr-ant1","title":"allowlisted bead","status":"closed","issue_type":"task","labels":["$ANT_SCOPE","plan","repo:pushrepo"],"close_reason":"Applied a temporary workaround."}
JSONL

printf '%d\n' "$now_ts" > "$WATERMARK_FILE"
printf '%d\n' "$now_ts" > "$LASTPASS_FILE"
printf 'sp-cr-ant1 quotation: test bead\n' > "$ALLOW_FILE"

_ant_bd="$(command -v "${SPIRA_BD:-bd}" 2>/dev/null || printf '%s' "${SPIRA_BD:-bd}")"
ant_out="$(env -i HOME="$T" \
    PATH="${TESTDB_BIN:+$TESTDB_BIN:}$HERE:/usr/bin:/bin" \
    SPIRA_CONF="$NONE" \
    SPIRA_BD="$_ant_bd" \
    SPIRA_DB="$SPIRA_DB" \
    SPIRA_HOME="$HERE" \
    SPIRA_RUN="$RUNDIR" \
    SPIRA_REPO="$TESTREPO" \
    SPIRA_REPO_MAP="$SELFMAP" \
    SPIRA_MAECHEN_LABEL="$ANT_MAE" \
    SPIRA_SCOPE_LABEL="$ANT_SCOPE" \
    SPIRA_MAECHEN_MAX_GAP_SECONDS=999999 \
    SPIRA_MAECHEN_LANDING_INTERVAL=999 \
    bash "$TRIGSH" 2>&1)"; ant_rc=$?

is     "allowlisted only: trigger exits 0"    0            "$ant_rc"
want   "allowlisted only: logs no trigger"    "no trigger" "$ant_out"
nowant "allowlisted only: no rows trigger"    "invalid-closed trigger:" "$ant_out"
ant_count="$("$SPIRA_BD" -C "$SPIRA_DB" list \
    --status open --label "$ANT_SCOPE,$ANT_MAE" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d))' 2>/dev/null || echo 0)"
is "allowlisted only: no trigger bead filed" "0" "${ant_count:-0}"

rm -f "$ALLOW_FILE"
testdb_drop

# ==========================================================================================
echo
echo "END-TO-END: three closed beads, trigger, simulated pass, zero counted rows after"
# ==========================================================================================
# Three closed beads in the fixture:
#   A (sp-cr-e2e-a) — INVALID-CLOSED (admission): temporary workaround admitted
#   B (sp-cr-e2e-b) — INVALID-CLOSED (quotation): reason discusses the flag list
#   C (sp-cr-e2e-c) — UNFILED-FOLLOW: "the real fix" without a tracking reference
#
# Sequence:
#   1. Trigger fires → one maechen bead carrying all three rows
#   2. Second trigger run → dedup (open bead already exists)
#   3. Simulated Maechen pass:
#       A → reopen (admission)
#       B → allowlist (quotation)
#       C → update close_reason to include tracking reference (clears UNFILED-FOLLOW)
#   4. detect_invalid_closed → 0 counted rows, 1 ALLOWED-IC line
testdb_up maechen_closed_record_e2e || { bad "e2e fixture: testdb_up failed" ""; }

E2E_SCOPE="sptest-cr-e2e"
E2E_MAE="sptest-cr-e2e-mae"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-cr-e2e-a","title":"bead A admission","status":"closed","issue_type":"task","labels":["$E2E_SCOPE","plan","repo:pushrepo"],"close_reason":"Applied a temporary workaround pending the real fix."}
{"id":"sp-cr-e2e-b","title":"bead B quotation","status":"closed","issue_type":"task","labels":["$E2E_SCOPE","plan","repo:pushrepo"],"close_reason":"Built close-reason-flags.py which adds 'TEMPORARY WORKAROUND' to RED_FLAGS. Landed on origin/main."}
{"id":"sp-cr-e2e-c","title":"bead C unfiled","status":"closed","issue_type":"task","labels":["$E2E_SCOPE","plan","repo:pushrepo"],"close_reason":"The real fix requires a schema migration. Deployed the stopgap."}
JSONL

rm -f "$ALLOW_FILE"
printf '%d\n' "$now_ts" > "$WATERMARK_FILE"
printf '%d\n' "$now_ts" > "$LASTPASS_FILE"

_e2e_bd="$(command -v "${SPIRA_BD:-bd}" 2>/dev/null || printf '%s' "${SPIRA_BD:-bd}")"

# Step 1: trigger files one bead carrying all three rows.
e2e_trig1="$(env -i HOME="$T" \
    PATH="${TESTDB_BIN:+$TESTDB_BIN:}$HERE:/usr/bin:/bin" \
    SPIRA_CONF="$NONE" \
    SPIRA_BD="$_e2e_bd" \
    SPIRA_DB="$SPIRA_DB" \
    SPIRA_HOME="$HERE" \
    SPIRA_RUN="$RUNDIR" \
    SPIRA_REPO="$TESTREPO" \
    SPIRA_REPO_MAP="$SELFMAP" \
    SPIRA_MAECHEN_LABEL="$E2E_MAE" \
    SPIRA_SCOPE_LABEL="$E2E_SCOPE" \
    SPIRA_MAECHEN_MAX_GAP_SECONDS=999999 \
    SPIRA_MAECHEN_LANDING_INTERVAL=999 \
    bash "$TRIGSH" 2>&1)"; e2e_rc1=$?
is   "e2e: trigger run 1 exits 0"           0                      "$e2e_rc1"
want "e2e: trigger log mentions rows"        "invalid-closed trigger:" "$e2e_trig1"

e2e_sweep_id="$("$SPIRA_BD" -C "$SPIRA_DB" list \
    --status open --label "$E2E_SCOPE,$E2E_MAE" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d[0]["id"] if d else "")' 2>/dev/null \
    || echo "")"
[ -n "$e2e_sweep_id" ] || bad "e2e: no sweep bead filed" ""

e2e_desc="$("$SPIRA_BD" -C "$SPIRA_DB" show "$e2e_sweep_id" 2>/dev/null || echo "")"
want "e2e: sweep bead has bead A row" "sp-cr-e2e-a" "$e2e_desc"
want "e2e: sweep bead has bead B row" "sp-cr-e2e-b" "$e2e_desc"
want "e2e: sweep bead has bead C row" "sp-cr-e2e-c" "$e2e_desc"

# Step 2: second trigger run → dedup.
e2e_trig2="$(env -i HOME="$T" \
    PATH="${TESTDB_BIN:+$TESTDB_BIN:}$HERE:/usr/bin:/bin" \
    SPIRA_CONF="$NONE" \
    SPIRA_BD="$_e2e_bd" \
    SPIRA_DB="$SPIRA_DB" \
    SPIRA_HOME="$HERE" \
    SPIRA_RUN="$RUNDIR" \
    SPIRA_REPO="$TESTREPO" \
    SPIRA_REPO_MAP="$SELFMAP" \
    SPIRA_MAECHEN_LABEL="$E2E_MAE" \
    SPIRA_SCOPE_LABEL="$E2E_SCOPE" \
    SPIRA_MAECHEN_MAX_GAP_SECONDS=999999 \
    SPIRA_MAECHEN_LANDING_INTERVAL=999 \
    bash "$TRIGSH" 2>&1)"
want "e2e: second trigger run logs skipping" "skipping" "$e2e_trig2"
e2e_bead_count="$("$SPIRA_BD" -C "$SPIRA_DB" list \
    --status open --label "$E2E_SCOPE,$E2E_MAE" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d))' 2>/dev/null || echo 0)"
is "e2e: second trigger files nothing new" "1" "${e2e_bead_count:-0}"

# Step 3: simulate Maechen pass decisions.
# A → reopen (admission).
"$SPIRA_BD" -C "$SPIRA_DB" reopen "sp-cr-e2e-a" 2>/dev/null || true
# B → allowlist (quotation).
printf 'sp-cr-e2e-b quotation: reason names the flag list, not a remainder\n' > "$ALLOW_FILE"
# C → file follow-up bead, then add bead C to the allowlist citing it (clears UNFILED-FOLLOW).
FOLLOW_ID="$("$SPIRA_BD" -C "$SPIRA_DB" create "follow-up: schema migration for e2e-c" \
    --type task --label "$E2E_SCOPE,plan" --priority 3 2>/dev/null \
    | grep -oE 'sp-[a-z0-9]+')" || FOLLOW_ID="sp-fake99"
printf 'sp-cr-e2e-c follow-up filed as %s\n' "$FOLLOW_ID" >> "$ALLOW_FILE"

# Step 4: detect_invalid_closed via cockpit seam → 0 counted, 1 allowed.
e2e_final="$(env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
    SPIRA_PATH="${SPIRA_PATH:-}" \
    SPIRA_CONF="$T/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$T" \
    SPIRA_RUN="$RUNDIR" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO_MAP="$SELFMAP" SPIRA_GOAL=sp-goal \
    SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=awaiting-ci \
    SPIRA_SPIKE_LABEL=spike \
    SPIRA_SCOPE_LABEL="$E2E_SCOPE" \
    bash "$HERE/cockpit.sh" livelock 2>/dev/null)"

is "e2e: SP_INVALID_CLOSED=0 after pass" "0" \
   "$(printf '%s\n' "$e2e_final" | sed -n 's/^SP_INVALID_CLOSED=//p' | head -1)"
is "e2e: SP_UNFILED_FOLLOW=0 after pass" "0" \
   "$(printf '%s\n' "$e2e_final" | sed -n 's/^SP_UNFILED_FOLLOW=//p' | head -1)"
# Both B (quotation) and C (follow-up filed) are in the allowlist → two ALLOWED-IC lines.
want   "e2e: ALLOWED-IC present"           "ALLOWED-IC"         "$e2e_final"
want   "e2e: ALLOWED-IC names bead B"      "ALLOWED-IC sp-cr-e2e-b" "$e2e_final"
want   "e2e: ALLOWED-IC names bead C"      "ALLOWED-IC sp-cr-e2e-c" "$e2e_final"
nowant "e2e: bead A not in ALLOWED-IC"     "ALLOWED-IC sp-cr-e2e-a" "$e2e_final"

rm -f "$ALLOW_FILE"
testdb_drop

echo
tl_summary
