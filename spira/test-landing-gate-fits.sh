#!/usr/bin/env bash
#
# test-landing-gate-fits.sh — gate_fits and gate_lock_wait, the pass-budget arithmetic
#   landing.sh's queue-mode certification loop calls before every gate.sh run.
#
# WHAT THIS REPLACES. test-landing-gate-wait.sh drove this same arithmetic through 4 real
# landing passes (repo, testdb, worktrees). gate_fits and gate_lock_wait take every input
# as an optional parameter and live above landing.sh's source guard for exactly this: a T1
# suite sources the file and calls them directly, with no live pass, no repo, no gate.sh.
#
# tier: T1
# covers: spira/landing.sh UC-landing-merge-queue-05 UC-landing-merge-queue-09
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

. "$HERE/conf.sh"
. "$HERE/lib.sh"
# shellcheck disable=SC1091
. "$HERE/landing.sh"

echo "test-landing-gate-fits.sh"

# ── gate_fits: 0 (no limit) means unbounded, so an operator's hand-run pass is never told
#    the budget is spent by a limit that isn't actually being enforced ─────────────────────
wantrc "maxsec=0: always fits (no RuntimeMaxSec set)" 0 "$(gate_fits 0 0 1200; echo $?)"

now="$(date +%s)"
wantrc "plenty of budget left: fits"    0 "$(gate_fits 3600 $(( now - 100 )) 1200; echo $?)"
wantrc "less than reserve left: refuses (positive control)" \
    1 "$(gate_fits 3600 $(( now - 3000 )) 1200; echo $?)"
wantrc "exactly at reserve: fits (>= reserve, not >)" \
    0 "$(gate_fits 1300 $(( now - 100 )) 1200; echo $?)"

# ── gate_lock_wait: 2x timeout, unless an explicit wait or the remaining budget caps it ───
gate_lock_wait 0 0 2700 ""
is "no explicit wait, no maxsec: 2x gate timeout" "5400" "$_gate_wait"

gate_lock_wait 0 0 2700 "900"
is "explicit wait wins outright" "900" "$_gate_wait"

gate_lock_wait 20000 "$(( now - 100 ))" 2700 ""
is "plenty of budget: still 2x timeout, uncapped" "5400" "$_gate_wait"

gate_lock_wait 3600 "$(( now - 3000 ))" 2700 ""
remaining=$(( 3600 - 3000 ))
is "short remaining budget caps the wait below 2x timeout" "$remaining" "$_gate_wait"

tl_summary
