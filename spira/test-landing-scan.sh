#!/usr/bin/env bash
#
# test-landing-scan.sh — a landing pass
# skips a branch whose bead is absent or has no repo: label rather than erroring. Split off
# test-landing.sh for wall time; the shared fixture is testlib/landing-fixture.sh.
#
# defect: sp-q9i sp-9194o
# tier: T2
# covers: landing-pass/* spira/lib.sh UC-landing-merge-queue-13 UC-landing-merge-queue-15 UC-landing-merge-queue-17 UC-landing-merge-queue-22 UC-landing-merge-queue-23
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=90
# THE LANDING PASS IS THE landing-pass BINARY (landing-pass/DESIGN.md §7.4): `land` for a
# pass, `halt` to stop one. Resolved from this tree before any fixture repoints SPIRA_REPO.

LAND_SUITE=test-landing-scan
# shellcheck disable=SC1090
. "$HERE/testlib/landing-fixture.sh"

echo "test-landing-scan.sh"


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
