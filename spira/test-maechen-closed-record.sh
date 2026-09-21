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
# check is live: allowlist has no effect when the file is absent (positive);
# trigger fires when rows exist (positive); trigger skips when no rows (negative).
#
# covers: spira/maechen-trigger.sh spira/lib.sh spira/cockpit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-maechen-closed-record
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1" ;; esac; }

TRIGSH="$HERE/maechen-trigger.sh"
T="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$T"' EXIT INT TERM
NONE="$T/none.conf"

# ---------------------------------------------------------------------------
# THROWAWAY GIT REPO — required by the trigger's lane check and landing count.
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

echo "test-maechen-closed-record.sh"

# ==========================================================================================
echo
echo "ALLOWLIST: id in allowlist → ALLOWED-IC, not counted"
# ==========================================================================================
# Build a fixture database with one INVALID-CLOSED bead and one allowlisted bead.
testdb_up maechen_closed_record_allow || { bad "allowlist fixture: testdb_up failed" ""; }

IC_SCOPE="sptest-cr-allow-scope"
# Plant an INVALID-CLOSED bead (close reason hits RED_FLAGS).
IC_ID="$("$SPIRA_BD" -C "$SPIRA_DB" create "admitting workaround" \
    --type task --label "$IC_SCOPE,plan" --priority 3 2>/dev/null \
    | grep -oE 'sp-[a-z0-9]+')"
[ -n "$IC_ID" ] || { bad "allowlist: could not create test bead" ""; }
BD_UPDATE_CLOSED_OVERRIDE=1 "$SPIRA_BD" -C "$SPIRA_DB" update "$IC_ID" \
    --status closed 2>/dev/null || true
# close_reason is set via bd close; use update on close_reason directly since the guard
# only blocks --status closed, not close_reason updates.
"$SPIRA_BD" -C "$SPIRA_DB" update "$IC_ID" \
    --close-reason "Applied a temporary workaround — real fix needed." 2>/dev/null || true

# Without allowlist: expect INVALID-CLOSED row.
_lc_run() {
    env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
        SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$T/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$T" \
        SPIRA_RUN="$RUNDIR" SPIRA_DB="$SPIRA_DB" \
        SPIRA_REPO_MAP="$SELFMAP" SPIRA_GOAL=sp-goal \
        SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=awaiting-ci \
        SPIRA_SPIKE_LABEL=spike \
        SPIRA_SCOPE_LABEL="$IC_SCOPE" \
        bash "$HERE/cockpit.sh" livelock 2>/dev/null
}

out="$(_lc_run)"
want  "no allowlist: INVALID-CLOSED row present"  "INVALID-CLOSED" "$out"
want  "no allowlist: bead id in row"              "$IC_ID"         "$out"
is "no allowlist: SP_INVALID_CLOSED=1" "1" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_INVALID_CLOSED=//p' | head -1)"

# Add to allowlist — bead should now appear as ALLOWED-IC, not counted.
printf '%s quotation: reason names the flag list, not a remainder\n' "$IC_ID" > "$ALLOW_FILE"

out="$(_lc_run)"
nowant "allowlisted: no INVALID-CLOSED row"   "INVALID-CLOSED"  "$out"
want   "allowlisted: ALLOWED-IC row present"  "ALLOWED-IC"      "$out"
want   "allowlisted: bead id in allowed row"  "$IC_ID"          "$out"
is "allowlisted: SP_INVALID_CLOSED=0" "0" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_INVALID_CLOSED=//p' | head -1)"

rm -f "$ALLOW_FILE"
testdb_drop

# ==========================================================================================
echo
echo "ALLOWLIST: UNFILED-FOLLOW id in allowlist → ALLOWED-IC, not counted"
# ==========================================================================================
testdb_up maechen_closed_record_uf_allow || { bad "uf-allowlist fixture: testdb_up failed" ""; }

UF_SCOPE="sptest-cr-uf-scope"
UF_ID="$("$SPIRA_BD" -C "$SPIRA_DB" create "unfiled follow bead" \
    --type task --label "$UF_SCOPE,plan" --priority 3 2>/dev/null \
    | grep -oE 'sp-[a-z0-9]+')"
[ -n "$UF_ID" ] || { bad "uf-allowlist: could not create test bead" ""; }
BD_UPDATE_CLOSED_OVERRIDE=1 "$SPIRA_BD" -C "$SPIRA_DB" update "$UF_ID" \
    --status closed 2>/dev/null || true
"$SPIRA_BD" -C "$SPIRA_DB" update "$UF_ID" \
    --close-reason "The real fix is a proper guard. Deployed the stopgap." 2>/dev/null || true

_lc_run_uf() {
    env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
        SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$T/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$T" \
        SPIRA_RUN="$RUNDIR" SPIRA_DB="$SPIRA_DB" \
        SPIRA_REPO_MAP="$SELFMAP" SPIRA_GOAL=sp-goal \
        SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=awaiting-ci \
        SPIRA_SPIKE_LABEL=spike \
        SPIRA_SCOPE_LABEL="$UF_SCOPE" \
        bash "$HERE/cockpit.sh" livelock 2>/dev/null
}

out="$(_lc_run_uf)"
want "no allowlist: UNFILED-FOLLOW row present" "UNFILED-FOLLOW" "$out"

printf '%s follow-up filed as sp-fake1\n' "$UF_ID" > "$ALLOW_FILE"
out="$(_lc_run_uf)"
nowant "allowlisted: no UNFILED-FOLLOW row"  "UNFILED-FOLLOW" "$out"
want   "allowlisted: ALLOWED-IC row present" "ALLOWED-IC"     "$out"
is "uf-allowlisted: SP_UNFILED_FOLLOW=0" "0" \
   "$(printf '%s\n' "$out" | sed -n 's/^SP_UNFILED_FOLLOW=//p' | head -1)"

rm -f "$ALLOW_FILE"
testdb_drop

# ==========================================================================================
echo
echo "TRIGGER: INVALID-CLOSED rows fire the third trigger condition"
# ==========================================================================================
# Use a real fixture database so the --status closed filter is exercised correctly
# (law-prefer-the-real-dependency). The stub bd in other trigger tests returns
# BD_LIST_OUTPUT for all list calls regardless of --status.
testdb_up maechen_closed_record_trigger || { bad "trigger fixture: testdb_up failed" ""; }

TR_SCOPE="sptest-cr-trig-scope"
TR_MAE="sptest-cr-mae"
TR_ID="$("$SPIRA_BD" -C "$SPIRA_DB" create "admitting close bead" \
    --type task --label "$TR_SCOPE,plan" --priority 3 2>/dev/null \
    | grep -oE 'sp-[a-z0-9]+')"
[ -n "$TR_ID" ] || { bad "trigger: could not create test bead" ""; }
BD_UPDATE_CLOSED_OVERRIDE=1 "$SPIRA_BD" -C "$SPIRA_DB" update "$TR_ID" \
    --status closed 2>/dev/null || true
"$SPIRA_BD" -C "$SPIRA_DB" update "$TR_ID" \
    --close-reason "Applied a temporary workaround, real fix still needed." 2>/dev/null || true

# Set lastpass=now, watermark=now so neither time nor landing trigger fires;
# only the INVALID-CLOSED trigger should fire.
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

is   "invalid-closed trigger: exits 0"         0 "$tr_rc"
want "invalid-closed trigger: bead created"    "invalid-closed" "$tr_out"
want "invalid-closed trigger: log mentions row" "invalid-closed trigger:" "$tr_out"

# Verify bead was filed with the closed-record rows in description.
tr_bead="$("$SPIRA_BD" -C "$SPIRA_DB" list \
    --status open --label "$TR_SCOPE,$TR_MAE" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d[0]["id"] if d else "")' 2>/dev/null \
    || echo "")"
is "invalid-closed trigger: bead open" "1" "$([ -n "$tr_bead" ] && echo 1 || echo 0)"
tr_desc="$("$SPIRA_BD" -C "$SPIRA_DB" show "$tr_bead" 2>/dev/null || echo "")"
want "trigger bead description has INVALID-CLOSED row" "INVALID-CLOSED" "$tr_desc"
want "trigger bead description has the bead id"        "$TR_ID"         "$tr_desc"

# ==========================================================================================
echo
echo "TRIGGER: second run files nothing (dedup)"
# ==========================================================================================
# The trigger bead from the previous run is still open — dedup must prevent another.
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
is     "dedup second run: exits 0"    0          "$tr_rc2"
want   "dedup second run: logs skip"  "skipping" "$tr_out2"
tr_count="$("$SPIRA_BD" -C "$SPIRA_DB" list \
    --status open --label "$TR_SCOPE,$TR_MAE" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d))' 2>/dev/null || echo 0)"
is "dedup second run: still one bead" "1" "${tr_count:-0}"

testdb_drop

# ==========================================================================================
echo
echo "TRIGGER: no fire when all rows are in the allowlist"
# ==========================================================================================
# When detect_invalid_closed finds only ALLOWED-IC rows (all ids in the allowlist),
# the invalid_closed_trigger must NOT fire. Set time and landing triggers to never fire.
testdb_up maechen_closed_record_allow_no_trigger || { bad "allow-notrigger fixture: testdb_up failed" ""; }

ANT_SCOPE="sptest-cr-ant-scope"
ANT_MAE="sptest-cr-ant-mae"
ANT_ID="$("$SPIRA_BD" -C "$SPIRA_DB" create "allowlisted closed bead" \
    --type task --label "$ANT_SCOPE,plan" --priority 3 2>/dev/null \
    | grep -oE 'sp-[a-z0-9]+')"
[ -n "$ANT_ID" ] || { bad "allow-notrigger: could not create test bead" ""; }
BD_UPDATE_CLOSED_OVERRIDE=1 "$SPIRA_BD" -C "$SPIRA_DB" update "$ANT_ID" \
    --status closed 2>/dev/null || true
"$SPIRA_BD" -C "$SPIRA_DB" update "$ANT_ID" \
    --close-reason "Applied a temporary workaround — but just for the test." 2>/dev/null || true

# Add to allowlist so detect_invalid_closed sees only ALLOWED-IC.
printf '%d\n' "$now_ts" > "$WATERMARK_FILE"
printf '%d\n' "$now_ts" > "$LASTPASS_FILE"
printf '%s quotation: this is a test bead\n' "$ANT_ID" > "$ALLOW_FILE"

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
is     "allowlisted rows: trigger exits 0"        0           "$ant_rc"
want   "allowlisted rows: logs no trigger"        "no trigger" "$ant_out"
nowant "allowlisted rows: no bead created"        "invalid-closed trigger:" "$ant_out"
ant_count="$("$SPIRA_BD" -C "$SPIRA_DB" list \
    --status open --label "$ANT_SCOPE,$ANT_MAE" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d))' 2>/dev/null || echo 0)"
is "allowlisted rows: no trigger bead filed" "0" "${ant_count:-0}"

rm -f "$ALLOW_FILE"
testdb_drop

# ==========================================================================================
echo
echo "END-TO-END: three closed beads, trigger, simulated pass, zero counted rows after"
# ==========================================================================================
# Three closed beads in a fixture:
#   A — INVALID-CLOSED (admission): temporary workaround admitted
#   B — INVALID-CLOSED (quotation): reason discusses the flag list, not a remainder
#   C — UNFILED-FOLLOW: "the real fix" without a tracking reference
#
# Sequence:
#   1. Trigger fires → one maechen bead carrying all three rows
#   2. Second trigger run → dedup (one open bead already)
#   3. Simulated pass actions:
#       A → reopen
#       B → allowlist
#       C → update close_reason with new bead id reference
#   4. detect_invalid_closed → 0 counted rows, 1 ALLOWED-IC line

testdb_up maechen_closed_record_e2e || { bad "e2e fixture: testdb_up failed" ""; }

E2E_SCOPE="sptest-cr-e2e-scope"
E2E_MAE="sptest-cr-e2e-mae"

# Bead A — admission.
BEAD_A="$("$SPIRA_BD" -C "$SPIRA_DB" create "bead A admission" \
    --type task --label "$E2E_SCOPE,plan" --priority 3 2>/dev/null \
    | grep -oE 'sp-[a-z0-9]+')"
BD_UPDATE_CLOSED_OVERRIDE=1 "$SPIRA_BD" -C "$SPIRA_DB" update "$BEAD_A" \
    --status closed 2>/dev/null || true
"$SPIRA_BD" -C "$SPIRA_DB" update "$BEAD_A" \
    --close-reason "Applied a temporary workaround pending the real fix." 2>/dev/null || true

# Bead B — quotation (reason discusses the flag list, not a remainder).
BEAD_B="$("$SPIRA_BD" -C "$SPIRA_DB" create "bead B quotation" \
    --type task --label "$E2E_SCOPE,plan" --priority 3 2>/dev/null \
    | grep -oE 'sp-[a-z0-9]+')"
BD_UPDATE_CLOSED_OVERRIDE=1 "$SPIRA_BD" -C "$SPIRA_DB" update "$BEAD_B" \
    --status closed 2>/dev/null || true
"$SPIRA_BD" -C "$SPIRA_DB" update "$BEAD_B" \
    --close-reason "Built close-reason-flags.py which adds 'TEMPORARY WORKAROUND' to RED_FLAGS. Landed on origin/main." 2>/dev/null || true

# Bead C — unfiled follow-on.
BEAD_C="$("$SPIRA_BD" -C "$SPIRA_DB" create "bead C unfiled" \
    --type task --label "$E2E_SCOPE,plan" --priority 3 2>/dev/null \
    | grep -oE 'sp-[a-z0-9]+')"
BD_UPDATE_CLOSED_OVERRIDE=1 "$SPIRA_BD" -C "$SPIRA_DB" update "$BEAD_C" \
    --status closed 2>/dev/null || true
"$SPIRA_BD" -C "$SPIRA_DB" update "$BEAD_C" \
    --close-reason "The real fix requires a schema migration. Deployed the stopgap." 2>/dev/null || true

[ -n "$BEAD_A" ] && [ -n "$BEAD_B" ] && [ -n "$BEAD_C" ] \
    || { bad "e2e: could not create fixture beads" "A=$BEAD_A B=$BEAD_B C=$BEAD_C"; }

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
    bash "$TRIGSH" 2>&1)"
is "e2e: trigger run 1 exits 0" "0" "$?"
want "e2e: trigger log mentions rows" "invalid-closed trigger:" "$e2e_trig1"

e2e_sweep_id="$("$SPIRA_BD" -C "$SPIRA_DB" list \
    --status open --label "$E2E_SCOPE,$E2E_MAE" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d[0]["id"] if d else "")' 2>/dev/null \
    || echo "")"
[ -n "$e2e_sweep_id" ] || { bad "e2e: no sweep bead filed" ""; }

e2e_desc="$("$SPIRA_BD" -C "$SPIRA_DB" show "$e2e_sweep_id" 2>/dev/null || echo "")"
want "e2e: sweep bead has BEAD_A row" "$BEAD_A" "$e2e_desc"
want "e2e: sweep bead has BEAD_B row" "$BEAD_B" "$e2e_desc"
want "e2e: sweep bead has BEAD_C row" "$BEAD_C" "$e2e_desc"

# Step 2: second trigger run → dedup (bead still open).
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
is "e2e: second trigger run files nothing new" "1" "${e2e_bead_count:-0}"

# Step 3: simulate Maechen pass decisions.
# A → reopen (admission).
"$SPIRA_BD" -C "$SPIRA_DB" reopen "$BEAD_A" 2>/dev/null || true

# B → allowlist (quotation).
printf '%s quotation: reason names the flag list, not a remainder\n' "$BEAD_B" > "$ALLOW_FILE"

# C → update close_reason with a tracking reference (follow-up filed as a fake bead id).
"$SPIRA_BD" -C "$SPIRA_DB" update "$BEAD_C" \
    --close-reason "The real fix requires a schema migration. Deployed the stopgap. Follow-up filed as sp-fake99." 2>/dev/null || true

# Step 4: detect_invalid_closed via cockpit seam → 0 counted, 1 allowed.
_e2e_lc() {
    env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
        SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$T/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$T" \
        SPIRA_RUN="$RUNDIR" SPIRA_DB="$SPIRA_DB" \
        SPIRA_REPO_MAP="$SELFMAP" SPIRA_GOAL=sp-goal \
        SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=awaiting-ci \
        SPIRA_SPIKE_LABEL=spike \
        SPIRA_SCOPE_LABEL="$E2E_SCOPE" \
        bash "$HERE/cockpit.sh" livelock 2>/dev/null
}

e2e_final="$(_e2e_lc)"
is "e2e: SP_INVALID_CLOSED=0 after pass" "0" \
   "$(printf '%s\n' "$e2e_final" | sed -n 's/^SP_INVALID_CLOSED=//p' | head -1)"
is "e2e: SP_UNFILED_FOLLOW=0 after pass" "0" \
   "$(printf '%s\n' "$e2e_final" | sed -n 's/^SP_UNFILED_FOLLOW=//p' | head -1)"
want   "e2e: one ALLOWED-IC line present" "ALLOWED-IC" "$e2e_final"
want   "e2e: ALLOWED-IC names BEAD_B"    "$BEAD_B"    "$e2e_final"
nowant "e2e: BEAD_A not in ALLOWED-IC"   "ALLOWED-IC $BEAD_A" "$e2e_final"
nowant "e2e: BEAD_C not in ALLOWED-IC"   "ALLOWED-IC $BEAD_C" "$e2e_final"

rm -f "$ALLOW_FILE"
testdb_drop

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
