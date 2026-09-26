#!/usr/bin/env bash
#
# test-model-switch-report.sh — model-switch-report.sh gives the four figures correctly
#
#   ./test-model-switch-report.sh
#
# WHAT IS UNDER TEST. model-switch-report.sh compares a before/after window to measure the
# cost and quality impact of a model change. Four figures: beads landed per window, $ per
# landed bead, attempts per landed bead (retry proxy), and SOP yield for Ops. The hard
# parts are that cost data may be missing (renders ?, never 0), and that the before window
# may have no cost data at all (pre-telemetry).
#
# THREE BEHAVIORS ASSERTED POSITIVELY:
#   - correct figures when data is present
#   - ? (not 0) for a missing cost field
#   - SOP yield calculated per-run, correctly matching SOP apps to their Ops session
#
# Driven entirely from fixture files in a temp dir; no database or network.
#
# defect: sp-59ej
# tier: T1
# covers: spira/model-switch-report.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

SPIRA_RUN_DIR="$TMP/run"; mkdir -p "$SPIRA_RUN_DIR/sop"
SOP_LEDGER="$TMP/run/sop/applied.jsonl"

# TIME CONSTANTS. The report compares [CHANGE, UNTIL) against [CHANGE-W, CHANGE).
# We fix all three to known integer epochs so the window is exactly 2h each side.
CHANGE=1000000                     # model-change boundary (arbitrary fixture epoch)
UNTIL=$(( CHANGE + 7200 ))         # end of after-window (2h after change)
BEFORE_START=$(( CHANGE - 7200 )) # start of before-window (2h before change)

# Timestamps within each window
ts1_after=$(( CHANGE + 1800 ))    # 30 min after change
ts2_after=$(( CHANGE + 3600 ))    # 60 min after change
ts3_after=$(( CHANGE + 5400 ))    # 90 min after change
ts1_before=$(( CHANGE - 1800 ))   # 30 min before change
ts2_before=$(( CHANGE - 3600 ))   # 60 min before change

ts_iso() { date -u -d "@$1" +%Y-%m-%dT%H:%M:%SZ; }

# run_report — run against the fixture files, controlling the exact window
run_report() {
    SPIRA_RUN="$SPIRA_RUN_DIR" \
    SPIRA_SOP_LEDGER="$SOP_LEDGER" \
    SPIRA_CONF=/dev/null \
    bash "$HERE/model-switch-report.sh" \
        --since "$CHANGE" --until "$UNTIL" \
        "$@" 2>/dev/null
}

write_ledger()  { printf '%s\n' "$@" > "$SPIRA_RUN_DIR/aeon-ledger.log"; }
write_landing() { printf '%s\n' "$@" > "$SPIRA_RUN_DIR/landing.log"; }
reset_sop()     { : > "$SOP_LEDGER"; }
append_sop()    { printf '%s\n' "$@" >> "$SOP_LEDGER"; }

# -------------------------------------------------------------------------
echo
echo "1. BEADS LANDED — correct count from landing.log"
# Three beads land in the after window, two before, one outside either window.
write_ledger ""
write_landing \
    "$(ts_iso $ts1_after) spira: landed spira/sp-a1" \
    "$(ts_iso $ts2_after) spira: landed spira/sp-a2" \
    "$(ts_iso $ts3_after) spira: landed spira/sp-a3" \
    "$(ts_iso $ts1_before) spira: landed spira/sp-b1" \
    "$(ts_iso $ts2_before) spira: landed spira/sp-b2" \
    "$(ts_iso $(( CHANGE - 100000 ))) spira: landed spira/sp-old"
reset_sop
out="$(run_report)"
want "three beads landed after"  "   3" "$(printf '%s\n' "$out" | grep 'after change:' | head -1)"
want "two beads landed before"   "   2" "$(printf '%s\n' "$out" | grep 'before change:' | head -1)"
want "rate label /h appears"     "/h"   "$(printf '%s\n' "$out" | grep 'after change:' | head -1)"

# -------------------------------------------------------------------------
echo
echo "2. \$ PER LANDED BEAD — correct mean, and ? never 0 for missing cost"
# Two beads after change: sp-a1 costs $2.00 (one session), sp-a2 costs $3.00 (one session).
# Mean should be $2.50. sp-b1 before change has no cost data → before window cost must be ?.
write_ledger \
    "$(ts_iso $ts1_after) done builder sp-a1 rc=0 status=closed wall_s=100 api_s=90 turns=10 in_tok=5 cache_read_tok=50000 out_tok=1000 think_tok=200 cost_usd=2.0000" \
    "$(ts_iso $ts2_after) done builder sp-a2 rc=0 status=closed wall_s=120 api_s=100 turns=15 in_tok=6 cache_read_tok=60000 out_tok=1200 think_tok=300 cost_usd=3.0000" \
    "$(ts_iso $ts1_before) done builder sp-b1 rc=0 status=closed wall_s=? api_s=? turns=? in_tok=? cache_read_tok=? out_tok=? think_tok=? cost_usd=?"
write_landing \
    "$(ts_iso $ts1_after) spira: landed spira/sp-a1" \
    "$(ts_iso $ts2_after) spira: landed spira/sp-a2" \
    "$(ts_iso $ts1_before) spira: landed spira/sp-b1"
reset_sop
out="$(run_report)"
# Section 2 rows: "after change:" and "before change:" appear multiple times — take by line
after_cost_line="$(printf '%s\n' "$out" | grep '2\. \$' -A 2 | grep 'after')"
before_cost_line="$(printf '%s\n' "$out" | grep '2\. \$' -A 3 | grep 'before')"
want "after cost mean is 2.50"  "2.50" "$after_cost_line"
want "n=2 beads with cost data" "n=2"  "$after_cost_line"
want "before cost is ?"         "\$?"  "$before_cost_line"
nowant "before cost not 0"      "\$0"  "$before_cost_line"
want "n=0 before"               "n=0"  "$before_cost_line"

# -------------------------------------------------------------------------
echo
echo "3. ATTEMPTS PER LANDED BEAD — retries counted correctly"
# sp-a1: 2 sessions (one retry) → 2 attempts
# sp-a2: 1 session → 1 attempt
# sp-a3: no ledger entry → excluded from mean
# Mean of [2, 1] = 1.50; one of two beads with data needed >1 attempt.
ts_retry=$(( CHANGE + 900 ))
write_ledger \
    "$(ts_iso $ts1_after) done builder sp-a1 rc=1 status=? wall_s=50 api_s=45 turns=5 in_tok=3 cache_read_tok=30000 out_tok=500 think_tok=100 cost_usd=1.0000" \
    "$(ts_iso $ts_retry) done builder sp-a1 rc=0 status=closed wall_s=100 api_s=90 turns=10 in_tok=5 cache_read_tok=50000 out_tok=1000 think_tok=200 cost_usd=2.0000" \
    "$(ts_iso $ts2_after) done builder sp-a2 rc=0 status=closed wall_s=120 api_s=100 turns=15 in_tok=6 cache_read_tok=60000 out_tok=1200 think_tok=300 cost_usd=3.0000"
write_landing \
    "$(ts_iso $ts_retry) spira: landed spira/sp-a1" \
    "$(ts_iso $ts2_after) spira: landed spira/sp-a2" \
    "$(ts_iso $ts3_after) spira: landed spira/sp-a3"
reset_sop
out="$(run_report)"
att_line="$(printf '%s\n' "$out" | grep '3\. ATTEMPTS' -A 2 | grep 'after')"
want "mean attempts is 1.50"    "1.50" "$att_line"
want "one of two beads retried" "1/2"  "$att_line"

# -------------------------------------------------------------------------
echo
echo "4. SOP YIELD — per-run coverage and effectiveness"
# Two Ops sessions in the after window:
#   session sp-op1: awake at CHANGE+100, done at CHANGE+500
#     one SOP applied: check=pass held=yes → covered, effective
#   session sp-op2: awake at CHANGE+600, done at CHANGE+900
#     no SOP applied → not covered
# Coverage = 1/2 = 0.50; effectiveness = 1/1 = 1.00
ts_op1_a=$(( CHANGE + 100 )); ts_op1_d=$(( CHANGE + 500 ))
ts_op2_a=$(( CHANGE + 600 )); ts_op2_d=$(( CHANGE + 900 ))
ts_sop=$(( CHANGE + 300 ))

write_ledger \
    "$(ts_iso $ts_op1_a) awake ops sp-op1" \
    "$(ts_iso $ts_op1_d) done ops sp-op1 rc=0 status=closed wall_s=400 api_s=380 turns=30 in_tok=25 cache_read_tok=1000000 out_tok=5000 think_tok=2000 cost_usd=0.5000" \
    "$(ts_iso $ts_op2_a) awake ops sp-op2" \
    "$(ts_iso $ts_op2_d) done ops sp-op2 rc=0 status=closed wall_s=300 api_s=280 turns=20 in_tok=20 cache_read_tok=800000 out_tok=4000 think_tok=1500 cost_usd=0.4000"
write_landing ""
reset_sop
append_sop \
    "{\"ts\":\"$(ts_iso $ts_sop)\",\"epoch\":$ts_sop,\"sop\":\"sop-sweep\",\"bead\":\"sp-op1\",\"check\":\"pass\",\"held\":\"yes\",\"actor\":\"overseer\",\"shelf\":\"ok\",\"note\":\"ok\",\"why\":\"healthy\"}"
out="$(run_report)"
sop_sec="$(printf '%s\n' "$out" | grep '4\. SOP' -A 8)"
want "two Ops sessions after"   "2 Ops sessions" "$sop_sec"
want "one applied a SOP"        "1 applied a SOP" "$sop_sec"
want "coverage is 0.50"         "0.50" "$(printf '%s\n' "$sop_sec" | grep 'coverage:' | head -1)"
want "effectiveness is 1.00"    "1.00" "$(printf '%s\n' "$sop_sec" | grep 'effectiveness:' | head -1)"

# -------------------------------------------------------------------------
echo
echo "5. MISSING DATA renders ? never 0"
# A bead with one session carrying all ? cost fields must not contribute 0 to the mean.
write_ledger \
    "$(ts_iso $ts1_after) done builder sp-q1 rc=0 status=closed wall_s=? api_s=? turns=? in_tok=? cache_read_tok=? out_tok=? think_tok=? cost_usd=?"
write_landing \
    "$(ts_iso $ts1_after) spira: landed spira/sp-q1"
reset_sop
out="$(run_report)"
cost_q="$(printf '%s\n' "$out" | grep '2\. \$' -A 2 | grep 'after')"
want "all-? session renders ?"  "\$?" "$cost_q"
nowant "not reported as 0"      "\$0" "$cost_q"

# -------------------------------------------------------------------------
echo
tl_summary
