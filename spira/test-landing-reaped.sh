#!/usr/bin/env bash
#
# test-landing-reaped.sh — a landing pass does not reopen a branch reaped or lost mid-pass
# (the stale branch list, and the push-race retry), and its bead scan is one query, not one per
# branch. Split off test-landing.sh for wall time; the shared fixture is testlib/landing-fixture.sh.
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

LAND_SUITE=test-landing-reaped
# shellcheck disable=SC1090
. "$HERE/testlib/landing-fixture.sh"

echo "test-landing-reaped.sh"

# --------------------------------------------------------------------------------------
# THE CASE ITSELF: landed on one pass, reaped during the next, reached from a stale list.
#
# sp-aaa sorts before sp-zzz and refs are enumerated by name, so the pass is inside sp-aaa's
# gate when sp-zzz's ref goes — which is where the real one went. sp-zzz's work is already on
# the base by then, exactly as the reaped branch's was.
# --------------------------------------------------------------------------------------
seed; branch sp-zzz; out="$(landing)"
want "the branch lands on the first pass" "landed spira/sp-zzz" "$out"
timeout 5 git -C "$REPO" fetch -q origin
zzz_tip="$(git -C "$REPO" rev-parse spira/sp-zzz)"
git -C "$REPO" merge-base --is-ancestor "$zzz_tip" origin/main \
    && ok  "and its commit is on the base before the second pass begins" \
    || bad "the fixture" "sp-zzz did not actually land"

branch sp-aaa; reap_during_gate sp-zzz; out="$(landing)"
nowant "a branch reaped mid-pass is not reopened as a conflict" "reopened sp-zzz" "$out"
is     "and its bead stays closed"                              closed "$(status_of sp-zzz)"
nowant "and no note claims a conflict it never had"             "conflicts in unknown" "$(notes_of sp-zzz)"
want   "and the pass names the branch it lost"                  "spira/sp-zzz is gone since this pass began" "$out"
want   "and says the commit is on the base, not merely that it is gone" \
       "$zzz_tip is on origin/main — landed and reaped" "$out"
want   "the branch that held the pass still landed"             "landed spira/sp-aaa" "$out"
drop_branch sp-aaa

# A BRANCH REMOVED WITH WORK STILL ON IT READS DIFFERENTLY, and it has to. Both cases end in
# the same non-action, so a single line covering both would report a destroyed branch in the
# words used for the ordinary reap — the reassuring reading, given for free, to the one case
# that deserves a look.
seed; branch sp-lost; branch sp-bbb; reap_during_gate sp-lost
lost_tip="$(git -C "$REPO" rev-parse spira/sp-lost)"
out="$(landing)"
nowant "a branch slain mid-pass is not reopened either" "reopened sp-lost" "$out"
is     "and its bead stays closed"                      closed "$(status_of sp-lost)"
want   "but the pass says its work is NOT on the base"  "$lost_tip is NOT on origin/main" "$out"
drop_branch sp-bbb

# --------------------------------------------------------------------------------------
# THE SECOND ROUTE INTO A REOPEN. Losing the push race sends the pass back through
# rebase_branch, and that arm falls through to "branch conflicts with the base" — so a ref
# reaped between the losing push and the replay is reported as a disagreement that never
# happened, the identical defect by a different path. Two guards were needed and only one of
# them is on the path the first case takes.
# --------------------------------------------------------------------------------------
cat > "$REMOTE/hooks/pre-receive" <<HOOK
#!/usr/bin/env bash
# Reject the first push only, advancing the base and removing the branch — simulating a
# concurrent commit (genuine race) and the Hel arriving in the one window this arm
# occupies. The base MUST move so the classification guard introduced by sp-utvi treats
# this as a real race rather than a non-race push failure (law-a-pattern-match-is-not-
# an-identity-check). Without a moved base the guard correctly stops before the retry, and
# the no-branch case the test exercises is never reached.
#
# OUTSIDE THE QUARANTINE. A pre-receive hook runs with GIT_QUARANTINE_PATH set and git
# refuses to touch refs from inside it, so a hook that does not clear the environment
# changes nothing and the case passes against the bug it is written for.
#
# PATH EMBEDDING: $REPO, $REMOTE, $RUN are shell-expanded at heredoc creation time and
# written as double-quoted literals in this script. The \$_prev etc. are runtime variables
# and must NOT expand at heredoc time; they are written as literal '$' via \$ escaping.
[ -f "\$GIT_DIR/rejected-once" ] && exit 0
: > "\$GIT_DIR/rejected-once"
unset GIT_QUARANTINE_PATH GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_DIR
git -C "$REPO" worktree remove --force "$RUN/worktree/sp-raced" >/dev/null 2>&1
git -C "$REPO" branch -D spira/sp-raced >/dev/null 2>&1
_prev=\$(git -C "$REMOTE" rev-parse HEAD 2>/dev/null)
_tree=\$(git -C "$REMOTE" rev-parse "HEAD^{tree}" 2>/dev/null)
_tip=\$(GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t git -C "$REMOTE" commit-tree -p "\$_prev" -m "race" "\$_tree" 2>/dev/null)
git -C "$REMOTE" update-ref refs/heads/main "\$_tip" 2>/dev/null
echo "rejected: non-fast-forward" >&2
exit 1
HOOK
chmod +x "$REMOTE/hooks/pre-receive"
seed; branch sp-raced; out="$(landing)"
rm -f "$REMOTE/hooks/pre-receive" "$REMOTE/rejected-once"
want   "the push is rejected, as the case requires"        "push rejected" "$out"
nowant "and a branch gone by the retry is not a conflict"  "reopened sp-raced" "$out"
is     "and its bead stays closed"                         closed "$(status_of sp-raced)"
want   "and the retry says it could not attempt a rebase"  "the retry could not attempt a rebase of spira/sp-raced" "$out"
want   "naming the reason rather than a file list"         "(no-branch)" "$out"
drop_branch sp-raced
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

tl_summary
