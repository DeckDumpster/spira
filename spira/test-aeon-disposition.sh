#!/usr/bin/env bash
#
# test-aeon-disposition.sh — aeon_disposition (lib.sh), the teardown-decision seam pulled
# out of aeon.sh cleanup() (sp-eq8a4.2.1). Precedence table over the 13 open-bead teardown
# branches (UC-aeon-execution-11) plus bead_has_label, the read-after-claim poison check
# (UC-aeon-execution-03). Both are pure: no worktree, no database, no systemd.
#
# FAIL-CLOSED FIRST (per docs/test-plan/aeon-execution.md §6): the branches that were
# undertested gaps (G1 lapsed, G3 timeout, G4 capacity, G5 slain, G6 gate-unfinished,
# G7 poison-race, G15 unjudged-<cause>) are asserted before the rest of the table, and the
# charging branches (lapsed, thrash-charged, pre-session, yield-headless, unlanded) are
# asserted before the free ones — a silently-always-free implementation fails loudest here.
# G8 (precedence) is the second half of the table: every adjacent pair of branches is given
# BOTH markers at once and must resolve to the higher one, never a coincidence of test order.
#
# tier: T1
# covers: spira/lib.sh spira/aeon.sh UC-aeon-execution-11 UC-aeon-execution-03
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
# Non-default, explicit runtime root: lib.sh writes here on source (mkdir -p), and this
# must never be the real installed $SPIRA_RUN (law-probe-a-fixture-not-production).
export SPIRA_INSTANCE=disptest SPIRA_RUN="$TMP/run"
. "$HERE/lib.sh"

# ---- G7: bead_has_label — read-after-claim poison race ------------------------------

bead_has_label '{"labels":["spira-poison","plan"]}' spira-poison
wantrc "bead_has_label: label present (positive control)" 0 $?

bead_has_label '{"labels":["plan"]}' spira-poison
wantrc "bead_has_label: label absent" 1 $?

bead_has_label '[{"labels":["spira-poison"]}]' spira-poison
wantrc "bead_has_label: list-wrapped bd-show JSON" 0 $?

bead_has_label '' spira-poison
wantrc "bead_has_label: unparseable JSON fails closed (not poisoned, proceeds)" 1 $?

# ---- aeon_disposition — field order --------------------------------------------------
#   status capacity_rc slain thrash thrash_charged lapsed gate_unfinished decision_blocked \
#   session_rc committed requeue_cause operator_wait yield_headless session_started outcome \
#   [submitted]
row() {   # row <case> <expected-4-field-line> <15-16 disposition args...>
    local name="$1" expected="$2"; shift 2
    is "$name" "$expected" "$(aeon_disposition "$@")"
}

# ---- charging branches first (G1, thrash-charged, pre-session, yield-headless, unlanded) --

row "G1 lease lapse charges an attempt" \
    "lapsed charge - lapsed" \
    open 1 no no no yes no no 0 no - no no 1 -

row "thrash streak at cap charges (thrash-stale, not the bare exemption)" \
    "requeue-thrash-charged charge thrash-stale thrash-charged" \
    open 1 no yes yes no no no 0 no - no no 1 -

row "pre-session death charges" \
    "pre-session charge - pre-session" \
    open 1 no no no no no no 0 no - no no 0 -

row "yield-headless charges" \
    "yield-headless charge - yield-headless" \
    open 1 no no no no no no 0 no - no yes 1 -

row "session ran to its own end, bead still open: unlanded charges" \
    "open charge - unlanded" \
    open 1 no no no no no no 0 no - no no 1 unlanded

# ---- free branches (G4, G5, thrash bare, G6, decision-blocked, G3, requeue, operator-wait) --

row "G4 capacity loss mid-session is free" \
    "capacity free - capacity" \
    open 0 no no no no no no 0 no - no no 1 -

row "G5 aeon-side slain is free" \
    "slain free - slain" \
    open 1 yes no no no no no 0 no - no no 1 -

row "thrash below the streak cap is free (bare exemption)" \
    "requeue-thrash free thrash thrash" \
    open 1 no yes no no no no 0 no - no no 1 -

row "G6 gate still running on an open bead is free" \
    "gate-unfinished free - gate-unfinished" \
    open 1 no no no no yes no 0 no - no no 1 -

row "open decision blocker is free" \
    "decision-blocked free unjudged-decision-blocked decision-blocked" \
    open 1 no no no no no yes 0 no - no no 1 -

row "G3 timeout with nothing committed is free" \
    "timeout free - timeout" \
    open 1 no no no no no no 124 no - no no 1 -

row "G3 timeout WITH a commit falls through to session_outcome, not the timeout branch" \
    "open charge - unlanded" \
    open 1 no no no no no no 124 yes - no no 1 unlanded

row "harness requeue (rebase-conflict) is free" \
    "requeue-rebase-conflict free rebase-conflict requeue" \
    open 1 no no no no no no 0 no rebase-conflict no no 1 -

row "operator-wait is free" \
    "operator-wait free unjudged-operator-wait operator-wait" \
    open 1 no no no no no no 0 no - yes no 1 -

row "submitted (sp-qsona) is free: done work waiting on the landing pass" \
    "submitted free - submitted" \
    open 1 no no no no no no 0 no - no no 1 unlanded yes

row "submitted omitted (15 args) defaults to no, falls through to unlanded" \
    "open charge - unlanded" \
    open 1 no no no no no no 0 no - no no 1 unlanded

# ---- G15: unjudged-<cause> requeue for killed/refused/unknown outcomes --------------------

row "G15 not-judged: refused" \
    "open free unjudged-refused not-judged" \
    open 1 no no no no no no 0 no - no no 1 refused

row "G15 not-judged: killed" \
    "open free unjudged-killed not-judged" \
    open 1 no no no no no no 0 no - no no 1 killed

row "G15 not-judged: unknown" \
    "open free unjudged-unknown not-judged" \
    open 1 no no no no no no 0 no - no no 1 unknown

# ---- G8: precedence — every adjacent pair, both markers set at once ------------------

row "G8 capacity beats slain" \
    "capacity free - capacity" \
    open 0 yes no no no no no 0 no - no no 1 -

row "G8 slain beats thrash" \
    "slain free - slain" \
    open 1 yes yes yes no no no 0 no - no no 1 -

row "G8 thrash beats lapsed" \
    "requeue-thrash free thrash thrash" \
    open 1 no yes no yes no no 0 no - no no 1 -

row "G8 lapsed beats gate-unfinished" \
    "lapsed charge - lapsed" \
    open 1 no no no yes yes no 0 no - no no 1 -

row "G8 gate-unfinished beats decision-blocked" \
    "gate-unfinished free - gate-unfinished" \
    open 1 no no no no yes yes 0 no - no no 1 -

row "G8 decision-blocked beats timeout" \
    "decision-blocked free unjudged-decision-blocked decision-blocked" \
    open 1 no no no no no yes 124 no - no no 1 -

row "G8 timeout beats a harness requeue cause" \
    "timeout free - timeout" \
    open 1 no no no no no no 124 no rebase-conflict no no 1 -

row "G8 a harness requeue cause beats operator-wait" \
    "requeue-rebase-conflict free rebase-conflict requeue" \
    open 1 no no no no no no 0 no rebase-conflict yes no 1 -

row "G8 operator-wait beats yield-headless" \
    "operator-wait free unjudged-operator-wait operator-wait" \
    open 1 no no no no no no 0 no - yes yes 1 -

row "G8 operator-wait beats submitted" \
    "operator-wait free unjudged-operator-wait operator-wait" \
    open 1 no no no no no no 0 no - yes no 1 - yes

row "G8 submitted beats yield-headless" \
    "submitted free - submitted" \
    open 1 no no no no no no 0 no - no yes 1 - yes

row "G8 yield-headless beats pre-session" \
    "yield-headless charge - yield-headless" \
    open 1 no no no no no no 0 no - no yes 0 -

row "G8 pre-session beats a would-be unlanded session_outcome" \
    "pre-session charge - pre-session" \
    open 1 no no no no no no 0 no - no no 0 unlanded

# ---- status propagation: only the fallthrough (session_outcome) branches echo it -----

row "raw bd status propagates through the unlanded fallthrough, not a literal" \
    "in_progress charge - unlanded" \
    in_progress 1 no no no no no no 0 no - no no 1 unlanded

row "a missing status reads ? rather than an empty ledger field" \
    "? free unjudged-refused not-judged" \
    "" 1 no no no no no no 0 no - no no 1 refused

tl_summary
