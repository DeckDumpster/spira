#!/usr/bin/env bash
#
# test-sending-unlanded-guard.sh — sending.sh skips unlanded branches; only landed ones are
#   deleted. The "reclaim path" guard: no unlanded branch reaches spira_destroy_branch via
#   sending.sh.
#
#   ./test-sending-unlanded-guard.sh
#
# THE PROPERTY UNDER TEST (sp-nxij / sp-mqsl). Before the content fence was added,
# spira_destroy_branch deleted branches unconditionally once the holder-witness checks
# passed. The Sending was the caller on both the landing path (legitimate) and any
# reclaim path that reached it (not). The fix placed two guards:
#
#   1. SELECTOR IN SENDING.SH (this suite). content_landed gates the for-each-ref loop;
#      a branch whose commits are not on the base gets a KEEP log line and is never
#      handed to send_branch. The fence in spira_destroy_branch never sees it.
#
#   2. FENCE IN SPIRA_DESTROY_BRANCH (test-destroy-branch.sh, test-reclaim-slay-branch-guard.sh).
#      Any caller that reaches the function without the bypass arg and without having
#      landed the work is refused there.
#
# This suite covers guard (1): sending.sh's selector. A future change that removes the
# content_landed predicate from sending.sh would not be caught by the fence tests alone,
# because those tests never invoke sending.sh — they call spira_destroy_branch directly.
#
# TWO BRANCHES, TWO OUTCOMES:
#
#   sp-ul: an UNLANDED branch — a real new file not on origin/main, representing an aeon
#          that committed but whose work never merged. sending.sh must KEEP it; the
#          branch must survive the sending pass.
#
#   sp-la: a LANDED branch — a real new file whose diff was squash-merged to origin/main.
#          content_landed returns true (ancestry alone would refuse the squash case, which
#          is why content_landed exists). sending.sh must SEND it; the branch must be gone.
#
# A POSITIVE CONTROL precedes the absence assertion: the landed branch IS deleted, which
# proves the code path reaches send_branch and spira_destroy_branch. Without it, a
# version of sending.sh that skipped every branch would pass the "unlanded branch still
# exists" check (law-absence-needs-a-positive-control).
#
# FIXTURE CONFIRMATION: content_landed is verified on both branches before running
# sending.sh, so a mis-built fixture (unlanded branch that content_landed sees as landed)
# is caught before the assertions, not silently reported as a sending.sh failure.
#
# A REAL bd ON A THROWAWAY DATABASE (law-prefer-the-real-dependency): sending.sh reads
# the bead's database status via spira_holder_witnesses; a stub would drift.
#
# defect: sp-mqsl
# covers: spira/sending.sh spira/lib.sh
# hermetic-ok: fixture database and local git repos, no systemd, no network
# scar: sending.sh passed unlanded branches to spira_destroy_branch once holder-witness checks passed; work that had not reached origin/main was silently deleted.
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
testdb_require test-sending-unlanded-guard
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up sendunlandedguard || { echo "test-sending-unlanded-guard: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; REMOTE="$TMP/remote.git"
RUN="$TMP/run"
export SPIRA_RUN="$RUN"; mkdir -p "$RUN/worktree"
export SPIRA_REAPLOG="$RUN/reap.log"
export SPIRA_CONF="$TMP/no-such-conf"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" remote set-head origin main

# shellcheck disable=SC1090
. "$HERE/lib.sh"

HOME_REPO="$(basename "$REPO")"
printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$HOME_REPO" "$REPO" push main '' '' > "$TMP/repo-map"

# ---- fixture branches ---------------------------------------------------------------

# sp-ul: unlanded — a new file not on origin/main.
git -C "$REPO" checkout -q -b spira/sp-ul main
printf 'unlanded-content\n' > "$REPO/sp-ul.txt"
git -C "$REPO" add sp-ul.txt
git -C "$REPO" commit -q -m "sp-ul: work that never merged"
git -C "$REPO" checkout -q main

# sp-la: landed — squash-merge the branch's diff onto main, then push.
git -C "$REPO" checkout -q -b spira/sp-la main
printf 'landed-content\n' > "$REPO/sp-la.txt"
git -C "$REPO" add sp-la.txt
git -C "$REPO" commit -q -m "sp-la: work"
git -C "$REPO" checkout -q main
git -C "$REPO" merge -q --squash spira/sp-la >/dev/null 2>&1
git -C "$REPO" commit -q -m "squash-land sp-la"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

# ---- seed the database --------------------------------------------------------------

testdb_reset
testdb_seed <<JSONL
{"id":"sp-ul","title":"unlanded work","status":"open","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-09T00:00:00Z"}
{"id":"sp-la","title":"landed work","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-09T00:00:00Z"}
JSONL

branch_exists() { git -C "$REPO" show-ref --verify -q "refs/heads/$1" 2>/dev/null; }

sending() {
    SPIRA_HOME="$HERE" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$HOME_REPO" \
    SPIRA_REPO_MAP="$TMP/repo-map" \
        bash "$HERE/sending.sh" --no-fetch 2>&1
}

echo "test-sending-unlanded-guard.sh"

# ---- fixture confirmation -----------------------------------------------------------
# Verify content_landed sees each branch correctly BEFORE running sending.sh.
# A wrong fixture (e.g. unlanded branch that content_landed sees as landed) would make
# the sending.sh assertions meaningless.
echo
echo "fixture confirmation:"

if content_landed "$REPO" spira/sp-ul origin/main; then
    bad "fixture: sp-ul should NOT be seen as landed by content_landed" \
        "content_landed returned 0 — fixture is wrong, test proves nothing"
else
    ok "fixture: content_landed correctly sees sp-ul as unlanded"
fi

if content_landed "$REPO" spira/sp-la origin/main; then
    ok "fixture: content_landed correctly sees sp-la as landed"
else
    bad "fixture: sp-la should be seen as landed by content_landed" \
        "squash merge did not produce a landed state — fixture is wrong"
fi

# Also confirm ancestry alone would refuse the squash-landed branch, so we know
# content_landed is doing the real work (not ancestry).
if git -C "$REPO" merge-base --is-ancestor spira/sp-la origin/main 2>/dev/null; then
    bad "fixture: squash branch should NOT be an ancestor of origin/main" \
        "branch IS an ancestor — squash fixture is wrong; ancestry alone would allow this"
else
    ok "fixture: sp-la is NOT an ancestor of origin/main (squash case confirmed)"
fi

# ======================================================================================
# POSITIVE CONTROL — landed branch is deleted by sending.sh.
#
# Proves the code path through send_branch and spira_destroy_branch is reached. Without
# this, an implementation that never reaches send_branch would pass the "unlanded branch
# still exists" assertion below.
# ======================================================================================
echo
echo "positive control — landed branch is deleted:"

is "sp-la branch exists before sending" 0 "$(branch_exists spira/sp-la; echo $?)"

out="$(sending)"
rc=$?
is "sending exits 0" 0 "$rc"
is "sp-la branch is gone after sending" 1 "$(branch_exists spira/sp-la; echo $?)"
want "sending reports SENT for sp-la" "SENT sp-la" "$out"

# ======================================================================================
# UNLANDED GUARD — unlanded branch is NOT deleted by sending.sh.
#
# This is the property the bead exists to assert: when an aeon commits work but the
# work has not yet merged, sending.sh must leave the branch alone. content_landed returns
# false; the selector emits KEEP and continues without calling send_branch.
# ======================================================================================
echo
echo "unlanded guard — unlanded branch survives sending:"

is "sp-ul branch still exists after sending" 0 "$(branch_exists spira/sp-ul; echo $?)"
want "sending reports KEEP for sp-ul" "KEEP   sp-ul" "$out"
nowant "sending does NOT report SENT for sp-ul" "SENT sp-ul" "$out"

# ======================================================================================
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
