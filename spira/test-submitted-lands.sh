#!/usr/bin/env bash
#
# test-submitted-lands.sh — a work bead the model submits (sp-qsona; `work submit` since
#   sp-v62vn) must, in a NON-QUEUE repository, still land and close: work submit ->
#   SUBMITTED -> landing pass gates and lands it -> LANDED, and close-on-land closes it.
#
# THE DEFECT THIS REPRODUCES. sp-qsona made spira-lc close-on-land the only thing that closes a
# work bead, called when the work reaches the base. Queue mode reaches it through the batch
# verdict. Every other mode reaches the base through landing.sh's CHECK 6 (push merges it,
# pr opens a pull request, hold gates it) — and CHECK 6 skipped, refused to certify and
# refused to land any branch whose bead was not `closed`. A submitted bead is open, so in
# push, pr and hold mode nothing ever landed and nothing ever closed. Release acceptance runs
# a push-mode scratch repo and failed phase A stage 5 ("no commit with bead id on origin/main
# after 121s") on every release from the one that shipped sp-qsona.
#
# CASES (law-absence-needs-a-positive-control), against a real lifecycle record (below):
#   1. END TO END, push mode: the real dwarf runs a model stand-in that commits and finishes
#      with `work submit` -> the row is SUBMITTED at the branch tip; the real landing pass
#      gates it (the gate's GatePass: CERTIFIED), lands it on origin/main, push-delivered
#      moves the row to LANDED and close-on-land closes the bead in bd.
#   2. A submitted bead whose gate is RED goes back to the builders: the gate's GateRed
#      moves the row to REWORK and nothing lands — and the next dwarf really claims it again
#      and re-submits. A red that left it unclaimable would strand it: open, excluded from
#      every claim, never landed.
#   3. pr/hold mode: the merge happens off-box, and the Hel is the first to see the work
#      on the base. A SUBMITTED bead whose branch is found landed there is closed.
#   4. push mode: a bead whose lifecycle row the push gate moved from
#      SUBMITTED to CERTIFIED is still landed by that same pass, and ends LANDED.
#
# sp-v62vn (lifecycle_enforce=off retired) changed what cases 1-3 are told in: the model
# no longer `bd close`s and the dwarf no longer converts that close into open +
# spira-submitted, so the assertions that read the spira-submitted label (1, 2, the setup
# of 3) now read the lifecycle row it stood for; case 2's "reopened <id>" line is replaced
# by REWORK plus a real re-claim (see case 2's note).
#
# SPLIT FOR WALL TIME: this file runs case 1 of the four below; the others are
# test-submitted-lands.sh (1), test-submitted-lands-rework.sh (2) and
# test-submitted-lands-hel-certified.sh (3, 4). The shared fixture is testlib/sl-fixture.sh.
#
# defect: sp-qsona (acceptance phase A stage 5)
# tier: T3
# covers: landing-pass/* hel/src/* spira/lib.sh dwarf/src/* work/* spira-lc/*
# hermetic-ok: uses fixture databases (bd, and a private dolt sql-server for spira_lifecycle) and local git repos, no systemd or gh
# timeout: 240
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=150
# THE LANDING PASS IS THE landing-pass BINARY (landing-pass/DESIGN.md §7.4): `land` for a
# pass, `halt` to stop one. Resolved from this tree before any fixture repoints SPIRA_REPO.
#
# THE LIFECYCLE SERVICE IS REAL (sp-v62vn retired lifecycle_enforce=off). The dwarf always
# runs the model restricted, and a model finishes only through the work broker: `work
# submit` -> `spira-lc serve` -> the bead machine. So this suite stands up a throwaway
# spira_lifecycle database (testlib/lc-fixture.sh) behind the tree's own `spira-lc serve`,
# and every actor — the dwarf's claim, the model's submit, the gate stub's verdict, the
# landing pass's delivery, close-on-land — reads and writes real lifecycle rows. bd holds
# the bead's content only; no assertion here reads bd status as the bead's state, except
# the close that close-on-land still writes to bd when the work lands.

SL_SUITE=test-submitted-lands
# shellcheck disable=SC1090
. "$HERE/testlib/sl-fixture.sh"

echo "test-submitted-lands.sh"

# ======================================================================================
echo
echo "1. END TO END, push mode — work submit -> SUBMITTED -> CERTIFIED -> LANDED -> closed:"
# ======================================================================================
testdb_reset; seed sp-sl1
run_dwarf
is   "the model's work submit was applied by the broker"             0 "$(cat "$TMP/submit.rc" 2>/dev/null || echo missing)"
is   "after the dwarf: the lifecycle row is SUBMITTED"                SUBMITTED "$(lcfix_state sp-sl1)"
is   "at the branch tip the model committed"                         "$(branch_tip sp-sl1)" "$(lcfix_tip sp-sl1)"
is   "bd's status was never touched (no close to convert)"           open "$(field sp-sl1 status)"
nowant "and nothing is on the base yet"                              "sp-sl1" "$(on_base)"

out="$(landing)"
is   "the gate's GatePass certified the row before the land"         CERTIFIED "$(cat "$TMP/gate-state" 2>/dev/null)"
want "the landing pass lands the submitted bead's branch"            "landed spira/sp-sl1" "$out"
want "its commit is on origin/main"                                  "sp-sl1: the work" "$(on_base)"
is   "the lifecycle row ends LANDED"                                 LANDED "$(lcfix_state sp-sl1)"
is   "and the bead is closed by the landing"                         closed "$(field sp-sl1 status)"
want "with the landed outcome as its close reason"                   "OUTCOME: landed" "$(field sp-sl1 close_reason)"
nowant "never the 'not landed — its bead is open' skip"              "its bead is open" "$out"

