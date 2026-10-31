#!/usr/bin/env bash
#
# test-landing.sh — a landing pass may put finished work back on the board only when the
# work itself disagrees with the base.
#
#   ./test-landing.sh
#
# THE CASE THIS IS WRITTEN FOR. A pass reads its branch list once and then runs for minutes,
# so by the time it reaches the tail of that list the head of it may be gone. The Hel
# reaps a landed branch on its own timer, and `rebase_branch` against a ref that is no longer
# there fails with exactly the exit status and exactly the empty conflict list that a real
# disagreement produces:
#
#   21:51:17  landed spira/<id>                      pass A lands it
#   21:52:13  landing: starting a pass               pass B enumerates, <id> still present
#   21:52:36  REMOVED branch spira/<id>              the Hel reaps it
#   22:00:51  landed spira/<other>                   pass B is eight minutes into one gate
#   22:00:55  reopened <id> — does not rebase onto origin/main; conflicts in unknown
#
# Nothing recreated that ref and nothing needed to: the branch list was twenty-three seconds
# older than the reap, refs sort by name, and the branch ahead of it in the list held the
# pass for eight minutes. "conflicts in unknown" is the tell — a real conflict names files.
# The cost is a whole session: a dwarf is called onto a bead whose work is already on the
# base, finds nothing to rebase, and learns that.
#
# So the property under test is not "the reaped branch is skipped" but the stronger one the
# skip is a special case of: A REOPEN REQUIRES AN ATTRIBUTED CONFLICT. Both routes into a
# reopen are exercised, the classification they rest on is exercised directly, and a genuine
# conflict is required to still reopen — without that last one every silence here is vacuous.
#
# The database is a REAL bd on a fixture dropped by a trap and git is real, with a real bare
# remote, because every claim is about ancestry, about what `git rebase` does against a ref
# that is not there, and about a bead's status afterwards. `gate.sh` and `confine.sh` are
# stubs: each has its own suite, and what is under test here is what landing does with a
# verdict, not how one is reached. The gate stub is also the clock — it is the only point in
# the pass this suite can reach, and reaping a branch from inside it reproduces the real
# sequence exactly rather than approximating it.
#
# SPLIT FOR WALL TIME: the cases after the positive control live in test-landing-reaped.sh (the
# reap and push-race routes, and the bulk scan) and test-landing-stale-dup.sh (a bead changed
# mid-gate, and the parallel duplicate). The shared fixture is testlib/landing-fixture.sh.
#
# defect: sp-q9i sp-9194o
# tier: T3
# covers: landing-pass/* spira/lib.sh UC-landing-merge-queue-13 UC-landing-merge-queue-15 UC-landing-merge-queue-17 UC-landing-merge-queue-22 UC-landing-merge-queue-23
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=90
# THE LANDING PASS IS THE landing-pass BINARY (landing-pass/DESIGN.md §7.4): `land` for a
# pass, `halt` to stop one. Resolved from this tree before any fixture repoints SPIRA_REPO.

LAND_SUITE=test-landing
# shellcheck disable=SC1090
. "$HERE/testlib/landing-fixture.sh"

echo "test-landing.sh"

# --------------------------------------------------------------------------------------
# THE POSITIVE CONTROL, first, because every silence below is read against it: this pass can
# land, and it can reopen. A suite whose assertions are all "did not happen" passes just as
# well against a landing.sh that does nothing at all.
# --------------------------------------------------------------------------------------
: > "$EMITTED"; : > "$RUN/events.log"
seed; branch sp-plain; out="$(landing)"
want "an uncontested land is reported" "landed spira/sp-plain" "$out"
# Landing pushes the base from the .landing worktree; the home checkout must be advanced in
# the same pass or every hand-run script reads a past state (sp-wud).
want "the same pass also advances the checkout humans read" "skew: refreshed to" "$out"
# AND IT SURVIVES THE PASS. The mailbox above is drained by the sentinel and the log line
# scrolls off the health pane below the fourth row; the event is the only record still
# answerable tomorrow. It is emitted AFTER the push, so what it asserts is the ancestry just
# proved rather than the close that preceded it (law-closed-is-not-landed).
want "the land is recorded as an event"  "kind: bead.landed"   "$(events)"
want "against the bead that landed"      "target: sp-plain"    "$(events)"
drop_branch sp-plain

# A GATE FAILURE ALSO RECORDS AN EVENT, and the negative is the landing above: a gate that
# passed produced bead.landed, not bead.reopened, so the check is not just reading the stub.
: > "$EMITTED"; : > "$RUN/events.log"
stub gate.sh 'echo "gate: VERDICT=FAIL reason=stub-fail branch=$1 repo=${2:-?}" >&2; exit 1'
seed; branch sp-gfail; out="$(landing)"
want "a failed gate reopens the bead"          "reopened sp-gfail — failed the gate" "$out"
want "and the reopen is recorded as an event"  "kind: bead.reopened" "$(events)"
want "against the bead that was reopened"      "target: sp-gfail"    "$(events)"
drop_branch sp-gfail
cp "$TMP/gate-full.sh" "$SH/gate.sh"   # restore for the tests that follow

# THE VERDICT CACHE IS PRUNED BY THE PASS, at the age the gate refuses to read at. The gate
# computes each verdict once and reuses it, so what accumulates here is one file per gated
# tree, forever; the pass is the only thing that runs on a clock and already touches every
# repository, which is why the janitor lives in it.
#
# THE AGE IS THE CONFIGURED ONE and that is the whole point of the case: two numbers here —
# a reader's and a janitor's — would be two answers to how long a verdict lives, and the
# operator would have tuned one of them. Pinned to a non-default, because asserting against
# the shipped default passes just as well against a literal written into the code.
# AND A REUSED VERDICT IS VISIBLE IN THE PASS. The gate returns the same status either way,
# so without this line a pass that skipped every gate and one that ran every gate read
# identically — and "nothing is being reused any more" is the first symptom of a key that has
# stopped matching anything, which otherwise looks exactly like a busy queue.
seed; branch sp-reused
out="$(GATE_REASON=cached landing)"
want "a pass says when a gate was skipped" "this tree had already passed, so no suite ran" "$out"
want "and still lands the branch"          "landed spira/sp-reused" "$out"
drop_branch sp-reused
# bash keeps a temporary assignment to a FUNCTION set after the call returns, so every later
# pass in this suite would go on claiming a reused verdict.
unset GATE_REASON

tl_config SPIRA_VERDICT_TTL=600
mkdir -p "$RUN/verdicts"
: > "$RUN/verdicts/stale"; touch -d '3 hours ago' "$RUN/verdicts/stale"
: > "$RUN/verdicts/fresh"
seed; landing >/dev/null 2>&1
[ -e "$RUN/verdicts/stale" ] && bad "a verdict past the TTL is deleted by a pass" "stale entry survived" \
    || ok "a verdict past the TTL is deleted by a pass"
[ -e "$RUN/verdicts/fresh" ] && ok "and one inside it is kept" \
    || bad "and one inside it is kept" "the pass deleted a live verdict"
tl_config SPIRA_VERDICT_TTL=86400   # restore the fixture's own default for the rest of the suite

# A REAL DISAGREEMENT STILL REOPENS. The branch and the base both write the same file with
# different content after they diverged, so the rebase genuinely conflicts and the bead
# genuinely belongs back on the board. Everything below asserts that a reopen did NOT happen;
# this is what makes those assertions mean something.
seed; branch sp-clash shared.txt "from the branch"
printf '%s\n' "from the base" > "$REPO/shared.txt"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "base writes shared.txt"
timeout 5 git -C "$REPO" push -q origin main; timeout 5 git -C "$REPO" fetch -q origin
out="$(landing)"
want "a branch that truly conflicts is reopened"   "reopened sp-clash" "$out"
is   "and its bead goes back to open"              open "$(status_of sp-clash)"
want "and the note names the file that collided"   "shared.txt" "$(notes_of sp-clash)"
drop_branch sp-clash

# --------------------------------------------------------------------------------------
# A BRANCH WHOSE BEAD IS ABSENT FROM THE DATABASE
#
# A branch named spira/<id> with no bead in the store must be treated exactly as a bdjson
# show that returned nothing: skipped as non-closed, never landed, never errored.
# --------------------------------------------------------------------------------------
seed
git -C "$REPO" worktree add -q -b "spira/sp-ghost" "$RUN/worktree/sp-ghost" main
printf 'ghost\n' > "$RUN/worktree/sp-ghost/ghost.txt"
git -C "$RUN/worktree/sp-ghost" add -A
git -C "$RUN/worktree/sp-ghost" commit -q -m "feat: sp-ghost — work"
# sp-ghost has a branch but NO bead in the database.
out="$(landing)"
want "an absent bead is skipped as non-closed" "spira/sp-ghost not landed — its bead is -" "$out"
nowant "and is never landed"                   "landed spira/sp-ghost" "$out"
git -C "$REPO" worktree remove --force "$RUN/worktree/sp-ghost" >/dev/null 2>&1
git -C "$REPO" branch -D spira/sp-ghost >/dev/null 2>&1

# --------------------------------------------------------------------------------------
# THE REPO: LABEL FALLS BACK TO THE SWEPT REPOSITORY
#
# A bead with no repo: label must be treated as belonging to the repository being swept,
# not rejected as unroutable.
# --------------------------------------------------------------------------------------
seed; branch sp-nolabel
# The branch helper creates a bead with empty labels. Verify landing treats it as
# belonging to the fixture repo.
out="$(landing)"
want "a bead with no repo: label still lands" "landed spira/sp-nolabel" "$out"
drop_branch sp-nolabel

tl_summary
