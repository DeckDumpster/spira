#!/usr/bin/env bash
#
# test-sending-certified-guard.sh — a CERTIFIED branch survives the reaping pass even when
#   content_landed returns true; a non-CERTIFIED branch with landed content is deleted.
#
# THE PROPERTY UNDER TEST (sp-e5ow0).  Between 03:15Z and 04:54Z on 2026-09-17 three
# branches were CERTIFIED in the merge queue (landstate) but were deleted while still queued.
# sending.sh's "sending" caller bypass skips the content fence in spira_destroy_branch, so a
# branch whose batch has just merged passes content_landed=true and is destroyed before
# verdict.sh has written LANDED.  The fix adds a CERTIFIED/BATCHED guard unconditionally
# before any caller bypass: no path through spira_destroy_branch may delete a queued branch.
#
# TWO BRANCHES, TWO OUTCOMES:
#
#   sp-cert: CERTIFIED landstate, content IS on the base (batch already fast-forwarded).
#            spira_destroy_branch must REFUSE; the branch must survive the sending pass.
#
#   sp-norm: LANDED landstate, content IS on the base (normal case).
#            spira_destroy_branch must allow it; the branch must be deleted (positive control).
#
# SEEN RED WITHOUT THE GUARD. Removing the CERTIFIED/BATCHED guard from spira_destroy_branch
# causes sp-cert to be deleted: content_landed returns true, the "sending" caller bypasses the
# content fence, and the deletion succeeds.  This suite catches that regression.
#
# defect: sp-e5ow0
# covers: spira/lib.sh spira/sending.sh
# scar: spira_destroy_branch had no guard against deleting CERTIFIED/BATCHED branches; sending.sh deleted queued branches when content_landed returned true after a batch merged, before verdict.sh could write LANDED.
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
testdb_require test-sending-certified-guard
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up sendcertguard || { echo "test-sending-certified-guard: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" remote set-head origin main
mkdir -p "$RUN/worktree" "$RUN/landstate"
export SPIRA_RUN="$RUN" SPIRA_REAPLOG="$RUN/reap.log" SPIRA_CONF="$TMP/no-such-conf"

# shellcheck disable=SC1090
. "$HERE/lib.sh"

HOME_REPO="$(basename "$REPO")"
printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$HOME_REPO" "$REPO" push main '' '' > "$TMP/repo-map"

# ─── Fixture branches ────────────────────────────────────────────────────────
# Both branches have content already on origin/main (squash-merged), so
# content_landed returns true for both.  Only the landstate differs.

# sp-cert: CERTIFIED landstate.  Content is on the base (simulates the moment
# after a batch fast-forwarded but before verdict.sh wrote LANDED).
git -C "$REPO" checkout -q -b spira/sp-cert main
printf 'cert-content\n' > "$REPO/sp-cert.txt"
git -C "$REPO" add sp-cert.txt && git -C "$REPO" commit -q -m "sp-cert: work"
git -C "$REPO" checkout -q main
git -C "$REPO" merge -q --squash spira/sp-cert >/dev/null 2>&1
git -C "$REPO" commit -q -m "squash-land sp-cert"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

printf 'CERTIFIED %s %s\n' \
    "$(git -C "$REPO" rev-parse spira/sp-cert)" "$(date +%s)" > "$RUN/landstate/sp-cert"

# sp-norm: LANDED landstate.  Same squash shape but state is LANDED — this is
# what the normal batch landing looks like after verdict.sh runs.
git -C "$REPO" checkout -q -b spira/sp-norm main
printf 'norm-content\n' > "$REPO/sp-norm.txt"
git -C "$REPO" add sp-norm.txt && git -C "$REPO" commit -q -m "sp-norm: work"
git -C "$REPO" checkout -q main
git -C "$REPO" merge -q --squash spira/sp-norm >/dev/null 2>&1
git -C "$REPO" commit -q -m "squash-land sp-norm"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

printf 'LANDED %s %s\n' \
    "$(git -C "$REPO" rev-parse spira/sp-norm)" "$(date +%s)" > "$RUN/landstate/sp-norm"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-cert","title":"certified branch","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-17T00:00:00Z"}
{"id":"sp-norm","title":"normal landed branch","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-17T00:00:00Z"}
JSONL

branch_exists() { git -C "$REPO" show-ref --verify -q "refs/heads/$1" 2>/dev/null; }

sending() {
    SPIRA_HOME="$HERE" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$HOME_REPO" \
    SPIRA_REPO_MAP="$TMP/repo-map" \
        bash "$HERE/sending.sh" --no-fetch 2>&1
}

echo "test-sending-certified-guard.sh"

# ─── Fixture confirmation ─────────────────────────────────────────────────────
echo
echo "fixture confirmation — content_landed must be true for both branches:"

if content_landed "$REPO" spira/sp-cert origin/main; then
    ok "fixture: content_landed is true for sp-cert (batch already merged this content)"
else
    bad "fixture: content_landed must return true for sp-cert — fixture is wrong, test proves nothing" \
        "content_landed returned non-zero"
fi

if content_landed "$REPO" spira/sp-norm origin/main; then
    ok "fixture: content_landed is true for sp-norm"
else
    bad "fixture: content_landed must return true for sp-norm — fixture is wrong" \
        "content_landed returned non-zero"
fi

# =============================================================================
# POSITIVE CONTROL — sp-norm (LANDED state) is deleted by the sending pass.
#
# Proves the code path through send_branch and spira_destroy_branch is reached.
# Without this, a version that refuses every deletion would pass the absence check.
# =============================================================================
echo
echo "positive control — sp-norm (LANDED) is deleted:"

is "sp-norm exists before sending" 0 "$(branch_exists spira/sp-norm; echo $?)"

out="$(sending)"
rc=$?
is "sending exits 0" 0 "$rc"
is "sp-norm is deleted after sending" 1 "$(branch_exists spira/sp-norm; echo $?)"
want "sending reports SENT for sp-norm" "SENT sp-norm" "$out"

# =============================================================================
# CERTIFIED GUARD — sp-cert (CERTIFIED state) must NOT be deleted.
#
# content_landed is true, so the selector in sending.sh would otherwise hand it
# to send_branch; the "sending" caller bypass would skip the content fence; the
# branch would be destroyed.  The CERTIFIED/BATCHED guard in spira_destroy_branch
# fires before any bypass and must refuse.
# =============================================================================
echo
echo "certified guard — sp-cert (CERTIFIED) is kept despite content being on the base:"

is "sp-cert still exists after sending" 0 "$(branch_exists spira/sp-cert; echo $?)"
want "reap log records REFUSED for sp-cert" "REFUSED" "$(cat "$SPIRA_REAPLOG" 2>/dev/null)"
nowant "sending does not report SENT for sp-cert" "SENT sp-cert" "$out"

printf '\n%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
