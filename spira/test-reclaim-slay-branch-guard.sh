#!/usr/bin/env bash
#
# test-reclaim-slay-branch-guard.sh — end-to-end: spira_destroy_branch refuses to delete
#   a branch carrying commits not on origin/<base>, in the reclaim and slay paths.
#
#   ./test-reclaim-slay-branch-guard.sh
#
# THE PROPERTY UNDER TEST (sp-r0ay / sp-mqsl). Before the content fence was added,
# spira_destroy_branch called git branch -D unconditionally. A reclaim or slay
# operation on a bead with unlanded commits would silently lose that work with no
# error and no log entry from landing. This test exercises the complete stack —
# real git repository, real beads database, real slay.sh — to verify that work
# is protected in both paths.
#
# Two complementary assertions:
#
#   SLAY PATH. slay.sh parks unlanded commits at refs/slain/<id> before calling
#   spira_destroy_branch with the "slain" bypass. The test verifies the commit
#   survives deletion at that ref — that the durable copy was written BEFORE the
#   branch was removed. A version that called git branch -D unconditionally would
#   pass the parking assertion but then silently delete the parked ref too, so the
#   test checks the ref explicitly after the branch is gone.
#
#   GATE PATH. spira_destroy_branch called without the caller bypass (the shape a
#   new reclaim path would take if it forgot to park first) MUST refuse deletion of
#   an unlanded branch. The gate is what "reclaim/slay gates must verify branch is
#   on base before deletion" actually means — the function-level fence that catches
#   any caller that reached the deletion without first securing the work.
#
# A POSITIVE CONTROL precedes each absence assertion so an implementation that
# refused or deleted everything would not pass silently
# (law-absence-needs-a-positive-control).
#
# A REAL bd ON A THROWAWAY DATABASE (law-prefer-the-real-dependency): slay.sh
# reads the bead's status and repo label; a stub would drift. A real git fixture
# with a bare remote for branch operations.
#
# defect: sp-mqsl
# covers: spira/lib.sh spira/slay.sh
# scar: spira_destroy_branch deleted branches unconditionally; a slay or reclaim on a bead with unlanded commits silently lost that work.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-reclaim-slay-branch-guard
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT; trap 'exit 143' INT TERM
testdb_up slaybrachguard || { echo "test-reclaim-slay-branch-guard: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; REMOTE="$TMP/remote.git"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN/worktree"
export SPIRA_REPO="$REPO"
export SPIRA_CONF="$TMP/no-such-conf"
export SPIRA_GOAL=sp-goal
export SPIRA_REPO_MAP="$TMP/repo-map"
export SPIRA_REAPLOG="$SPIRA_RUN/reap.log"
printf '# fixture\n' > "$TMP/repo-map"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main

# shellcheck disable=SC1090
. "$HERE/lib.sh"
SLAY="$HERE/slay.sh"

# ---- helpers -----------------------------------------------------------------------

branch_exists() { git -C "$REPO" show-ref --verify -q "refs/heads/$1" 2>/dev/null; }
slain_ref_exists() { git -C "$REPO" show-ref --verify -q "refs/slain/$1" 2>/dev/null; }

seed() {
    local id="$1" st="${2:-open}" as="${3:-}"
    testdb_reset
    local line; line="{\"id\":\"$id\",\"title\":\"test bead\",\"status\":\"$st\",\"issue_type\":\"task\",\"labels\":[\"spira\",\"plan\"]"
    [ -n "$as" ] && line="$line,\"assignee\":\"$as\""
    line="$line,\"updated_at\":\"2026-09-09T00:00:00Z\"}"
    testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-09T00:00:00Z"}
$line
JSONL
}

# make_branch <id> — create a branch with a unique file commit, no worktree
make_branch() {
    local id="$1" br="spira/$1"
    git -C "$REPO" checkout -q -b "$br" main
    printf '%s\n' "$id" > "$REPO/$id.txt"
    git -C "$REPO" add "$id.txt"
    git -C "$REPO" commit -q -m "sp-$id: work"
    git -C "$REPO" checkout -q main
}

# make_work <id> — branch + committed work in a worktree (the slay path)
make_work() {
    local id="$1" br="spira/$1" wt="$SPIRA_RUN/worktree/$1"
    git -C "$REPO" branch -q "$br" main 2>/dev/null || true
    git -C "$REPO" worktree add -q "$wt" "$br" 2>/dev/null
    printf '%s\n' "$id" > "$wt/$id.txt"
    git -C "$wt" add "$id.txt"
    git -C "$wt" commit -q -m "sp-$id: work"
}

land() {
    local id="$1" br="spira/$1"
    git -C "$REPO" merge -q --squash "$br" >/dev/null 2>&1
    git -C "$REPO" commit -q -m "squash-land sp-$id"
    git -C "$REPO" push -q origin main
    git -C "$REPO" fetch -q origin
}

teardown() {
    local id="$1" wt="$SPIRA_RUN/worktree/$1"
    git -C "$REPO" worktree remove --force "$wt" 2>/dev/null || true
    rm -rf "$wt"
    git -C "$REPO" branch -D "spira/$id" 2>/dev/null || true
    git -C "$REPO" update-ref -d "refs/slain/$id" 2>/dev/null || true
    rm -f "$REPO/$id.txt"
    git -C "$REPO" checkout -q main -- . 2>/dev/null || true
}

echo "test-reclaim-slay-branch-guard.sh"

# ======================================================================================
# POSITIVE CONTROL — slay on a bead whose branch IS on origin/main (landed).
#
# The branch is on main; there is nothing unique to protect. slay deletes it without
# parking (no refs/slain entry). Without this control, an implementation that refused
# every deletion would pass the "branch still exists" assertion below.
# ======================================================================================
echo
echo "positive control — slay on landed branch (must delete without park):"

seed sp-s1 open
make_work sp-s1
land sp-s1

is "landed branch exists before slay" 0 \
   "$(branch_exists spira/sp-s1; echo $?)"

# Verify landing: content_landed must see this branch as landed.
if content_landed "$REPO" spira/sp-s1 origin/main; then
    ok "content_landed sees landed branch as landed (fixture confirmed)"
else
    bad "fixture: landed branch should be seen as landed by content_landed" "returned non-zero"
fi

out="$(bash "$SLAY" sp-s1 2>&1)"
rc=$?
is "slay exits 0 for landed branch"       0  "$rc"
is "landed branch is gone after slay"     1  "$(branch_exists spira/sp-s1; echo $?)"
is "landed branch is NOT parked"          1  "$(slain_ref_exists sp-s1; echo $?)"
nowant "does not report parking for landed branch" "parked" "$out"
teardown sp-s1

# ======================================================================================
# SLAY PATH — unlanded branch: work must be parked, not silently lost.
#
# This is the core scenario the defect hit. The aeon dies holding a branch with
# committed but unlanded work. slay.sh must park the tip at refs/slain/<id> BEFORE
# calling spira_destroy_branch (which bypasses the content fence with "slain").
# The parked ref makes the commit reachable after the branch is gone.
# ======================================================================================
echo
echo "slay path — unlanded branch (work must be parked and preserved):"

seed sp-s2 open
make_work sp-s2
tip="$(git -C "$REPO" rev-parse --short spira/sp-s2)"

is "unlanded branch exists before slay"   0  "$(branch_exists spira/sp-s2; echo $?)"

# Confirm the fixture: content_landed sees it as unlanded (so we are testing the right thing).
if content_landed "$REPO" spira/sp-s2 origin/main; then
    bad "fixture: unlanded branch should NOT be seen as landed" "content_landed returned 0"
else
    ok "fixture: content_landed correctly sees branch as unlanded"
fi

out="$(bash "$SLAY" sp-s2 2>&1)"
rc=$?
is "slay exits 0 for unlanded branch"              0  "$rc"
is "unlanded branch is gone after slay"            1  "$(branch_exists spira/sp-s2; echo $?)"
is "unlanded work is parked at refs/slain"          0  "$(slain_ref_exists sp-s2; echo $?)"
want "slay reports parking"                         "parked" "$out"

# Verify the parked ref points to the original tip commit — the commit is still reachable.
parked_sha="$(git -C "$REPO" rev-parse --short refs/slain/sp-s2 2>/dev/null)"
is "parked ref resolves to original tip" "$tip" "$parked_sha"
teardown sp-s2

# ======================================================================================
# GATE PATH — direct call without bypass on an unlanded branch must be refused.
#
# This is the fence that protects any future reclaim path that reaches
# spira_destroy_branch without first parking. The "slain" and "sending" bypass
# arguments are what ESTABLISHED callers pass after they have secured the work; a
# caller without one is unknown. The gate must refuse it, leaving the branch intact.
#
# A POSITIVE CONTROL comes first (with bypass → deletion succeeds) so an
# implementation that never reaches git branch -D cannot pass the absence check.
# ======================================================================================
echo
echo "gate path — positive control (bypass): unlanded branch IS deleted with bypass:"

seed sp-g1 open
make_branch sp-g1

is "gate +ctrl: branch exists before destroy" 0 \
   "$(branch_exists spira/sp-g1; echo $?)"

spira_destroy_branch sp-g1 spira/sp-g1 "$REPO" "test: bypass" sending >/dev/null 2>&1
rc=$?
is "gate +ctrl: destroy exits 0 with bypass"       0  "$rc"
is "gate +ctrl: branch gone with bypass"           1  "$(branch_exists spira/sp-g1; echo $?)"

echo
echo "gate path — no bypass on unlanded branch must be refused:"

seed sp-g2 open
make_branch sp-g2

is "gate: unlanded branch exists before destroy" 0 \
   "$(branch_exists spira/sp-g2; echo $?)"

out="$(spira_destroy_branch sp-g2 spira/sp-g2 "$REPO" "test: no bypass" 2>&1)"
rc=$?
is "gate: destroy returns 1 for unlanded branch"   1  "$rc"
is "gate: unlanded branch still exists"            0  "$(branch_exists spira/sp-g2; echo $?)"
want "gate: REFUSED in reaplog" "REFUSED" "$(cat "$SPIRA_REAPLOG" 2>/dev/null)"

git -C "$REPO" branch -D spira/sp-g2 >/dev/null 2>&1 || true

# ======================================================================================
# GATE PATH — no bypass on a LANDED branch must be allowed.
#
# The gate must not block legitimate deletion of landed work. This is the positive
# case for the gate itself: content_landed says yes, the gate approves.
# ======================================================================================
echo
echo "gate path — no bypass on landed branch must be allowed:"

seed sp-g3 open
make_branch sp-g3
land sp-g3

is "gate: landed branch exists before destroy" 0 \
   "$(branch_exists spira/sp-g3; echo $?)"

spira_destroy_branch sp-g3 spira/sp-g3 "$REPO" "test: landed, no bypass" >/dev/null 2>&1
rc=$?
is "gate: destroy exits 0 for landed branch"  0  "$rc"
is "gate: landed branch gone"                 1  "$(branch_exists spira/sp-g3; echo $?)"

# ======================================================================================
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
