#!/usr/bin/env bash
#
# test-check8-progressed.sh — check8_should_judge: CHECK 8 fires only when nothing
#   progressed and the plan is starved, never merely because the pass acted.
#
#   ./test-check8-progressed.sh
#
# WHY THIS EXISTS. Before sp-acted-conflation, sentinel.sh kept a single `acted` counter
# and gated CHECK 8 (judgement/inference) on `acted == 0`. Any false positive from any
# earlier check — the reclaim probe matching its own idle message, the Sending failing to
# delete a locked branch — incremented `acted` and silenced the one check that notices
# the system is stuck. The fix gates on `progressed == 0` instead, never on `acted == 0`.
#
# DEMOTED FROM T3 TO T1 (dispatch.md G15, sp-9ce60.5): this suite used to build a Dolt
# fixture and run two full sentinel.sh passes to exercise these two cases (10s). CHECK 8's
# whole decision is now check8_should_judge(plan_ready, plan_inprog, n_open, progressed,
# last, now, every), a pure predicate over sentinel.sh's own state variables, extracted
# from sentinel.sh l.1379-1400 into lib.sh — a table over it needs no database at all. The
# real-pass coverage for CHECK 7 (which this suite's stubs also incidentally exercised)
# lives in test-sentinel-pass.sh.
#
# G15 GAPS CLOSED HERE: the cooldown case and the plan_ready>0 exits-before-judgement case
# had no test before this extraction — only the acted/progressed distinction did.
#
# defect: sp-acted-conflation
# tier: T1
# covers: spira/sentinel.sh spira/lib.sh UC-dispatch-18
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# Isolated per call: check8_should_judge takes every input as an argument and touches no
# ambient state, but lib.sh itself must still be sourced somewhere clean
# (law-gates-run-in-a-clean-environment) rather than into this suite's own shell.
libcall() {
    env -i PATH="$PATH" HOME="$TMP" SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        bash -c '. "'"$HERE"'/lib.sh" >/dev/null 2>&1 || exit 1
'"$1"
}
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-check8-progressed.sh"

# ======================================================================================
echo
echo "case 1 (POSITIVE CONTROL) — starved, nothing progressed: CHECK 8 fires:"
# ======================================================================================
# plan_ready=0, plan_inprog=0, n_open=1, progressed=0, way outside cooldown.
is "acted-without-progress fires judgement (yes)" \
   "yes" "$(libcall 'check8_should_judge 0 0 1 0 0 100000 3600')"

# ======================================================================================
echo
echo "case 2 — progressed > 0: CHECK 8 does NOT fire even though the plan is starved:"
# ======================================================================================
# Same starved shape as case 1, but progressed=1. acted is not an input at all — this is
# the regression case sp-acted-conflation fixed: acted>0 alone must never suppress it,
# and only progressed does.
is "progress suppresses judgement (no)" \
   "no" "$(libcall 'check8_should_judge 0 0 1 1 0 100000 3600')"

# ======================================================================================
echo
echo "case 3 (G15) — starved, nothing progressed, but within cooldown of the last fire:"
# ======================================================================================
# last=99000, now=100000, every=3600: only 1000s elapsed since the last judgement.
is "within cooldown: judgement withheld (cooldown)" \
   "cooldown" "$(libcall 'check8_should_judge 0 0 1 0 99000 100000 3600')"
# POSITIVE CONTROL: the same gap once cooldown has elapsed fires again.
is "cooldown elapsed: judgement fires again (yes)" \
   "yes" "$(libcall 'check8_should_judge 0 0 1 0 90000 100000 3600')"

# ======================================================================================
echo
echo "case 4 (G15) — plan_ready > 0 exits before judgement, even with a ready incident:"
# ======================================================================================
# plan_ready>0 means the DAG is moving whether or not an aeon was free to take it, so it
# must exit "no" before the starvation predicate is even considered — n_open, plan_inprog
# and progressed here are set exactly as case 1's firing shape to prove plan_ready is what
# stops it, not some other input.
is "plan_ready>0 exits before judgement (no)" \
   "no" "$(libcall 'check8_should_judge 1 0 1 0 0 100000 3600')"
# Ready INCIDENT work (a non-plan partition) must not suppress judgement on its own —
# check8_should_judge only ever sees plan_ready, so an incident-partition count never
# reaches it; this row documents that plan_ready is plan-only by construction.
is "plan_ready=0 (incident-only readiness is invisible here) still fires (yes)" \
   "yes" "$(libcall 'check8_should_judge 0 0 1 0 0 100000 3600')"

# ======================================================================================
echo
echo "boundary — plan_inprog > 0 or n_open == 0 also withhold judgement:"
# ======================================================================================
is "plan_inprog>0: no judgement" "no" "$(libcall 'check8_should_judge 0 1 1 0 0 100000 3600')"
is "n_open==0: no judgement"     "no" "$(libcall 'check8_should_judge 0 0 0 0 0 100000 3600')"

tl_summary
