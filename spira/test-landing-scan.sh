#!/usr/bin/env bash
#
# test-landing-scan.sh — a landing pass reads its beads in one bulk query, not one per branch, and
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
# THE BULK SCAN: one query not N
#
# The scan was the whole cost of a quiet pass, growing linearly with the branch count.
# A counting shim around bd proves the scan issues one show per repository, not one per
# branch.
# --------------------------------------------------------------------------------------
echo
seed; branch sp-cnt1; branch sp-cnt2; branch sp-cnt3
cat > "$TMP/bd-counter.sh" <<'SHIM'
#!/usr/bin/env bash
# Count bd show invocations by appending to a log. bdq prepends -C <db> before the
# subcommand, so match anywhere in the args rather than on $1.
case "$*" in *" show "*|*" show") printf '%s\n' "$*" >> "${BD_CALL_LOG:?}" ;; esac
exec "$BD_REAL" "$@"
SHIM
chmod +x "$TMP/bd-counter.sh"
export BD_CALL_LOG="$TMP/bd-calls.log" BD_REAL="${SPIRA_BD:-bd}"
SPIRA_BD="$TMP/bd-counter.sh"   # shell var only: landing()'s own tl_config read picks this up
landing >/dev/null 2>&1
SPIRA_BD="$BD_REAL"; unset BD_CALL_LOG BD_REAL
show_calls="$(grep -c '^show' "$TMP/bd-calls.log" 2>/dev/null || echo 0)"
# One bulk show for the scan, plus one re-read per branch that reaches the gate (three
# here). The scan must not grow with the branch count — three branches and eleven must
# both start with one show.
want "three branches land with one scan query plus per-branch re-reads" \
     "show" "$(head -1 "$TMP/bd-calls.log" 2>/dev/null)"
# The first call is the bulk scan; it carries all three IDs on one line.
first_call="$(head -1 "$TMP/bd-calls.log" 2>/dev/null)"
want "the scan carries all ids in one call" "sp-cnt1" "$first_call"
want "including the second"                 "sp-cnt2" "$first_call"
want "and the third"                        "sp-cnt3" "$first_call"
drop_branch sp-cnt1; drop_branch sp-cnt2; drop_branch sp-cnt3
rm -f "$TMP/bd-calls.log"

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
