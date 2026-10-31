#!/usr/bin/env bash
#
# test-submitted-lands-hel-certified.sh — a work bead the model submits (sp-qsona; `work submit` since
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
# SPLIT FOR WALL TIME: this file runs case 3 and 4 of the four below; the others are
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

SL_SUITE=test-submitted-lands-hel-certified
# shellcheck disable=SC1090
. "$HERE/testlib/sl-fixture.sh"

echo "test-submitted-lands-hel-certified.sh"

# ======================================================================================
echo
echo "3. pr/hold mode — a submitted bead whose branch the Hel finds on the base closes:"
# ======================================================================================
# The forge (or a human) merged the branch; nothing on this box landed it. Simulated with a
# merge made directly on origin/main, then the Hel sweep.
testdb_reset
git -C "$REPO" branch -q -f spira/sp-sl3 origin/main
git -C "$REPO" worktree add -q "$TMP/wt3" spira/sp-sl3
printf 'three\n' > "$TMP/wt3/three.txt"
git -C "$TMP/wt3" add -A; git -C "$TMP/wt3" commit -qm "sp-sl3: the work"
git -C "$REPO" worktree remove --force "$TMP/wt3"
seed sp-sl3 SUBMITTED "$(branch_tip sp-sl3)"
git -C "$REPO" checkout -q main 2>/dev/null; git -C "$REPO" reset -q --hard origin/main
git -C "$REPO" merge -q --no-ff -m "Merge pull request #3 from spira/sp-sl3" spira/sp-sl3
timeout 5 git -C "$REPO" push -q origin main 2>/dev/null
timeout 5 git -C "$REPO" fetch -q origin
is   "setup: open and SUBMITTED before the sweep" "open SUBMITTED" "$(field sp-sl3 status) $(lcfix_state sp-sl3)"
out="$(hel)"
want "the Hel sends the landed branch"             "sp-sl3" "$out"
is   "and closes the submitted bead"                   closed "$(field sp-sl3 status)"
want "with the landed outcome"                         "OUTCOME: landed" "$(field sp-sl3 close_reason)"

# ======================================================================================
echo
echo "4. push mode — a row the push gate moved SUBMITTED -> CERTIFIED at the tip still lands:"
# ======================================================================================
# Local acceptance on d40bbb589: the model finishes with `work submit` (no bd close), so the
# bead is open and only the lifecycle row says SUBMITTED. The push gate itself records
# GatePass (SUBMITTED -> CERTIFIED) before the land; the pass then re-read the bead, found
# no SUBMITTED row, logged "bead is now open (was closed at scan time) — not landing" and
# every later pass skipped it as "its bead is open": READY, WORKING, SUBMITTED, CERTIFIED
# and never LANDED. Here the row is a real one and the gate stub's `spira-lc certify` is the
# real GatePass, so the pass's re-read sees exactly what production's does.
testdb_reset
timeout 5 git -C "$REPO" fetch -q origin
timeout 5 git -C "$REPO" branch -q -f spira/sp-sl4 origin/main
timeout 5 git -C "$REPO" worktree add -q "$TMP/wt4" spira/sp-sl4
printf 'four\n' > "$TMP/wt4/four.txt"
timeout 5 git -C "$TMP/wt4" add -A; timeout 5 git -C "$TMP/wt4" commit -qm "sp-sl4: the work"
timeout 5 git -C "$REPO" worktree remove --force "$TMP/wt4"
tip4="$(timeout 5 git -C "$REPO" rev-parse spira/sp-sl4)"
seed sp-sl4 SUBMITTED "$tip4"
is   "setup: the bead is open and SUBMITTED at its tip" "open SUBMITTED $tip4" \
     "$(field sp-sl4 status) $(lcfix_state sp-sl4) $(lcfix_tip sp-sl4)"
out="$(landing)"
is     "the gate moved the row to CERTIFIED before the land"         CERTIFIED "$(cat "$TMP/gate-state" 2>/dev/null)"
nowant "the pass does not refuse the land right after its own gate" "was closed at scan time" "$out"
want   "the landing pass lands the certified bead's branch"         "landed spira/sp-sl4" "$out"
want   "its commit is on origin/main"                               "sp-sl4: the work" "$(on_base)"
is     "push-delivered moves the CERTIFIED row to LANDED"           LANDED "$(lcfix_state sp-sl4)"
is     "and close-on-land closes the bead"                          closed "$(field sp-sl4 status)"

tl_summary
