#!/usr/bin/env bash
#
# test-submitted-lands-rework.sh — a work bead the model submits (sp-qsona; `work submit` since
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
# SPLIT FOR WALL TIME: one file per slice of the four cases — test-submitted-lands.sh (1),
# test-submitted-lands-rework.sh (2, the red gate, from a seeded SUBMITTED row), test-submitted-lands-reclaim.sh
# (2, the re-claim, from a seeded REWORK row) and test-submitted-lands-hel-certified.sh (3, 4).
# The shared fixture is testlib/sl-fixture.sh.
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

SL_SUITE=test-submitted-lands-rework
# shellcheck disable=SC1090
. "$HERE/testlib/sl-fixture.sh"

echo "test-submitted-lands-rework.sh"

# ======================================================================================
echo
echo "2. a submitted bead whose gate is RED goes back to the builders — claimable again:"
# ======================================================================================
# The row is seeded as `work submit` leaves it (SUBMITTED at the branch tip, the bead open);
# the model's own submit through the dwarf is test-submitted-lands.sh case 1.
testdb_reset
timeout 5 git -C "$REPO" fetch -q origin
timeout 5 git -C "$REPO" branch -q -f spira/sp-sl2 origin/main
timeout 5 git -C "$REPO" worktree add -q "$TMP/wt2" spira/sp-sl2
printf 'two\n' > "$TMP/wt2/two.txt"
timeout 5 git -C "$TMP/wt2" add -A; timeout 5 git -C "$TMP/wt2" commit -qm "sp-sl2: the work"
timeout 5 git -C "$REPO" worktree remove --force "$TMP/wt2"
seed sp-sl2 SUBMITTED "$(branch_tip sp-sl2)"
is   "setup: submitted" SUBMITTED "$(lcfix_state sp-sl2)"
gate_fail
out="$(landing)"
gate_pass
is     "the red gate moves the row to REWORK, which the ready set claims" REWORK "$(lcfix_state sp-sl2)"
nowant "the red branch is not landed"                                     "landed spira/sp-sl2" "$out"
nowant "and nothing of it reaches the base"                               "sp-sl2" "$(on_base)"
is     "the bead is open"                                                 open "$(field sp-sl2 status)"

tl_summary
